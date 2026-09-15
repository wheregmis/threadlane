//! Agent lifecycle events, canonical in `threadlane-protocol::events`.
//!
//! Re-exported here so existing `crate::events::…` and
//! `threadlane_runtime::…` paths keep working; new code should import
//! `threadlane_protocol` directly.

pub use threadlane_protocol::events::{
    AgentEvent, HarnessMetrics, SubagentIsolation, SubagentProgressUpdate, SubagentRecoveryStatus,
};
// Interaction contracts (permission prompts, clarifying questions) are
// canonical in `threadlane_protocol::interaction`; re-exported here so
// existing `threadlane_runtime::…` paths keep working.
pub use threadlane_protocol::interaction::{
    PermissionRequest, PermissionScope, QuestionAnswer, QuestionItem, QuestionItemAnswer,
    QuestionRequest,
};
