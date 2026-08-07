use crate::op_log::SteerPriority;
use crate::types::{AgentMessage, TokenUsage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub parent_id: Option<String>,
    #[serde(default = "default_main_lane")]
    pub lane: String,
    pub seq: u64,
    pub timestamp: u64,
    pub message: AgentMessage,
    #[serde(default)]
    pub terminate: bool,
}

fn default_main_lane() -> String {
    "main".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub message: AgentMessage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedEntry {
    pub id: String,
    pub run_id: Option<String>,
    pub queue: QueueKind,
    #[serde(default)]
    pub priority: Option<SteerPriority>,
    pub target: ProvisionedEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub index: usize,
    pub call_id: String,
    pub name: String,
    pub effective_args: Value,
    pub result_entry_id: String,
    pub replay: ToolReplaySafety,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
    pub terminate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationIntent {
    Run,
    Compaction,
    Navigation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueKind {
    Steer,
    FollowUp,
    NextRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolReplaySafety {
    Never,
    Safe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationOutcome {
    Completed,
    Aborted,
    Failed,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsageCause {
    Provider,
    Discarded,
    Tool,
    Replay,
    Compaction,
    Adjustment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryState {
    pub attempt: u32,
    pub retry_at: u64,
    pub reason: String,
}

impl Default for UsageCause {
    fn default() -> Self {
        Self::Provider
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Record {
    OperationStarted {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        source_leaf_id: Option<String>,
        intent: OperationIntent,
    },
    AbortRequested {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
    },
    OperationFinished {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        outcome: OperationOutcome,
        error: Option<String>,
    },
    LaneMoved {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        target_leaf_id: String,
    },
    StepAttempt {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        result_entry_id: String,
        compaction_reason: Option<String>,
    },
    RetryScheduled {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        retry_at: u64,
        reason: String,
    },
    RetryConsumed {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
    },
    ToolStarted {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        assistant_entry_id: String,
        tool_index: usize,
        tool_call_id: String,
        tool_name: String,
        effective_args: Value,
        result_entry_id: String,
        replay: ToolReplaySafety,
    },
    ToolFinished {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        tool_call_id: String,
        result_entry_id: String,
        terminate: bool,
    },
    QueueEnqueued {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        queue: QueueKind,
        #[serde(default)]
        priority: Option<SteerPriority>,
        target: ProvisionedEntry,
    },
    QueueCancelled {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        entry_id: String,
    },
    QueueConsumed {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        entry_id: String,
    },
    WriteDeferred {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        target: ProvisionedEntry,
    },
    WriteApplied {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        entry_id: String,
    },
    FactSet {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        key: String,
        value: String,
    },
    HookResumeData {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        hook_id: String,
        data: String,
    },
    Usage {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        #[serde(default)]
        cause: UsageCause,
        #[serde(default)]
        entry_id: Option<String>,
        #[serde(default)]
        tool_call_id: Option<String>,
        #[serde(default)]
        attempt: Option<u32>,
        usage: TokenUsage,
    },
}

impl Record {
    pub fn with_seq(self, seq: u64) -> Self {
        match self {
            Self::OperationStarted {
                id,
                lane,
                timestamp,
                source_leaf_id,
                intent,
                ..
            } => Self::OperationStarted {
                id,
                seq,
                lane,
                timestamp,
                source_leaf_id,
                intent,
            },
            Self::AbortRequested {
                id,
                lane,
                timestamp,
                run_id,
                ..
            } => Self::AbortRequested {
                id,
                seq,
                lane,
                timestamp,
                run_id,
            },
            Self::OperationFinished {
                id,
                lane,
                timestamp,
                run_id,
                outcome,
                error,
                ..
            } => Self::OperationFinished {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                outcome,
                error,
            },
            Self::LaneMoved {
                id,
                lane,
                timestamp,
                run_id,
                target_leaf_id,
                ..
            } => Self::LaneMoved {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                target_leaf_id,
            },
            Self::StepAttempt {
                id,
                lane,
                timestamp,
                run_id,
                attempt,
                result_entry_id,
                compaction_reason,
                ..
            } => Self::StepAttempt {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                attempt,
                result_entry_id,
                compaction_reason,
            },
            Self::RetryScheduled {
                id,
                lane,
                timestamp,
                run_id,
                attempt,
                retry_at,
                reason,
                ..
            } => Self::RetryScheduled {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                attempt,
                retry_at,
                reason,
            },
            Self::RetryConsumed {
                id,
                lane,
                timestamp,
                run_id,
                attempt,
                ..
            } => Self::RetryConsumed {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                attempt,
            },
            Self::ToolStarted {
                id,
                lane,
                timestamp,
                run_id,
                assistant_entry_id,
                tool_index,
                tool_call_id,
                tool_name,
                effective_args,
                result_entry_id,
                replay,
                ..
            } => Self::ToolStarted {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                assistant_entry_id,
                tool_index,
                tool_call_id,
                tool_name,
                effective_args,
                result_entry_id,
                replay,
            },
            Self::ToolFinished {
                id,
                lane,
                timestamp,
                run_id,
                tool_call_id,
                result_entry_id,
                terminate,
                ..
            } => Self::ToolFinished {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                tool_call_id,
                result_entry_id,
                terminate,
            },
            Self::QueueEnqueued {
                id,
                lane,
                timestamp,
                run_id,
                queue,
                priority,
                target,
                ..
            } => Self::QueueEnqueued {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                queue,
                priority,
                target,
            },
            Self::QueueCancelled {
                id,
                lane,
                timestamp,
                run_id,
                entry_id,
                ..
            } => Self::QueueCancelled {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                entry_id,
            },
            Self::QueueConsumed {
                id,
                lane,
                timestamp,
                run_id,
                entry_id,
                ..
            } => Self::QueueConsumed {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                entry_id,
            },
            Self::WriteDeferred {
                id,
                lane,
                timestamp,
                run_id,
                target,
                ..
            } => Self::WriteDeferred {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                target,
            },
            Self::WriteApplied {
                id,
                lane,
                timestamp,
                run_id,
                entry_id,
                ..
            } => Self::WriteApplied {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                entry_id,
            },
            Self::FactSet {
                id,
                lane,
                timestamp,
                run_id,
                key,
                value,
                ..
            } => Self::FactSet {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                key,
                value,
            },
            Self::HookResumeData {
                id,
                lane,
                timestamp,
                run_id,
                hook_id,
                data,
                ..
            } => Self::HookResumeData {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                hook_id,
                data,
            },
            Self::Usage {
                id,
                lane,
                timestamp,
                run_id,
                cause,
                entry_id,
                tool_call_id,
                attempt,
                usage,
                ..
            } => Self::Usage {
                id,
                seq,
                lane,
                timestamp,
                run_id,
                cause,
                entry_id,
                tool_call_id,
                attempt,
                usage,
            },
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::OperationStarted { id, .. }
            | Self::AbortRequested { id, .. }
            | Self::OperationFinished { id, .. }
            | Self::LaneMoved { id, .. }
            | Self::StepAttempt { id, .. }
            | Self::RetryScheduled { id, .. }
            | Self::RetryConsumed { id, .. }
            | Self::ToolStarted { id, .. }
            | Self::ToolFinished { id, .. }
            | Self::QueueEnqueued { id, .. }
            | Self::QueueCancelled { id, .. }
            | Self::QueueConsumed { id, .. }
            | Self::WriteDeferred { id, .. }
            | Self::WriteApplied { id, .. }
            | Self::FactSet { id, .. }
            | Self::HookResumeData { id, .. }
            | Self::Usage { id, .. } => id,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            Self::OperationStarted { seq, .. }
            | Self::AbortRequested { seq, .. }
            | Self::OperationFinished { seq, .. }
            | Self::LaneMoved { seq, .. }
            | Self::StepAttempt { seq, .. }
            | Self::RetryScheduled { seq, .. }
            | Self::RetryConsumed { seq, .. }
            | Self::ToolStarted { seq, .. }
            | Self::ToolFinished { seq, .. }
            | Self::QueueEnqueued { seq, .. }
            | Self::QueueCancelled { seq, .. }
            | Self::QueueConsumed { seq, .. }
            | Self::WriteDeferred { seq, .. }
            | Self::WriteApplied { seq, .. }
            | Self::FactSet { seq, .. }
            | Self::HookResumeData { seq, .. }
            | Self::Usage { seq, .. } => *seq,
        }
    }

    pub fn lane(&self) -> &str {
        match self {
            Self::OperationStarted { lane, .. }
            | Self::AbortRequested { lane, .. }
            | Self::OperationFinished { lane, .. }
            | Self::LaneMoved { lane, .. }
            | Self::StepAttempt { lane, .. }
            | Self::RetryScheduled { lane, .. }
            | Self::RetryConsumed { lane, .. }
            | Self::ToolStarted { lane, .. }
            | Self::ToolFinished { lane, .. }
            | Self::QueueEnqueued { lane, .. }
            | Self::QueueCancelled { lane, .. }
            | Self::QueueConsumed { lane, .. }
            | Self::WriteDeferred { lane, .. }
            | Self::WriteApplied { lane, .. }
            | Self::FactSet { lane, .. }
            | Self::HookResumeData { lane, .. }
            | Self::Usage { lane, .. } => lane,
        }
    }

    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::OperationStarted { id, .. } => Some(id),
            Self::AbortRequested { run_id, .. }
            | Self::OperationFinished { run_id, .. }
            | Self::LaneMoved { run_id, .. }
            | Self::StepAttempt { run_id, .. }
            | Self::RetryScheduled { run_id, .. }
            | Self::RetryConsumed { run_id, .. }
            | Self::ToolStarted { run_id, .. }
            | Self::ToolFinished { run_id, .. }
            | Self::QueueCancelled { run_id, .. }
            | Self::QueueConsumed { run_id, .. }
            | Self::WriteDeferred { run_id, .. }
            | Self::WriteApplied { run_id, .. } => Some(run_id),
            Self::FactSet { run_id, .. } => run_id.as_deref(),
            Self::HookResumeData { run_id, .. } => run_id.as_deref(),
            Self::QueueEnqueued { run_id, .. } | Self::Usage { run_id, .. } => run_id.as_deref(),
        }
    }

    pub fn turn(&self) -> Option<u32> {
        match self {
            Self::StepAttempt { attempt, .. }
            | Self::RetryScheduled { attempt, .. }
            | Self::RetryConsumed { attempt, .. } => Some(*attempt),
            Self::Usage { attempt, .. } => *attempt,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneStatus {
    Idle,
    SuspendedCrash,
    SuspendedDeferred,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolState {
    pub run_id: String,
    pub assistant_entry_id: String,
    pub tool_index: usize,
    pub tool_call_id: String,
    pub tool_name: String,
    pub result_entry_id: String,
    pub replay: ToolReplaySafety,
    pub completed: bool,
    pub terminate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneState {
    pub name: String,
    pub status: LaneStatus,
    pub leaf_id: Option<String>,
    pub open_operation: Option<String>,
    pub attempts: u32,
    #[serde(default)]
    pub retry: Option<RetryState>,
    pub queued: Vec<QueuedEntry>,
    pub deferred_writes: Vec<ProvisionedEntry>,
    pub abort_requested: bool,
    pub usage: TokenUsage,
    pub tools: Vec<ToolState>,
    #[serde(default)]
    pub facts: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub resume_data: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReducedState {
    pub lanes: Vec<LaneState>,
}

impl ReducedState {
    pub fn lane(&self, name: &str) -> Option<&LaneState> {
        self.lanes.iter().find(|lane| lane.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReduceError {
    DuplicateId(String),
    NonMonotonicSequence { previous: u64, current: u64 },
    MissingParent(String),
    InvalidLane(String),
    MultipleOpenOperations(String),
    UnknownOperation(String),
    InvalidRecord(String),
    Storage(String),
}

impl std::fmt::Display for ReduceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ReduceError {}
