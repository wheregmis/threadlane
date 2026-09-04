use super::queue::SteerPriority;
use crate::types::{AgentMessage, ReasoningEffort, TokenUsage};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A bounded, non-secret trace label or identifier.
///
/// Trace producers must use this only for identifiers, categories, and short
/// summaries that are safe to persist. Prompt text, tool arguments/results,
/// provider response bodies, credentials, and other secret-bearing payloads
/// must never be stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TraceString(String);

impl TraceString {
    const MAX_BYTES: usize = 4096;

    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.len() > Self::MAX_BYTES {
            return Err(format!("trace string exceeds {} bytes", Self::MAX_BYTES));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TraceString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct BoundedText(String);

impl BoundedText {
    const MAX_BYTES: usize = 32 * 1024;

    pub fn truncated(value: &str) -> Self {
        if value.len() <= Self::MAX_BYTES {
            return Self(value.into());
        }
        let mut end = Self::MAX_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        Self(value[..end].into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BoundedText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.len() > Self::MAX_BYTES {
            return Err(D::Error::custom("bounded text exceeds 32768 bytes"));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundedPromptText(String);

impl BoundedPromptText {
    const MAX_BYTES: usize = 256 * 1024;

    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.len() > Self::MAX_BYTES {
            Err(format!(
                "prompt text exceeds {} byte limit: got {} bytes",
                Self::MAX_BYTES,
                value.len()
            ))
        } else {
            Ok(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl std::ops::Deref for BoundedPromptText {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for BoundedPromptText {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BoundedPromptText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.len() > Self::MAX_BYTES {
            return Err(D::Error::custom(format!(
                "prompt text exceeds {} byte limit: got {} bytes",
                Self::MAX_BYTES,
                value.len()
            )));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptSnapshot {
    Full {
        /// Deliberately captured resolved system prompt. Producers must apply
        /// their configured redaction policy before constructing this variant.
        content: BoundedPromptText,
        sha256: TraceString,
    },
    Redacted {
        sha256: TraceString,
        byte_len: usize,
        reason: TraceString,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySnapshot {
    /// Stable capability identifiers only. Producers should cap this list at 256 items.
    pub capabilities: Vec<TraceString>,
    pub fingerprint: Option<TraceString>,
}

/// Proof that a run prompt has been committed to the canonical session log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedRun {
    pub session_id: String,
    pub run_id: String,
    pub lane: String,
    pub prompt_entry_id: String,
    pub assistant_entry_id: String,
    pub accepted_through_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderOutcome {
    Completed,
    Aborted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCategory {
    Authentication,
    Authorization,
    RateLimit,
    InvalidRequest,
    Unavailable,
    Timeout,
    Transport,
    Protocol,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderErrorSummary {
    pub category: ErrorCategory,
    /// A provider-defined error code, never a response body or exception dump.
    pub code: Option<TraceString>,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionTraceScope {
    Once,
    Session,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionTraceDecision {
    Allowed,
    Denied,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionTraceSource {
    User,
    Policy,
    PersistedGrant,
    UnattendedDefault,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolExecutionPhase {
    Started,
    Progress,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolExecutionOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbortObservation {
    SignalSent,
    ProviderNotified,
    TaskCancelled,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbortInitiator {
    User,
    Timeout,
    Shutdown,
    Recovery,
    Policy,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbortTarget {
    Provider,
    Tool,
    Subagent,
    Scheduler,
    ActiveRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentLifecyclePhase {
    Spawned,
    Started,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamCheckpointKind {
    AssistantText,
    Reasoning,
    ToolCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SurfaceOperation {
    Append,
    Replace {
        start_seq: u64,
        end_seq: u64,
        #[serde(default)]
        source_event_seqs: Vec<u64>,
    },
}

impl Default for SurfaceOperation {
    fn default() -> Self {
        Self::Append
    }
}

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
    pub surface_op: SurfaceOperation,
    #[serde(default)]
    pub terminate: bool,
}

impl Entry {
    #[cfg(test)]
    pub(crate) fn new(
        id: impl Into<String>,
        parent_id: Option<String>,
        lane: impl Into<String>,
        seq: u64,
        timestamp: u64,
        message: AgentMessage,
        terminate: bool,
    ) -> Self {
        Self {
            id: id.into(),
            parent_id,
            lane: lane.into(),
            seq,
            timestamp,
            message,
            surface_op: SurfaceOperation::Append,
            terminate,
        }
    }
}

fn default_main_lane() -> String {
    "main".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub message: AgentMessage,
    #[serde(default)]
    pub surface_op: SurfaceOperation,
}

impl ProvisionedEntry {
    pub fn new(id: impl Into<String>, parent_id: Option<String>, message: AgentMessage) -> Self {
        Self {
            id: id.into(),
            parent_id,
            message,
            surface_op: SurfaceOperation::Append,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedEntry {
    pub(crate) id: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UsageCause {
    #[default]
    Provider,
    Discarded,
    Tool,
    Replay,
    Compaction,
    Adjustment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryState {
    pub(crate) attempt: u32,
    pub(crate) retry_at: u64,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextItemSource {
    SystemPrompt,
    WorkspaceInstructions,
    Skill,
    Memory,
    Message,
    ToolResult,
    ToolSchema,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextItemStatus {
    Active,
    Superseded,
    Truncated,
    Omitted,
    Compacted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextManifestItem {
    pub(crate) position: usize,
    pub source: ContextItemSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) entry_id: Option<TraceString>,
    pub role: TraceString,
    pub token_estimate: u32,
    pub(crate) status: ContextItemStatus,
    pub digest_sha256: TraceString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<TraceString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub context_id: String,
    pub source_lane: String,
    pub source_run_id: String,
    pub source_tool_call_id: String,
    pub source_entry_id: String,
    pub path: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub file_sha256: TraceString,
    pub output_chars: usize,
    pub captured_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSnapshotLoadOutcome {
    Loaded,
    Stale,
    Missing,
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    Manual,
    AdaptiveBudget,
    OverflowRecovery,
}

impl CompactionReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::AdaptiveBudget => "adaptive_budget",
            Self::OverflowRecovery => "overflow_recovery",
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &u64) -> bool {
    *value == 0
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
    ContextManifestCaptured {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        request_id: TraceString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_estimated_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effective_model: Option<TraceString>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_limit: Option<usize>,
        #[serde(default, skip_serializing_if = "is_false")]
        context_limit_is_estimate: bool,
        #[serde(default, skip_serializing_if = "is_zero")]
        compaction_generation: u64,
        items: Vec<ContextManifestItem>,
    },
    ContextSnapshotIndexed {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        snapshot: ContextSnapshot,
    },
    ContextSnapshotLoaded {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        context_id: String,
        source_lane: String,
        current_digest: Option<TraceString>,
        outcome: ContextSnapshotLoadOutcome,
    },
    ContextCompacted {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        generation: u64,
        reason: CompactionReason,
        effective_model: TraceString,
        context_limit: usize,
        context_limit_is_estimate: bool,
        pre_tokens: usize,
        post_tokens: usize,
        retained_tail_target: usize,
        retained_tail_tokens: usize,
        compacted_messages: usize,
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
    RunContextCaptured {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: Option<u32>,
        model: TraceString,
        provider: TraceString,
        reasoning_effort: ReasoningEffort,
        prompt_cache_enabled: bool,
        work_dir: TraceString,
        system_prompt: PromptSnapshot,
        tool_schema_sha256: TraceString,
        enabled_tool_names: Vec<TraceString>,
        capabilities: CapabilitySnapshot,
        prompt_template_ids: Vec<TraceString>,
        git_head: Option<TraceString>,
        #[serde(default)]
        context_window_limit: Option<usize>,
        #[serde(default)]
        route_defaults: Option<TraceString>,
    },
    ProviderRequestStarted {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        provider: TraceString,
        model: TraceString,
        request_id: Option<TraceString>,
    },
    ProviderRequestFinished {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        request_id: Option<TraceString>,
        outcome: ProviderOutcome,
        error: Option<ProviderErrorSummary>,
        duration_ms: Option<u64>,
        usage: Option<TokenUsage>,
    },
    ProviderResponseAttached {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: u32,
        request_id: Option<TraceString>,
        entry_id: String,
        reasoning_entry_id: Option<String>,
    },
    PermissionRequested {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        attempt: Option<u32>,
        request_id: TraceString,
        capability: TraceString,
        scopes: Vec<PermissionTraceScope>,
        detail_sha256: TraceString,
        source: PermissionTraceSource,
    },
    PermissionResolved {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        attempt: Option<u32>,
        request_id: TraceString,
        decision: PermissionTraceDecision,
        scope: Option<PermissionTraceScope>,
        source: PermissionTraceSource,
        remembered: bool,
    },
    ToolExecutionObserved {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: Option<u32>,
        tool_call_id: TraceString,
        tool_name: TraceString,
        executor_kind: TraceString,
        phase: ToolExecutionPhase,
        started_at_ms: Option<u64>,
        duration_ms: Option<u64>,
        outcome: Option<ToolExecutionOutcome>,
        exit_code: Option<i32>,
        cancelled: bool,
        is_error: Option<bool>,
        terminate: Option<bool>,
        output_sha256: Option<TraceString>,
        output_bytes: Option<u64>,
    },
    AbortObserved {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: Option<u32>,
        observation: AbortObservation,
        initiator: AbortInitiator,
        target: AbortTarget,
        acknowledged: bool,
        detail: Option<TraceString>,
    },
    SubagentLifecycle {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: Option<String>,
        attempt: Option<u32>,
        child_run_id: TraceString,
        parent_tool_call_id: Option<TraceString>,
        task_index: Option<u32>,
        agent_id: TraceString,
        subagent_lane: TraceString,
        phase: SubagentLifecyclePhase,
        result_entry_id: Option<TraceString>,
        error: Option<TraceString>,
    },
    /// Observational snapshot of an active streaming provider response.
    /// Checkpoints are purely observational telemetry for progress tracking and UI telemetry;
    /// they are NOT model-visible, NOT replayable context, and do not reconstruct partial turns.
    /// The final committed assistant entry remains the authoritative model record.
    StreamCheckpoint {
        id: String,
        seq: u64,
        lane: String,
        timestamp: u64,
        run_id: String,
        attempt: Option<u32>,
        request_id: TraceString,
        assistant_entry_id: Option<TraceString>,
        text: Option<BoundedText>,
        reasoning: Option<BoundedText>,
        checkpoint_index: u32,
        byte_count: u64,
        /// A non-reversible digest of the checkpoint content.
        fingerprint: TraceString,
    },
}

impl Record {
    pub(crate) fn with_seq(self, seq: u64) -> Self {
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
            mut record @ Self::RunContextCaptured { .. } => {
                if let Self::RunContextCaptured { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ContextManifestCaptured { .. } => {
                if let Self::ContextManifestCaptured { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ContextSnapshotIndexed { .. } => {
                if let Self::ContextSnapshotIndexed { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ContextSnapshotLoaded { .. } => {
                if let Self::ContextSnapshotLoaded { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ProviderRequestStarted { .. } => {
                if let Self::ProviderRequestStarted { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ContextCompacted { .. } => {
                if let Self::ContextCompacted { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ProviderRequestFinished { .. } => {
                if let Self::ProviderRequestFinished { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ProviderResponseAttached { .. } => {
                if let Self::ProviderResponseAttached { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::PermissionRequested { .. } => {
                if let Self::PermissionRequested { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::PermissionResolved { .. } => {
                if let Self::PermissionResolved { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::ToolExecutionObserved { .. } => {
                if let Self::ToolExecutionObserved { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::AbortObserved { .. } => {
                if let Self::AbortObserved { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::SubagentLifecycle { .. } => {
                if let Self::SubagentLifecycle { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
            mut record @ Self::StreamCheckpoint { .. } => {
                if let Self::StreamCheckpoint { seq: current, .. } = &mut record {
                    *current = seq;
                }
                record
            }
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
            | Self::Usage { id, .. }
            | Self::RunContextCaptured { id, .. }
            | Self::ContextManifestCaptured { id, .. }
            | Self::ContextSnapshotIndexed { id, .. }
            | Self::ContextSnapshotLoaded { id, .. }
            | Self::ContextCompacted { id, .. }
            | Self::ProviderRequestStarted { id, .. }
            | Self::ProviderRequestFinished { id, .. }
            | Self::ProviderResponseAttached { id, .. }
            | Self::PermissionRequested { id, .. }
            | Self::PermissionResolved { id, .. }
            | Self::ToolExecutionObserved { id, .. }
            | Self::AbortObserved { id, .. }
            | Self::SubagentLifecycle { id, .. }
            | Self::StreamCheckpoint { id, .. } => id,
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
            | Self::Usage { seq, .. }
            | Self::RunContextCaptured { seq, .. }
            | Self::ContextManifestCaptured { seq, .. }
            | Self::ContextSnapshotIndexed { seq, .. }
            | Self::ContextSnapshotLoaded { seq, .. }
            | Self::ContextCompacted { seq, .. }
            | Self::ProviderRequestStarted { seq, .. }
            | Self::ProviderRequestFinished { seq, .. }
            | Self::ProviderResponseAttached { seq, .. }
            | Self::PermissionRequested { seq, .. }
            | Self::PermissionResolved { seq, .. }
            | Self::ToolExecutionObserved { seq, .. }
            | Self::AbortObserved { seq, .. }
            | Self::SubagentLifecycle { seq, .. }
            | Self::StreamCheckpoint { seq, .. } => *seq,
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
            | Self::Usage { lane, .. }
            | Self::RunContextCaptured { lane, .. }
            | Self::ContextManifestCaptured { lane, .. }
            | Self::ContextSnapshotIndexed { lane, .. }
            | Self::ContextSnapshotLoaded { lane, .. }
            | Self::ContextCompacted { lane, .. }
            | Self::ProviderRequestStarted { lane, .. }
            | Self::ProviderRequestFinished { lane, .. }
            | Self::ProviderResponseAttached { lane, .. }
            | Self::PermissionRequested { lane, .. }
            | Self::PermissionResolved { lane, .. }
            | Self::ToolExecutionObserved { lane, .. }
            | Self::AbortObserved { lane, .. }
            | Self::SubagentLifecycle { lane, .. }
            | Self::StreamCheckpoint { lane, .. } => lane,
        }
    }

    pub(crate) fn run_id(&self) -> Option<&str> {
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
            Self::RunContextCaptured { run_id, .. }
            | Self::ContextManifestCaptured { run_id, .. }
            | Self::ContextSnapshotIndexed { run_id, .. }
            | Self::ContextSnapshotLoaded { run_id, .. }
            | Self::ContextCompacted { run_id, .. }
            | Self::ProviderRequestStarted { run_id, .. }
            | Self::ProviderRequestFinished { run_id, .. }
            | Self::ProviderResponseAttached { run_id, .. }
            | Self::ToolExecutionObserved { run_id, .. }
            | Self::AbortObserved { run_id, .. }
            | Self::StreamCheckpoint { run_id, .. } => Some(run_id),
            Self::FactSet { run_id, .. }
            | Self::HookResumeData { run_id, .. }
            | Self::QueueEnqueued { run_id, .. }
            | Self::Usage { run_id, .. }
            | Self::PermissionRequested { run_id, .. }
            | Self::PermissionResolved { run_id, .. }
            | Self::SubagentLifecycle { run_id, .. } => run_id.as_deref(),
        }
    }

    pub(crate) fn turn(&self) -> Option<u32> {
        match self {
            Self::StepAttempt { attempt, .. }
            | Self::RetryScheduled { attempt, .. }
            | Self::RetryConsumed { attempt, .. } => Some(*attempt),
            Self::Usage { attempt, .. }
            | Self::RunContextCaptured { attempt, .. }
            | Self::PermissionRequested { attempt, .. }
            | Self::PermissionResolved { attempt, .. }
            | Self::ToolExecutionObserved { attempt, .. }
            | Self::AbortObserved { attempt, .. }
            | Self::SubagentLifecycle { attempt, .. }
            | Self::StreamCheckpoint { attempt, .. } => *attempt,
            Self::ContextManifestCaptured { attempt, .. }
            | Self::ProviderRequestStarted { attempt, .. }
            | Self::ProviderRequestFinished { attempt, .. }
            | Self::ProviderResponseAttached { attempt, .. } => Some(*attempt),
            _ => None,
        }
    }
}

/// Redact secret values and bound oversized payload fields in tool arguments
/// before persisting them to durable traces.
pub fn sanitize_tool_args(value: &Value) -> Value {
    const MAX_FIELD_BYTES: usize = 64 * 1024;
    match value {
        Value::Object(map) => {
            let mut sanitized = serde_json::Map::with_capacity(map.len());
            for (key, val) in map {
                let lower = key.to_ascii_lowercase();
                if lower.contains("token")
                    || lower.contains("secret")
                    || lower.contains("password")
                    || lower.contains("api_key")
                    || lower.contains("credential")
                    || lower.contains("auth")
                    || lower.contains("private_key")
                {
                    sanitized.insert(key.clone(), Value::String("[REDACTED]".into()));
                } else {
                    sanitized.insert(key.clone(), sanitize_tool_args(val));
                }
            }
            Value::Object(sanitized)
        }
        Value::Array(list) => Value::Array(list.iter().map(sanitize_tool_args).collect()),
        Value::String(s) => {
            if s.len() > MAX_FIELD_BYTES {
                let mut end = MAX_FIELD_BYTES;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                Value::String(format!("{}[TRUNCATED {} BYTES]", &s[..end], s.len() - end))
            } else {
                Value::String(s.clone())
            }
        }
        other => other.clone(),
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
    pub(crate) replay: ToolReplaySafety,
    pub completed: bool,
    pub(crate) terminate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneState {
    pub name: String,
    pub status: LaneStatus,
    pub leaf_id: Option<String>,
    pub open_operation: Option<String>,
    pub attempts: u32,
    #[serde(default)]
    pub(crate) retry: Option<RetryState>,
    pub queued: Vec<QueuedEntry>,
    pub(crate) deferred_writes: Vec<ProvisionedEntry>,
    pub abort_requested: bool,
    pub(crate) usage: TokenUsage,
    pub tools: Vec<ToolState>,
    #[serde(default)]
    pub context_snapshots: Vec<ContextSnapshot>,
    #[serde(default)]
    pub(crate) facts: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) resume_data: std::collections::BTreeMap<String, String>,
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

#[derive(Debug, Clone, Default)]
pub struct RecoveryResult {
    pub recovered_open_operations: usize,
    pub open_operation_ids: Vec<String>,
    pub abort_requested_operation_ids: Vec<String>,
    pub unreplayable_tools: usize,
    pub safe_tools_to_replay: Vec<Record>,
}

#[derive(Debug, Clone)]
pub struct InterruptedSubagentLane {
    pub lane: String,
    pub run_id: String,
    pub source_leaf_id: Option<String>,
    pub started_seq: u64,
    pub task: String,
    pub task_attempted: bool,
    pub messages: Vec<AgentMessage>,
    pub safe_tools: Vec<Record>,
    pub unsafe_tools: Vec<Record>,
}
