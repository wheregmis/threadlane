//! User-interaction contracts shared by runtime, session, and UI surfaces.
//!
//! Canonical home for the permission and clarifying-question wire shapes
//! previously defined in `threadlane_runtime::events`. They are protocol
//! types — serialized on session JSONL, mirrored by the daemon transport
//! (`crate::daemon`), and rendered by GPUI — not execution logic, so they
//! live here. `threadlane-runtime` re-exports them for compatibility; new
//! code should import from `threadlane_protocol::interaction` directly.

use serde::{Deserialize, Serialize};

/// A permission prompt published to the user (e.g. network host, computer use).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub capability: String,
    pub title: String,
    pub detail: String,
    pub scopes: Vec<PermissionScope>,
}

/// Grant scopes offered on a [`PermissionRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    Once,
    Always,
}

/// One clarifying question posed to the user through the `ask_question` tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionItem {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub allow_custom: bool,
}

/// A model-initiated request for user answers (issue #40: Ask Questions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionRequest {
    pub id: String,
    pub questions: Vec<QuestionItem>,
}

/// The user's answer to a single [`QuestionItem`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionItemAnswer {
    pub question_id: String,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_text: Option<String>,
}

/// Resolution of a [`QuestionRequest`]. `dismissed` is true when no answer UI
/// was available and the request was released without user input, so the turn
/// can never block forever on an unanswered question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub request_id: String,
    #[serde(default)]
    pub answers: Vec<QuestionItemAnswer>,
    #[serde(default)]
    pub dismissed: bool,
}

impl QuestionAnswer {
    pub fn dismissed(request_id: &str) -> Self {
        Self {
            request_id: request_id.to_owned(),
            answers: Vec::new(),
            dismissed: true,
        }
    }
}
