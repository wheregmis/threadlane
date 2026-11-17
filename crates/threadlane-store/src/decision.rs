//! [`DecisionStore`] on the hub database (Plan.md §9.4).

use async_trait::async_trait;
use serde_json::json;
use threadlane_core::ids::{DecisionId, LaneId};
use threadlane_core::model::{
    Answered, Decision, DecisionAnswer, DecisionStatus, EventKind, Inserted, NewDecision, NewEvent,
    Trust,
};
use threadlane_core::store::{DecisionStore, StoreError, StoreResult};
use turso::{Connection, Row};

use crate::HubDb;
use crate::db::{exec, query_all, query_one, query_opt};
use crate::inbox::insert_event_in;
use crate::row::{Cols, json_text, opt_raw, opt_ts, params};

/// Source name of `decision_answer` events.
pub const DECISION_SOURCE: &str = "decision";

const DECISION_FIELDS: &str = "id, project_id, lane_id, task_id, session_id, run_id,
    tool_call_id, tool, arguments, arguments_hash, kind, question, context, options,
    recommended, offers_standing_rule, status, answer, expires_at, created_at";

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

#[async_trait]
impl DecisionStore for HubDb {
    async fn open_decision(&self, decision: &NewDecision) -> StoreResult<Decision> {
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
        let created = query_one(
            tx.conn(),
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
        tx.commit().await?;
        Ok(created)
    }

    async fn decision(&self, id: DecisionId) -> StoreResult<Option<Decision>> {
        let conn = self.db.reader()?;
        decision_in(&conn, id).await
    }

    async fn pending_decisions(&self, lane_id: LaneId) -> StoreResult<Vec<Decision>> {
        let conn = self.db.reader()?;
        query_all(
            &conn,
            &format!(
                "SELECT {DECISION_FIELDS} FROM decisions WHERE lane_id = ?1 AND status = 'pending'
                 ORDER BY id"
            ),
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
        if answer.option as usize >= decision.options.len() {
            return Err(StoreError::Invalid(format!(
                "option {} is out of range for decision {id}",
                answer.option
            )));
        }
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
        let inserted = insert_event_in(
            conn,
            &NewEvent {
                project_id: decision.project_id,
                lane_id: decision.lane_id,
                source: DECISION_SOURCE.to_owned(),
                source_event_id: format!("answer:{}", id.get()),
                kind: EventKind::DecisionAnswer,
                payload: json!({
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
        )
        .await?;
        tx.commit().await?;
        match inserted {
            Inserted::New(event) | Inserted::Duplicate(event) => Ok(Answered::Recorded(event)),
        }
    }
}
