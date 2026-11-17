//! [`DecisionStore`] on the hub database (Plan.md §9.4).

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::ids::{DecisionId, LaneId};
use threadlane_core::model::{
    Answered, Decision, DecisionAnswer, DecisionStatus, EventKind, Inserted, Lease, NewDecision,
    NewEvent, NewOutboxMessage, OutboxKind, ToolCallBegin, Trust, Urgency,
};
use threadlane_core::store::{DecisionStore, StoreError, StoreResult};
use threadlane_core::time::Timestamp;
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::inbox::{check_lease, insert_event_in};
use crate::outbox::enqueue_in;
use crate::row::{Cols, json_text, opt_raw, opt_ts, params};
use crate::session::NOTICE_CHANNEL;

/// Source name of `decision_answer` events.
pub const DECISION_SOURCE: &str = "decision";

const DECISION_FIELDS: &str = "id, project_id, lane_id, task_id, session_id, run_id,
    tool_call_id, tool, arguments, arguments_hash, kind, question, context, options,
    recommended, offers_standing_rule, status, answer, expires_at, created_at, outcome";

fn decode_decision(row: &Row) -> StoreResult<Decision> {
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
    })
}

/// Records an answer to a pending decision and inserts its event into the
/// decision's lane, inside the caller's transaction.
async fn answer_in(
    conn: &Connection,
    decision: &Decision,
    answer: &DecisionAnswer,
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
    exec(
        conn,
        "UPDATE decisions SET status = ?1, answer = ?2, answered_at = ?3 WHERE id = ?4",
        params![
            DecisionStatus::Answered.as_str(),
            json_text(answer)?,
            answer.answered_at.as_millis(),
            id.get(),
        ],
    )
    .await?;
    let mut text = if option.approves() {
        format!(
            "You approved {id} ({}). The system performs the call before this run starts; \
             its outcome follows.",
            decision.tool
        )
    } else {
        format!(
            "You declined {id} ({}): {:?}. It was not performed.",
            decision.tool, option.label
        )
    };
    if let Some(note) = answer
        .note
        .as_deref()
        .filter(|note| !note.trim().is_empty())
    {
        text.push_str(&format!(" Note from the user: {note}"));
    }
    Box::pin(insert_event_in(
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
                "option": answer.option,
                "note": answer.note,
                "standing_rule": answer.standing_rule,
            }),
            trust: Trust::User,
            requested: true,
            session_ref: decision.session_id,
            decision_ref: Some(id),
            reply_to_ref: None,
            received_at: answer.answered_at,
        },
    ))
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

#[async_trait]
impl DecisionStore for HubDb {
    async fn open_decision(
        &self,
        lease: Option<&Lease>,
        decision: &NewDecision,
    ) -> StoreResult<Decision> {
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
        let tx = self.db.begin().await?;
        let conn = tx.conn();
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
            return Ok(existing);
        }
        let created = query_one(
            conn,
            &format!(
                "INSERT INTO decisions (project_id, lane_id, task_id, session_id, run_id,
                     tool_call_id, tool, arguments, arguments_hash, kind, question, context,
                     options, recommended, offers_standing_rule, status, expires_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     'pending', ?16, ?17)
                 RETURNING {DECISION_FIELDS}"
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
            ],
            decode_decision,
        )
        .await?;
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
                body: card(&created),
                urgency: Urgency::High,
                not_before: None,
                created_at: created.created_at,
            },
        )
        .await?;
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
        let tx = self.db.begin().await?;
        let conn = tx.conn();
        let decision = decision_in(conn, id)
            .await?
            .ok_or_else(|| StoreError::not_found("decision", id))?;
        if decision.status != DecisionStatus::Pending {
            return Ok(Answered::Ignored);
        }
        let inserted = answer_in(conn, &decision, answer).await?;
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
}
