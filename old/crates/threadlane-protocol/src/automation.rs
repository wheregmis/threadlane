//! Automation wire contract: the durable recurring-task store remote
//! clients drive through `SessionCommand::AutomationRequest` and observe
//! through `SessionEvent::AutomationChanged`.
//!
//! The store types themselves are canonical in `threadlane-automation` —
//! re-exported here so a client names the same `Definition`/`Run` it would
//! build on desktop, with no wire-side mirror to drift.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::interaction::{PermissionRequest, QuestionRequest};

pub use threadlane_automation::{
    display_time, new_id, now, Definition, Run, RunStatus, Schedule, Snapshot,
};

/// One automation-store operation a client issues inside
/// `SessionCommand::AutomationRequest`. Every command answers
/// `CommandResponse::Automation` carrying the post-command projection —
/// reads and mutations share one reply shape so a client re-renders from a
/// single payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum AutomationCommand {
    /// Current definitions, runs, and pending requests.
    GetSnapshot,
    /// Create or replace a definition (validated + persisted by the store).
    Save { definition: Definition },
    /// Toggle whether a definition dispatches on its schedule.
    SetEnabled { id: String, enabled: bool },
    /// Remove a definition and its recorded runs.
    Delete { id: String },
    /// Remove one run record (a deleted active run is cancelled).
    DeleteRun { id: String },
    /// Queue a manual run of the definition now.
    RunNow { id: String },
    /// Cancel the active or queued run with this run id.
    Cancel { id: String },
    /// Clear a finished run's unreviewed marker.
    Review { id: String },
}

/// The automation store's observable state as it crosses the wire. Mirrors
/// the daemon-side service projection except `active_runtime` — an
/// in-process handle remote clients reach as `active_session_id` (the live
/// run's chat session, opened with the usual session commands).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationProjection {
    pub snapshot: Snapshot,
    /// Permission requests raised by the live run, keyed by session id —
    /// answered with the normal `AnswerPermission` command.
    pub permissions: HashMap<String, PermissionRequest>,
    /// Latest question request raised by the live run, keyed by session id.
    /// Retained for older clients; `question_queues` is the complete surface.
    pub questions: HashMap<String, QuestionRequest>,
    /// Complete pending questions per session, in request order. `Some`
    /// is authoritative even when empty; `None` denotes an older daemon
    /// that reports only the latest request through `questions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_queues: Option<HashMap<String, Vec<QuestionRequest>>>,
    /// Session id of the run currently executing, when one is.
    pub active_session_id: Option<String>,
    /// Store-level failure (e.g. the service could not open its state). The
    /// snapshot may still carry last-known definitions; mutations fail
    /// while this is set.
    #[serde(default)]
    pub error: Option<String>,
}

/// Payload of `CommandResponse::Automation`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationResponse {
    /// The store's post-command projection.
    Projection { projection: AutomationProjection },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wire decoding distinguishes an omitted legacy field from an authoritative empty queue.
    #[test]
    fn automation_question_queues_distinguish_legacy_from_authoritative_empty() {
        let legacy = serde_json::to_value(AutomationProjection::default()).unwrap();
        assert!(legacy.get("question_queues").is_none());
        let legacy: AutomationProjection = serde_json::from_value(legacy).unwrap();
        assert!(legacy.question_queues.is_none());

        for queues in [
            HashMap::new(),
            HashMap::from([(
                "session".into(),
                vec![QuestionRequest {
                    id: "question".into(),
                    questions: Vec::new(),
                }],
            )]),
        ] {
            let projection = AutomationProjection {
                question_queues: Some(queues),
                ..Default::default()
            };
            let encoded = serde_json::to_value(&projection).unwrap();
            assert!(encoded.get("question_queues").is_some());
            assert_eq!(
                serde_json::from_value::<AutomationProjection>(encoded).unwrap(),
                projection
            );
        }
    }
}
