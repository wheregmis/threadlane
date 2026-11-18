//! The space lead: the General project's lead, which sees the space's other
//! projects and hands goals to their leads (docs/one-place-to-talk.md
//! stage 1). It reads no other project's memory (invariant 12): the summary
//! comes from the projects, tasks and sent messages only.

use std::fmt::Write as _;

use serde_json::{Value as Json, json};
use threadlane_core::ids::{EventId, ProjectId, TaskId};
use threadlane_core::model::{Event, EventKind, Inserted, NewEvent, Run, Trust};
use threadlane_core::runtime::{WakeTarget, Waker};
use threadlane_core::store::{InboxStore, OrgStore, OutboxStore, SessionStore, TaskStore};
use threadlane_core::time::Timestamp;
use threadlane_core::tools::{Tier, ToolDefinition, ToolError};
use threadlane_store::HubDb;

use crate::error::tool_error;
use crate::tools::fields;

/// The General lead hands a goal to another project's lead.
pub const ASK_PROJECT_LEAD: &str = "ask_project_lead";
/// Source of the event `ask_project_lead` inserts in a project's lead lane.
pub const SPACE_LEAD_SOURCE: &str = "space_lead";
/// Source of the event that brings a project lead's reply to the General lane.
pub const PROJECT_LEAD_SOURCE: &str = "project_lead";

/// The longest a lead's latest message is quoted in the summary.
const MAX_QUOTE: usize = 400;

/// The heading of the summary, which also tells the space lead what to do.
const HEADING: &str = "You are the space lead. The space's other projects, as they stand. \
For a goal that belongs to one of them, hand it to its lead with ask_project_lead; its lead's \
reply comes back to you as an event, and you report to the user. Nothing here comes from \
another space.";

pub(crate) fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ASK_PROJECT_LEAD.to_owned(),
        description: "Hand a goal or a question to the lead of another project in this space. \
                      Its lead works on it as it would on a message from the user, and its \
                      reply comes back to you as an event."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "project": { "type": "string", "description": "The project's name." },
                "message": {
                    "type": "string",
                    "description": "What you ask of its lead, in plain language."
                }
            },
            "required": ["project", "message"],
            "additionalProperties": false
        }),
        tier: Tier::Local,
        // The event is keyed on the call, so repeating it inserts nothing.
        safe_to_repeat: true,
    }
}

/// The space summary appended to the General lead's instructions: each other
/// project with its state, open tasks and its lead's latest sent message.
/// Byte-identical while none of these change.
pub(crate) async fn summary(db: &HubDb) -> Result<String, threadlane_core::store::StoreError> {
    let mut out = HEADING.to_owned();
    for project in db.projects().await? {
        if project.is_general {
            continue;
        }
        let _ = write!(out, "\n\n- {}", project.name);
        if !project.description.trim().is_empty() {
            let _ = write!(out, ": {}", one_line(&project.description));
        }
        if project.archived_at.is_some() {
            out.push_str(" (archived; cannot be asked)");
        } else if project.paused {
            out.push_str(" (paused)");
        }
        let tasks = db.tasks(project.id).await?;
        let open: Vec<_> = tasks
            .iter()
            .filter(|task| !task.status.is_terminal())
            .collect();
        if open.is_empty() {
            out.push_str("\n  No open tasks.");
        } else {
            out.push_str("\n  Open tasks:");
            for task in open {
                let _ = write!(
                    out,
                    "\n  - {} {} ({})",
                    task.id,
                    one_line(&task.title),
                    task.status
                );
            }
        }
        if let Some(message) = db.recent_messages(project.id, 1).await?.first() {
            let text = message.body["text"].as_str().unwrap_or_default();
            let _ = write!(out, "\n  Its lead's latest message: {}", quote(text));
        }
    }
    Ok(out)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn quote(text: &str) -> String {
    let text = one_line(text);
    match text.char_indices().nth(MAX_QUOTE) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
}

/// Whether the run works on something you asked for.
pub(crate) fn requested(events: &[Event]) -> bool {
    events.iter().any(|event| event.requested)
}

/// `ask_project_lead`: inserts the ask in the named project's lead lane,
/// keyed on the call. Only the General lead may call it; an unknown,
/// archived or General project is refused.
pub(crate) async fn ask(
    db: &HubDb,
    waker: &dyn Waker,
    now: Timestamp,
    asking_project: ProjectId,
    run: &Run,
    key: String,
    arguments: &Json,
) -> Result<Json, ToolError> {
    let object = fields(arguments, &["project", "message"])?;
    let text = |name: &str| {
        object
            .get(name)
            .and_then(Json::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be non-empty text")))
    };
    let (name, message) = (text("project")?, text("message")?);
    let general = db.general_project().await.map_err(tool_error)?;
    if general.id != asking_project {
        return Err(ToolError::Blocked(format!(
            "{ASK_PROJECT_LEAD} is only for the lead of the General project"
        )));
    }
    let projects = db.projects().await.map_err(tool_error)?;
    let open: Vec<&str> = projects
        .iter()
        .filter(|project| !project.is_general && project.archived_at.is_none())
        .map(|project| project.name.as_str())
        .collect();
    let target = projects
        .iter()
        .find(|project| project.name == name && !project.is_general)
        .ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "no project {name:?} in this space; it has {}",
                if open.is_empty() {
                    "none".to_owned()
                } else {
                    open.join(", ")
                }
            ))
        })?;
    if target.archived_at.is_some() {
        return Err(ToolError::InvalidArguments(format!(
            "project {name:?} is archived; it cannot be asked"
        )));
    }
    let events = db.run_events(run.id).await.map_err(tool_error)?;
    let inserted = db
        .insert_event(&NewEvent {
            project_id: target.id,
            lane_id: target.lead_lane_id,
            source: SPACE_LEAD_SOURCE.to_owned(),
            source_event_id: key,
            kind: EventKind::UserMessage,
            payload: json!({
                "text": format!(
                    "The space lead, the General project's lead, asks you:\n{message}\n\n\
                     Your reply with send_message reaches the user and the space lead."
                ),
                "message": message,
            }),
            // Written by an agent, not the user: requested only when the
            // asking run was.
            trust: Trust::System,
            requested: requested(&events),
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: now,
        })
        .await
        .map_err(tool_error)?;
    if let Inserted::New(_) = inserted {
        waker.wake(WakeTarget::Lane(target.lead_lane_id));
    }
    Ok(json!({
        "project": target.name,
        "event_id": inserted.id(),
        "next": "Its lead has the message. Its reply comes back to you as an event; end your turn until then.",
    }))
}

/// The space-lead ask a project lead's run answers, if it answers one: an
/// ask it consumes, or the result of a task created for one.
pub(crate) async fn answered_ask(db: &HubDb, events: &[Event]) -> Result<Option<Event>, ToolError> {
    if let Some(ask) = events
        .iter()
        .find(|event| event.source == SPACE_LEAD_SOURCE)
    {
        return Ok(Some(ask.clone()));
    }
    for event in events
        .iter()
        .filter(|event| event.kind == EventKind::TaskResult)
    {
        let Ok(task) = serde_json::from_value::<TaskId>(event.payload["task_id"].clone()) else {
            continue;
        };
        if let Some(id) = db.space_lead_ask(task).await.map_err(tool_error)? {
            return Ok(db.event(id).await.map_err(tool_error)?);
        }
    }
    Ok(None)
}

/// Marks a task a project lead created for a space-lead ask, so the reply
/// to its result reaches the space lead too.
pub(crate) async fn mark_task(db: &HubDb, task: TaskId, ask: EventId) -> Result<(), ToolError> {
    db.set_space_lead_ask(task, ask).await.map_err(tool_error)
}

/// Delivers a project lead's message, sent in reply to `ask`, to the
/// General lane as an event, once per message.
pub(crate) async fn route_reply(
    db: &HubDb,
    waker: &dyn Waker,
    now: Timestamp,
    ask: &Event,
    project: ProjectId,
    outbox_id: impl std::fmt::Display,
    text: &str,
) -> Result<(), ToolError> {
    let general = db.general_project().await.map_err(tool_error)?;
    let name = db
        .project(project)
        .await
        .map_err(tool_error)?
        .map_or_else(|| project.to_string(), |project| project.name);
    let inserted = db
        .insert_event(&NewEvent {
            project_id: general.id,
            lane_id: general.lead_lane_id,
            source: PROJECT_LEAD_SOURCE.to_owned(),
            source_event_id: format!("reply:{outbox_id}"),
            kind: EventKind::System,
            payload: json!({
                "text": format!(
                    "The lead of {name} replied to your ask (event {}):\n{text}",
                    ask.id
                ),
                "project": name,
                "ask_event_id": ask.id,
            }),
            trust: Trust::System,
            requested: ask.requested,
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: now,
        })
        .await
        .map_err(tool_error)?;
    if let Inserted::New(_) = inserted {
        waker.wake(WakeTarget::Lane(general.lead_lane_id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_are_one_line_and_bounded() {
        assert_eq!(quote("a\n  b"), "a b");
        let long = "x".repeat(MAX_QUOTE + 10);
        assert_eq!(quote(&long).chars().count(), MAX_QUOTE + 1);
    }
}
