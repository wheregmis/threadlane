//! The system's tools and the permission gate (Plan.md §2.3, §4.6, §9).

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::ids::{AgentId, ProjectId, RunId, StandingRuleId, TaskId};
use crate::model::Lease;
use crate::runtime::Killed;

/// How much an action can affect others (Plan.md §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Tier 0: read.
    Read,
    /// Tier 1: local and reversible.
    Local,
    /// Tier 2: shared and reversible.
    SharedReversible,
    /// Tier 3: shared and hard to undo.
    SharedHardToUndo,
    /// Never allowed.
    Forbidden,
}

impl Tier {
    /// The number a standing rule stores its tier as: 0 to 3, and 4 for
    /// forbidden.
    #[must_use]
    pub const fn number(self) -> i64 {
        match self {
            Self::Read => 0,
            Self::Local => 1,
            Self::SharedReversible => 2,
            Self::SharedHardToUndo => 3,
            Self::Forbidden => 4,
        }
    }

    /// The tier stored as `number`, if it is one.
    #[must_use]
    pub const fn from_number(number: i64) -> Option<Self> {
        Some(match number {
            0 => Self::Read,
            1 => Self::Local,
            2 => Self::SharedReversible,
            3 => Self::SharedHardToUndo,
            4 => Self::Forbidden,
            _ => return None,
        })
    }
}

/// A tool as the agent sees it, plus what the system needs to know about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Json,
    pub tier: Tier,
    /// Whether a call that started but never finished may simply be repeated.
    pub safe_to_repeat: bool,
}

/// Why a tool call failed; returned to the agent as a tool error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    #[error("unknown tool {0}")]
    UnknownTool(String),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("blocked: {0}")]
    Blocked(String),
    /// A decision was opened; the agent must end its turn (Plan.md §9.4).
    #[error("waiting on you, end your turn")]
    WaitingOnYou,
    /// The call was authorized and will be performed when the lane resumes.
    #[error("already authorized and queued. End your turn to receive the outcome")]
    AuthorizedAndQueued,
    /// Started before and not safe to repeat (Plan.md §4.6).
    #[error("outcome unknown, verify before retrying")]
    OutcomeUnknown,
    #[error("the hub is unreachable")]
    HubUnreachable,
    /// The run's lease is gone. The runner must stop the run.
    #[error("this run no longer holds its lane")]
    LeaseLost,
    #[error("tool failed: {0}")]
    Failed(String),
    /// Crash injection stopped the hub mid-call. Only an in-process hub
    /// produces this; treat it as process death and propagate it.
    #[error(transparent)]
    Killed(#[from] Killed),
}

/// Where a call comes from. Every call carries the run's lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolContext {
    pub lease: Lease,
    pub run_id: RunId,
    pub project_id: ProjectId,
    pub agent_id: Option<AgentId>,
    pub task_id: Option<TaskId>,
    /// The backend's call id, if it supplies one.
    pub call_id: Option<String>,
}

/// What a running backend calls to reach the system's tools. The tool server
/// implements it; a scripted backend calls it directly.
#[async_trait]
pub trait ToolEndpoint: Send + Sync + fmt::Debug {
    /// The tools this run may use.
    fn definitions(&self) -> Vec<ToolDefinition>;
    /// Calls one tool.
    async fn call(
        &self,
        call_id: Option<&str>,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError>;
}

/// The system's tools: definition, tier, whether safe to repeat, executor.
#[async_trait]
pub trait ToolRegistry: Send + Sync + fmt::Debug {
    fn definitions(&self) -> Vec<ToolDefinition>;
    fn definition(&self, name: &str) -> Option<ToolDefinition>;
    /// Executes a call that the gate and the at-most-once record have allowed.
    async fn execute(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError>;
}

/// The gate's answer to one tool call (Plan.md §9.1, docs/milestone-9.md
/// decision 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Proceed. `rule` is the standing rule that allowed a call the matrix
    /// would have asked about: such a call is recorded as a decision the
    /// rule answered, and performed as an approved one is.
    Allow { rule: Option<StandingRuleId> },
    /// Proceed by the built-in default: recorded and performed as a
    /// rule-allowed call is, with the decision marked as allowed by default.
    AllowByDefault,
    /// Classifier review. An unavailable or malformed answer opens a decision
    /// with the same `offers_rule` semantics as [`Verdict::Ask`].
    Review { offers_rule: bool },
    /// Pause the call as a decision. `offers_rule` says whether answering it
    /// "always" could create a standing rule that would apply here.
    Ask { offers_rule: bool },
    /// Refuse; the reason goes back to the agent as a tool error.
    Block { reason: String },
}
