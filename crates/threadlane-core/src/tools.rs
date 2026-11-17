//! The system's tools and the permission gate (Plan.md §2.3, §4.6, §9).

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::ids::{AgentId, ProjectId, RunId, TaskId};
use crate::model::{Lease, Mode};
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

/// A tool call presented to the gate (Plan.md §9.1).
#[derive(Debug, Clone, PartialEq)]
pub struct GateRequest {
    pub project_id: ProjectId,
    pub agent_id: Option<AgentId>,
    pub tool: String,
    pub tier: Tier,
    pub arguments: Json,
    /// The lower of the binding's mode and the project ceiling.
    pub binding_mode: Mode,
    /// Whether the run was triggered by external content.
    pub tainted: bool,
}

/// The gate's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Block {
        reason: String,
    },
    /// Pause the call as a decision.
    Ask,
}

/// Tool call in; allow, block, or ask out.
#[async_trait]
pub trait PermissionPolicy: Send + Sync + fmt::Debug {
    async fn decide(&self, request: &GateRequest) -> Verdict;
}
