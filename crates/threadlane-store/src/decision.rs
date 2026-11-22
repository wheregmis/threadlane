//! [`DecisionStore`] on the hub database (Plan.md §9.4).

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::ids::{DecisionId, LaneId, ProjectId, StandingRuleId};
use threadlane_core::model::{
    Answered, Decision, DecisionAnswer, DecisionKind, DecisionStatus, EventKind, Inserted, Lease,
    NewDecision, NewEvent, NewOutboxMessage, NewStandingRule, NewTimer, OutboxKind, StandingRule,
    TimerCategory, TimerCreator, ToolCallBegin, Trust, Urgency,
};
use threadlane_core::store::{DecisionStore, StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use threadlane_core::tools::Tier;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::inbox::{check_lease, insert_event_in};
use crate::outbox::enqueue_in;
use crate::row::{Cols, json_text, opt_raw, opt_ts, params};
use crate::session::NOTICE_CHANNEL;
use crate::timer::insert_timer_in;

/// Source name of `decision_answer` events.
pub const DECISION_SOURCE: &str = "decision";

/// The note of a decision that expired (docs/milestone-9.md decision 8).
pub const NO_RESPONSE: &str = "no response";

pub(crate) const DECISION_FIELDS: &str = "id, project_id, lane_id, task_id, session_id, run_id,
    tool_call_id, tool, arguments, arguments_hash, kind, question, context, options,
    recommended, offers_standing_rule, status, answer, expires_at, created_at, outcome, trace_id, urgent";

pub(crate) fn decode_decision(row: &Row) -> StoreResult<Decision> {
    let c = Cols(row);
    Ok(Decision {
        id: c.id(0)?,
        project_id: c.id(1)?,
        lane_id: c.id(2)?,
        task_id: c.opt_id(3)?,
        session_id: c.opt_id(4)?,
        run_id: c.opt_id(5)?,
        tool_call_id: c.opt_text(6)?,
        tool: c.text(7)?,
        arguments: c.json(8)?,
        arguments_hash: c.text(9)?,
        kind: c.parse(10)?,
        question: c.text(11)?,
        context: c.text(12)?,
        options: c.de(13)?,
        recommended: c.opt_u32(14)?,
        offers_standing_rule: c.bool(15)?,
        status: c.parse(16)?,
        answer: c.opt_json(17)?,
        expires_at: c.opt_ts(18)?,
        created_at: c.ts(19)?,
        outcome: c.opt_json(20)?,
        proposed_by: None,
        trace_id: c.opt_text(21)?,
        urgent: c.bool(22)?,
    })
}

async fn decision_in(conn: &Connection, id: DecisionId) -> StoreResult<Option<Decision>> {
    query_opt(
        conn,
        &format!("SELECT {DECISION_FIELDS} FROM decisions WHERE id = ?1"),
        params![id.get()],
        decode_decision,
    )
    .await
}

/// The card that shows a decision in the client's feed (Plan.md §10.1).
fn card(decision: &Decision) -> Json {
    let labels: Vec<&str> = decision
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    let recommended = decision
        .recommended
        .and_then(|index| decision.options.get(index as usize))
        .map(|option| option.label.as_str());
    let advice = recommended.map_or_else(String::new, |label| format!(" (recommended: {label})"));
    json!({
        "text": format!(
            "{}: {} Answer with `threadlane answer {} <{}>`{advice}.",
            decision.id,
            decision.question,
            decision.id,
            labels.join("|"),
        ),
        "decision": decision.id.to_string(),
        "question": decision.question,
        "context": decision.context,
        "options": labels,
        "recommended": recommended,
        "kind": decision.kind,
        "urgent": decision.urgent,
    })
}

/// The `created_by_call` of the system timer that expires decision `id`.
fn expiry_call(id: DecisionId) -> String {
    format!("decision:{}", id.get())
}

/// The decision a system timer's `created_by_call` expires, if it is one.
pub(crate) fn expiring_decision(call: &str) -> Option<DecisionId> {
    call.strip_prefix("decision:")?.parse().ok().map(DecisionId)
}

/// Closes a pending decision with `status` and `answer`, cancels its expiry
/// timer, and inserts the event that resumes its lane, inside the caller's
/// transaction. `text` is what the agent is told.
async fn close_in(
    conn: &Connection,
    decision: &Decision,
    status: DecisionStatus,
    answer: &Json,
    text: String,
    trust: Trust,
    at: Timestamp,
) -> StoreResult<Inserted> {
    let id = decision.id;
    if decision.tool == "workflow_review" && answer["option"].as_u64() != Some(0) {
        exec(
            conn,
            "UPDATE tasks SET result=NULL WHERE id=?1",
            params![opt_raw(decision.task_id, |v| v.get())],
        )
        .await?;
    }
    exec(
        conn,
        "UPDATE decisions SET status = ?1, answer = ?2, answered_at = ?3 WHERE id = ?4",
        params![
            status.as_str(),
            json_text(answer)?,
            at.as_millis(),
            id.get()
        ],
    )
    .await?;
    exec(
        conn,
        "UPDATE timers SET status = 'cancelled' WHERE created_by_call = ?1 AND status = 'active'",
        params![expiry_call(id).as_str()],
    )
    .await?;
    let from_dream = match decision.run_id {
        Some(run) => crate::dreaming::dream_run_in(conn, run).await?,
        None => false,
    };
    let inserted = Box::pin(insert_event_in(
        conn,
        &NewEvent {
            project_id: decision.project_id,
            lane_id: decision.lane_id,
            source: DECISION_SOURCE.to_owned(),
            source_event_id: format!("answer:{}", id.get()),
            kind: EventKind::DecisionAnswer,
            payload: json!({
                "text": text,
                "decision_id": id,
                "option": answer.get("option"),
                "note": answer.get("note"),
                "standing_rule": answer.get("standing_rule").cloned().unwrap_or(Json::Bool(false)),
            }),
            trust,
            // It continues the run that asked.
            requested: true,
            session_ref: if from_dream {
                None
            } else {
                decision.session_id
            },
            decision_ref: Some(id),
            reply_to_ref: None,
            received_at: at,
        },
    ))
    .await?;
    if from_dream && decision.tool != threadlane_core::forge::REPORT_LIMITATION {
        exec(
            conn,
            "UPDATE events SET status = 'skipped', status_reason = 'dream proposal closed'
             WHERE id = ?1 AND status = 'pending'",
            params![inserted.id().get()],
        )
        .await?;
    }
    Ok(inserted)
}

/// Records an answer to a pending decision and inserts its event into the
/// decision's lane, inside the caller's transaction. `rule` is the standing
/// rule that answered it, if no person did.
async fn answer_in(
    conn: &Connection,
    decision: &Decision,
    answer: &DecisionAnswer,
    rule: Option<StandingRuleId>,
) -> StoreResult<Inserted> {
    let id = decision.id;
    let option = decision
        .options
        .get(answer.option as usize)
        .ok_or_else(|| {
            StoreError::Invalid(format!(
                "option {} is out of range for decision {id}",
                answer.option
            ))
        })?;
    let mut text = match rule {
        Some(rule) => format!(
            "Standing rule {rule} allowed {id} ({}). The system performs the call before this \
             run starts; its outcome follows.",
            decision.tool
        ),
        None if decision.kind == DecisionKind::Input => format!(
            "You answered {id} ({:?}): {:?}.",
            decision.question, option.label
        ),
        None if option.approves() => format!(
            "You approved {id} ({}). The system performs the call before this run starts; \
             its outcome follows.",
            decision.tool
        ),
        None => format!(
            "You declined {id} ({}): {:?}. It was not performed.",
            decision.tool, option.label
        ),
    };
    if let Some(note) = answer
        .note
        .as_deref()
        .filter(|note| !note.trim().is_empty())
    {
        text.push_str(&format!(" Note from the user: {note}"));
    }
    close_in(
        conn,
        decision,
        DecisionStatus::Answered,
        &serde_json::to_value(answer).map_err(|error| StoreError::Invalid(error.to_string()))?,
        text,
        Trust::User,
        answer.answered_at,
    )
    .await
}

/// Expires decision `id` as declined with "no response", once: a decision
/// no longer pending keeps what it has, and its answer event is returned.
pub(crate) async fn expire_in(
    conn: &Connection,
    id: DecisionId,
    now: Timestamp,
) -> StoreResult<Inserted> {
    let decision = decision_in(conn, id)
        .await?
        .ok_or_else(|| StoreError::not_found("decision", id))?;
    if decision.status != DecisionStatus::Pending {
        return query_opt(
            conn,
            "SELECT id FROM events WHERE source = ?1 AND source_event_id = ?2",
            params![DECISION_SOURCE, format!("answer:{}", id.get())],
            |row| Cols(row).id(0),
        )
        .await?
        .map(Inserted::Duplicate)
        .ok_or_else(|| StoreError::Conflict(format!("decision {id} is {}", decision.status)));
    }
    let decline = decision
        .options
        .iter()
        .position(|option| !option.approves())
        .filter(|_| decision.kind != DecisionKind::Input);
    let (answer, text) = match decline {
        Some(option) => (
            json!({
                "option": option,
                "note": NO_RESPONSE,
                "standing_rule": false,
                "answered_at": now,
            }),
            format!(
                "{id} ({}) expired with no response, so it is declined. It was not performed.",
                decision.tool
            ),
        ),
        None => (
            json!({ "note": NO_RESPONSE, "answered_at": now }),
            format!(
                "{id} ({:?}) expired with no response: the user did not answer.",
                decision.question
            ),
        ),
    };
    close_in(
        conn,
        &decision,
        DecisionStatus::Expired,
        &answer,
        text,
        Trust::System,
        now,
    )
    .await
}

async fn pending_in(conn: &Connection, lane_id: LaneId) -> StoreResult<Vec<Decision>> {
    query_all(
        conn,
        &format!(
            "SELECT {DECISION_FIELDS} FROM decisions WHERE lane_id = ?1 AND status = 'pending'
             ORDER BY id"
        ),
        params![lane_id.get()],
        decode_decision,
    )
    .await
}

/// A message from you in a task lane declines every decision pending there,
/// with the message as the note (Plan.md §9.4 "while a decision is open").
pub(crate) async fn decline_pending_in(
    conn: &Connection,
    lane_id: LaneId,
    note: &str,
    now: Timestamp,
) -> StoreResult<()> {
    for decision in pending_in(conn, lane_id).await? {
        // A question has no decline: the message is the answer.
        if decision.kind == DecisionKind::Input {
            close_in(
                conn,
                &decision,
                DecisionStatus::Answered,
                &json!({ "note": note, "answered_at": now }),
                format!(
                    "You replied to {} ({:?}) instead of choosing an option: {note}",
                    decision.id, decision.question
                ),
                Trust::User,
                now,
            )
            .await?;
            continue;
        }
        let Some(option) = decision
            .options
            .iter()
            .position(|option| !option.approves())
        else {
            continue;
        };
        answer_in(
            conn,
            &decision,
            &DecisionAnswer {
                option: u32::try_from(option).unwrap_or(u32::MAX),
                note: Some(note.to_owned()),
                standing_rule: false,
                answered_at: now,
            },
            None,
        )
        .await?;
    }
    Ok(())
}

/// The decision, if `lease` is the current lease of its lane.
async fn fenced(conn: &Connection, lease: &Lease, id: DecisionId) -> StoreResult<Decision> {
    check_lease(conn, lease).await?;
    decision_in(conn, id)
        .await?
        .filter(|decision| decision.lane_id == lease.lane_id)
        .ok_or_else(|| StoreError::not_found("decision", id))
}

const RULE_FIELDS: &str = "id, project_id, agent_id, tool, pattern, tier, created_from_decision, created_at, revoked_at, uses";

fn decode_rule(row: &Row) -> StoreResult<StandingRule> {
    let c = Cols(row);
    let tier = c.i64(5)?;
    Ok(StandingRule {
        id: c.id(0)?,
        project_id: c.id(1)?,
        agent_id: c.id(2)?,
        tool: c.text(3)?,
        pattern: c.json(4)?,
        tier: Tier::from_number(tier)
            .ok_or_else(|| StoreError::Invalid(format!("unknown tier {tier}")))?,
        created_from_decision: c.opt_id(6)?,
        created_at: c.ts(7)?,
        revoked_at: c.opt_ts(8)?,
        uses: c.u32(9)?,
    })
}

/// Inserts a decision inside the caller's transaction, under `lease` if a
/// run asks. A repeated `(run_id, tool_call_id)` returns the original and
/// `false`.
pub(crate) async fn insert_decision_in(
    conn: &Connection,
    lease: Option<&Lease>,
    decision: &NewDecision,
) -> StoreResult<(Decision, bool)> {
    if decision.options.is_empty() {
        return Err(StoreError::Invalid(
            "a decision needs at least one option".into(),
        ));
    }
    if let Some(recommended) = decision.recommended
        && recommended as usize >= decision.options.len()
    {
        return Err(StoreError::Invalid(format!(
            "recommended option {recommended} is out of range"
        )));
    }
    if let Some(lease) = lease {
        check_lease(conn, lease).await?;
        if lease.lane_id != decision.lane_id {
            return Err(StoreError::Invalid(format!(
                "a run in lane {} cannot ask in lane {}",
                lease.lane_id, decision.lane_id
            )));
        }
    }
    if let (Some(run_id), Some(call)) = (decision.run_id, decision.tool_call_id.as_deref())
        && let Some(existing) = query_opt(
            conn,
            &format!(
                "SELECT {DECISION_FIELDS} FROM decisions WHERE run_id = ?1 AND tool_call_id = ?2"
            ),
            params![run_id.get(), call],
            decode_decision,
        )
        .await?
    {
        return Ok((existing, false));
    }
    crate::org::require_active_project(conn, decision.project_id).await?;
    if let Some(run) = decision.run_id
        && decision
            .options
            .iter()
            .any(|option| option.effect["dream_proposal"] == true)
        && let Some(previous) = query_opt(
            conn,
            &format!(
                "SELECT {DECISION_FIELDS} FROM decisions WHERE arguments_hash = ?2
                AND run_id IN (
                    SELECT prior.run_id FROM run_events prior JOIN run_events current
                        ON prior.event_id = current.event_id WHERE current.run_id = ?1
                ) ORDER BY id LIMIT 1"
            ),
            params![run.get(), decision.arguments_hash.as_str()],
            decode_decision,
        )
        .await?
    {
        return Ok((previous, false));
    }
    let created = query_one(
        conn,
        &format!(
            "INSERT INTO decisions (id, project_id, lane_id, task_id, session_id, run_id,
                 tool_call_id, tool, arguments, arguments_hash, kind, question, context,
                 options, recommended, offers_standing_rule, status, expires_at, created_at, trace_id, urgent)
             VALUES ({}, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                 'pending', ?16, ?17, (SELECT trace_id FROM runs WHERE id = ?5), ?18)
             RETURNING {DECISION_FIELDS}", crate::db::next_id_sql!("decisions")
        ),
        params![
            decision.project_id.get(),
            decision.lane_id.get(),
            opt_raw(decision.task_id, |id| id.get()),
            opt_raw(decision.session_id, |id| id.get()),
            opt_raw(decision.run_id, |id| id.get()),
            decision.tool_call_id.as_deref(),
            decision.tool.as_str(),
            json_text(&decision.arguments)?,
            decision.arguments_hash.as_str(),
            decision.kind.as_str(),
            decision.question.as_str(),
            decision.context.as_str(),
            json_text(&decision.options)?,
            decision.recommended.map(i64::from),
            decision.offers_standing_rule,
            opt_ts(decision.expires_at),
            decision.created_at.as_millis(),
            decision.urgent,
        ],
        decode_decision,
    )
    .await?;
    Ok((created, true))
}

pub(crate) async fn enqueue_card(conn: &Connection, created: &Decision) -> StoreResult<()> {
    // Decisions bypass pacing (Plan.md §10.2); the key makes the card
    // one per decision.
    enqueue_in(
        conn,
        &NewOutboxMessage {
            project_id: Some(created.project_id),
            lane_id: Some(created.lane_id),
            session_id: created.session_id,
            run_id: created.run_id,
            tool_call_id: Some(format!("decision:{}", created.id.get())),
            kind: OutboxKind::Decision,
            decision_id: Some(created.id),
            channel: NOTICE_CHANNEL.to_owned(),
            body: card(created),
            urgency: Urgency::High,
            not_before: None,
            created_at: created.created_at,
        },
    )
    .await?;
    if let Some(due_at) = created.expires_at {
        insert_timer_in(
            conn,
            &NewTimer {
                project_id: created.project_id,
                lane_id: created.lane_id,
                agent_id: None,
                task_ref: created.task_id,
                category: TimerCategory::FollowUp,
                due_at,
                recurrence: None,
                instruction: format!("expire {}", created.id),
                created_by: TimerCreator::System,
                created_by_call: Some(expiry_call(created.id)),
                requested: false,
            },
            None,
        )
        .await?;
    }
    Ok(())
}

impl HubDb {
    /// Claims a question once. A retry freezes and receives the last recorded answer,
    /// including the conservative default if the first ask was interrupted.
    pub async fn claim_question(
        &self,
        lease: &Lease,
        key: &str,
        fallback: &Json,
    ) -> StoreResult<Option<Json>> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        let inserted = exec(
            conn,
            "INSERT INTO permission_questions(question_key, answer, pending)
            VALUES (?1, ?2, 1) ON CONFLICT(question_key) DO NOTHING",
            params![key, json_text(fallback)?],
        )
        .await?;
        let result = if inserted == 1 {
            None
        } else {
            exec(
                conn,
                "UPDATE permission_questions SET pending=0 WHERE question_key=?1 AND pending=1",
                params![key],
            )
            .await?;
            Some(
                query_one(
                    conn,
                    "SELECT answer FROM permission_questions WHERE question_key=?1",
                    params![key],
                    |r| Cols(r).json(0),
                )
                .await?,
            )
        };
        tx.commit().await?;
        Ok(result)
    }

    /// Records an answer under the current lease, returning the frozen answer on a race.
    pub async fn finish_question(
        &self,
        lease: &Lease,
        key: &str,
        answer: &Json,
    ) -> StoreResult<Json> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        exec(conn, "UPDATE permission_questions SET answer=?2,pending=0 WHERE question_key=?1 AND pending=1",params![key,json_text(answer)?]).await?;
        let result = query_one(
            conn,
            "SELECT answer FROM permission_questions WHERE question_key=?1",
            params![key],
            |r| Cols(r).json(0),
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Whether this run consumed any external event without a recorded clean screen.
    pub async fn run_tainted(&self, run: threadlane_core::ids::RunId) -> StoreResult<bool> {
        query_one(&self.db.reader()?, "SELECT EXISTS(SELECT 1 FROM run_events re JOIN events e ON e.id=re.event_id
          LEFT JOIN permission_questions q ON q.question_key='event:' || e.id
          WHERE re.run_id=?1 AND e.trust='external' AND COALESCE(json_extract(q.answer,'$.verdict'),'tainted') != 'clean')",
          params![run.get()], |r| Cols(r).bool(0)).await
    }

    /// Stores the reviewed call, answering it automatically or opening its fallback card.
    /// A replay of the same question also reuses its decision across run attempts.
    pub async fn reviewed_decision(
        &self,
        lease: &Lease,
        key: &str,
        new: &NewDecision,
        reason: Option<&str>,
        rule: Option<StandingRuleId>,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        check_lease(conn, lease).await?;
        let existing = query_one(
            conn,
            "SELECT decision_id FROM permission_questions WHERE question_key=?1",
            params![key],
            |r| Cols(r).opt_id::<DecisionId>(0),
        )
        .await?;
        if existing.is_none() {
            let (decision, inserted) = insert_decision_in(conn, Some(lease), new).await?;
            if inserted {
                if let Some(reason) = reason {
                    // A rule that had it reviewed is used once the classifier
                    // allows it, as a rule that allows a call is.
                    if let Some(rule) = rule {
                        let used = exec(
                            conn,
                            "UPDATE standing_rules SET uses = uses + 1 WHERE id = ?1 AND revoked_at IS NULL",
                            params![rule.get()],
                        )
                        .await?;
                        if used == 0 {
                            return Err(StoreError::Conflict(format!(
                                "standing rule {rule} is revoked or does not exist"
                            )));
                        }
                    }
                    close_in(conn, &decision, DecisionStatus::Answered,
                        &json!({"option":0,"classifier":"allow","reason":reason,"standing_rule":false,"answered_at":new.created_at}),
                        format!("The classifier allowed {} ({}). Its outcome follows.", decision.id, new.tool),
                        Trust::System, new.created_at).await?;
                } else {
                    enqueue_card(conn, &decision).await?;
                }
            }
            exec(
                conn,
                "UPDATE permission_questions SET decision_id=?2 WHERE question_key=?1",
                params![key, decision.id.get()],
            )
            .await?;
        }
        tx.commit().await
    }

    /// Grants one dispatch of a repository effect even after lease recovery.
    pub async fn claim_forge_dispatch(&self, lease: &Lease, id: DecisionId) -> StoreResult<bool> {
        let tx = self.db.begin().await?;
        check_lease(tx.conn(), lease).await?;
        let changed = exec(tx.conn(), "UPDATE decisions SET forge_dispatched=1 WHERE id=?1 AND lane_id=?2 AND forge_dispatched=0",params![id.get(),lease.lane_id.get()]).await?;
        tx.commit().await?;
        Ok(changed == 1)
    }
    /// Decisions with a recorded outcome in non-cancelled task lanes, ordered by `id`.
    pub async fn decisions_with_outcome(&self) -> StoreResult<Vec<Decision>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {DECISION_FIELDS} FROM decisions
                 WHERE outcome IS NOT NULL
                   AND lane_id IN (SELECT lane_id FROM tasks WHERE status != 'cancelled')
                 ORDER BY id"
            ),
            (),
            decode_decision,
        )
        .await
    }
}

#[async_trait]
impl DecisionStore for HubDb {
    async fn open_decision(
        &self,
        lease: Option<&Lease>,
        decision: &NewDecision,
    ) -> StoreResult<Decision> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let (created, new) = insert_decision_in(conn, lease, decision).await?;
        if !new {
            return Ok(created);
        }
        enqueue_card(conn, &created).await?;
        tx.commit().await?;
        Ok(created)
    }

    async fn decision(&self, id: DecisionId) -> StoreResult<Option<Decision>> {
        let conn = self.db.reader()?;
        decision_in(&conn, id).await
    }

    async fn pending_decisions(&self, lane_id: LaneId) -> StoreResult<Vec<Decision>> {
        let conn = self.db.reader()?;
        pending_in(&conn, lane_id).await
    }

    async fn open_decisions(&self) -> StoreResult<Vec<Decision>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {DECISION_FIELDS} FROM decisions WHERE status = 'pending' ORDER BY id"
            ),
            (),
            decode_decision,
        )
        .await
    }

    async fn lane_decisions(&self, lane_id: LaneId) -> StoreResult<Vec<Decision>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!("SELECT {DECISION_FIELDS} FROM decisions WHERE lane_id = ?1 ORDER BY id"),
            params![lane_id.get()],
            decode_decision,
        )
        .await
    }

    async fn answer(&self, id: DecisionId, answer: &DecisionAnswer) -> StoreResult<Answered> {
        self.answer_with_rule(id, answer, None).await
    }

    async fn answer_with_rule(
        &self,
        id: DecisionId,
        answer: &DecisionAnswer,
        rule: Option<&NewStandingRule>,
    ) -> StoreResult<Answered> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let decision = decision_in(conn, id)
            .await?
            .ok_or_else(|| StoreError::not_found("decision", id))?;
        if decision.status != DecisionStatus::Pending {
            return Ok(Answered::Ignored);
        }
        let inserted = answer_in(conn, &decision, answer, None).await?;
        if let Some(rule) = rule {
            exec(
                conn,
                concat!(
                    "INSERT INTO standing_rules (id, project_id, agent_id, tool, pattern, tier,
                     created_from_decision, created_at)
                 VALUES (",
                    crate::db::next_id_sql!("standing_rules"),
                    ", ?1, ?2, ?3, ?4, ?5, ?6, ?7)"
                ),
                params![
                    rule.project_id.get(),
                    rule.agent_id.get(),
                    rule.tool.as_str(),
                    json_text(&rule.pattern)?,
                    rule.tier.number(),
                    id.get(),
                    rule.created_at.as_millis(),
                ],
            )
            .await?;
        }
        tx.commit().await?;
        match inserted {
            Inserted::New(event) | Inserted::Duplicate(event) => Ok(Answered::Recorded(event)),
        }
    }

    async fn begin_perform(
        &self,
        lease: &Lease,
        id: DecisionId,
        now: Timestamp,
    ) -> StoreResult<ToolCallBegin> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let decision = fenced(conn, lease, id).await?;
        if !decision.approved() {
            return Err(StoreError::Conflict(format!(
                "decision {id} was not answered with approval"
            )));
        }
        if let Some(outcome) = decision.outcome {
            return Ok(ToolCallBegin::Finished(outcome));
        }
        let changed = exec(
            conn,
            "UPDATE decisions SET perform_started_at = ?1
             WHERE id = ?2 AND perform_started_at IS NULL",
            params![now.as_millis(), id.get()],
        )
        .await?;
        tx.commit().await?;
        Ok(if changed == 1 {
            ToolCallBegin::Perform
        } else {
            ToolCallBegin::OutcomeUnknown
        })
    }

    async fn finish_perform(
        &self,
        lease: &Lease,
        id: DecisionId,
        outcome: &Json,
    ) -> StoreResult<()> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        fenced(conn, lease, id).await?;
        exec(
            conn,
            "UPDATE decisions SET outcome = ?1 WHERE id = ?2 AND outcome IS NULL",
            params![json_text(outcome)?, id.get()],
        )
        .await?;
        tx.commit().await
    }

    async fn allow_by_rule(
        &self,
        lease: &Lease,
        decision: &NewDecision,
        rule: StandingRuleId,
    ) -> StoreResult<Decision> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let (created, new) = insert_decision_in(conn, Some(lease), decision).await?;
        if !new {
            return Ok(created);
        }
        let used = exec(
            conn,
            "UPDATE standing_rules SET uses = uses + 1 WHERE id = ?1 AND revoked_at IS NULL",
            params![rule.get()],
        )
        .await?;
        if used == 0 {
            return Err(StoreError::Conflict(format!(
                "standing rule {rule} is revoked or does not exist"
            )));
        }
        answer_in(
            conn,
            &created,
            &DecisionAnswer {
                option: 0,
                note: None,
                standing_rule: false,
                answered_at: created.created_at,
            },
            Some(rule),
        )
        .await?;
        let answered = decision_in(conn, created.id)
            .await?
            .ok_or_else(|| StoreError::not_found("decision", created.id))?;
        tx.commit().await?;
        Ok(answered)
    }

    async fn allow_by_default(
        &self,
        lease: &Lease,
        decision: &NewDecision,
    ) -> StoreResult<Decision> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let (created, new) = insert_decision_in(conn, Some(lease), decision).await?;
        if !new {
            return Ok(created);
        }
        close_in(
            conn,
            &created,
            DecisionStatus::Answered,
            &json!({
                "option": 0,
                "note": null,
                "standing_rule": false,
                "by_default": true,
                "answered_at": created.created_at,
            }),
            format!(
                "{} ({}) is allowed by default. The system performs the call before this run \
                 starts; its outcome follows.",
                created.id, created.tool
            ),
            // Not the user's answer: it must not make the next run count as requested.
            Trust::System,
            created.created_at,
        )
        .await?;
        let answered = decision_in(conn, created.id)
            .await?
            .ok_or_else(|| StoreError::not_found("decision", created.id))?;
        tx.commit().await?;
        Ok(answered)
    }

    async fn standing_rules(
        &self,
        project_id: Option<ProjectId>,
    ) -> StoreResult<Vec<StandingRule>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {RULE_FIELDS} FROM standing_rules
                 WHERE ?1 IS NULL OR project_id = ?1 ORDER BY id"
            ),
            params![opt_raw(project_id, |id| id.get())],
            decode_rule,
        )
        .await
    }

    async fn revoke_standing_rule(
        &self,
        id: StandingRuleId,
        now: Timestamp,
    ) -> StoreResult<StandingRule> {
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        exec(
            conn,
            "UPDATE standing_rules SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
            params![now.as_millis(), id.get()],
        )
        .await?;
        let rule = query_opt(
            conn,
            &format!("SELECT {RULE_FIELDS} FROM standing_rules WHERE id = ?1"),
            params![id.get()],
            decode_rule,
        )
        .await?
        .ok_or_else(|| StoreError::not_found("standing rule", id))?;
        tx.commit().await?;
        Ok(rule)
    }

    async fn claim_reminder(&self, id: DecisionId, now: Timestamp) -> StoreResult<bool> {
        let tx = self.db.begin().await?;
        let claimed = exec(
            tx.conn(),
            "UPDATE decisions SET reminded_at = ?1 WHERE id = ?2 AND reminded_at IS NULL AND status = 'pending'",
            params![now.as_millis(), id.get()],
        )
        .await?;
        tx.commit().await?;
        Ok(claimed == 1)
    }
}
