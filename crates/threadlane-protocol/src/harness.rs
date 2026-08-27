use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

use crate::session::{AdvisorNote, AgentMessage, SessionPlan, TokenUsageSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdvisorSeverity {
    #[default]
    Info,
    Warning,
    Critical,
    Aside,
    Concern,
    Blocker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiMessageRole {
    User,
    Assistant,
    System,
    Advisor(AdvisorSeverity),
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMarkerSnapshot {
    pub seq: u64,
    pub pre_tokens: u64,
    pub post_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptItem {
    Message(AgentMessage),
    ContextCompacted(ContextMarkerSnapshot),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        tool_call_id: String,
        content: String,
        is_error: bool,
    },
    Plan(SessionPlan),
    Advisor(AdvisorNote),
    Compaction {
        summary: String,
    },
    Custom {
        data: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationIntent {
    Run,
    Compaction,
    Navigation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationOutcome {
    Completed,
    Aborted,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    Manual,
    AdaptiveBudget,
    OverflowRecovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolReplaySafety {
    Never,
    Safe,
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
    pub position: usize,
    pub source: ContextItemSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    pub role: String,
    pub token_estimate: u32,
    pub status: ContextItemStatus,
    pub digest_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
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
pub enum ProviderOutcome {
    Succeeded,
    Success,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderErrorSummary {
    pub message: String,
    pub code: Option<String>,
    pub status_code: Option<u16>,
    #[serde(default)]
    pub category: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySnapshot {
    pub skills: Vec<String>,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DurableEventRecord {
    pub id: String,
    pub seq: u64,
    pub lane: String,
    pub run_id: Option<String>,
    pub turn: Option<u32>,
    pub kind: DurableEventKind,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionDiagnostics {
    pub total_turns: usize,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub model_context: Vec<Entry>,
    #[serde(default)]
    pub durable_events: Vec<DurableEventRecord>,
    #[serde(default)]
    pub recovery: Vec<LaneRecoveryDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentLifecyclePhase {
    Queued,
    Spawned,
    Started,
    Completed,
    Finished,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsageCause {
    #[default]
    Provider,
    Compaction,
    Subagent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptSnapshot {
    Full { sha256: String, content: String },
    Redacted { sha256: String, byte_len: usize, reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    OperationStarted {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        #[serde(default)]
        source_leaf_id: Option<String>,
        #[serde(default)]
        intent: Option<OperationIntent>,
    },
    ContextManifestCaptured {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        attempt: u32,
        #[serde(default)]
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_estimated_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effective_model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_limit: Option<usize>,
        #[serde(default)]
        context_limit_is_estimate: bool,
        #[serde(default)]
        compaction_generation: u64,
        #[serde(default)]
        items: Vec<ContextManifestItem>,
    },
    ContextCompacted {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        generation: u64,
        reason: CompactionReason,
        #[serde(default)]
        effective_model: String,
        #[serde(default)]
        context_limit: usize,
        #[serde(default)]
        context_limit_is_estimate: bool,
        #[serde(default)]
        pre_tokens: usize,
        #[serde(default)]
        post_tokens: usize,
        #[serde(default)]
        retained_tail_target: usize,
        #[serde(default)]
        retained_tail_tokens: usize,
        #[serde(default)]
        compacted_messages: usize,
    },
    OperationFinished {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        #[serde(default)]
        outcome: Option<OperationOutcome>,
        #[serde(default)]
        error: Option<String>,
    },
    LaneMoved {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        target_leaf_id: String,
    },
    StepAttempt {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        attempt: u32,
        #[serde(default)]
        result_entry_id: String,
        #[serde(default)]
        compaction_reason: Option<String>,
    },
    RetryScheduled {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        attempt: u32,
        #[serde(default)]
        retry_at: u64,
        #[serde(default)]
        reason: String,
    },
    RetryConsumed {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        attempt: u32,
    },
    ToolStarted {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        #[serde(default)]
        assistant_entry_id: String,
        #[serde(default)]
        tool_index: usize,
        tool_call_id: String,
        tool_name: String,
        #[serde(default)]
        effective_args: Value,
        #[serde(default)]
        result_entry_id: String,
        #[serde(default)]
        replay: Option<ToolReplaySafety>,
    },
    ToolFinished {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        tool_call_id: String,
        #[serde(default)]
        result_entry_id: String,
        #[serde(default)]
        terminate: bool,
    },
    Usage {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        #[serde(default)]
        run_id: Option<String>,
        #[serde(default)]
        cause: UsageCause,
        #[serde(default)]
        entry_id: Option<String>,
        #[serde(default)]
        tool_call_id: Option<String>,
        #[serde(default)]
        attempt: Option<u32>,
        usage: TokenUsageSummary,
    },
    RunContextCaptured {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        #[serde(default)]
        attempt: Option<u32>,
        model: String,
        provider: String,
        #[serde(default)]
        reasoning_effort: crate::session::ReasoningEffort,
        #[serde(default)]
        prompt_cache_enabled: bool,
        work_dir: String,
        system_prompt: PromptSnapshot,
        #[serde(default)]
        tool_schema_sha256: String,
        #[serde(default)]
        enabled_tool_names: Vec<String>,
        #[serde(default)]
        capabilities: CapabilitySnapshot,
        #[serde(default)]
        prompt_template_ids: Vec<String>,
        #[serde(default)]
        git_head: Option<String>,
        #[serde(default)]
        context_window_limit: Option<usize>,
        #[serde(default)]
        route_defaults: Option<String>,
    },
    ProviderRequestStarted {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        attempt: u32,
        #[serde(default)]
        provider: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        request_id: Option<String>,
    },
    ProviderRequestFinished {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        attempt: u32,
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        outcome: Option<ProviderOutcome>,
        #[serde(default)]
        error: Option<ProviderErrorSummary>,
        #[serde(default)]
        duration_ms: Option<u64>,
        #[serde(default)]
        usage: Option<TokenUsageSummary>,
    },
    ProviderResponseAttached {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        attempt: u32,
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        entry_id: String,
        #[serde(default)]
        reasoning_entry_id: Option<String>,
    },
    PermissionRequested {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        #[serde(default)]
        run_id: Option<String>,
        #[serde(default)]
        attempt: Option<u32>,
        request_id: String,
        capability: String,
        #[serde(default)]
        scopes: Vec<PermissionTraceScope>,
        #[serde(default)]
        detail_sha256: String,
        #[serde(default)]
        source: Option<PermissionTraceSource>,
    },
    PermissionResolved {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        #[serde(default)]
        run_id: Option<String>,
        #[serde(default)]
        attempt: Option<u32>,
        request_id: String,
        decision: PermissionTraceDecision,
        #[serde(default)]
        scope: Option<PermissionTraceScope>,
        #[serde(default)]
        source: Option<PermissionTraceSource>,
        #[serde(default)]
        remembered: bool,
    },
    ToolExecutionObserved {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        #[serde(default)]
        attempt: Option<u32>,
        tool_call_id: String,
        tool_name: String,
        #[serde(default)]
        executor_kind: String,
        #[serde(default)]
        phase: Option<ToolExecutionPhase>,
        #[serde(default)]
        started_at_ms: Option<u64>,
        #[serde(default)]
        duration_ms: Option<u64>,
        #[serde(default)]
        outcome: Option<ToolExecutionOutcome>,
        #[serde(default)]
        exit_code: Option<i32>,
        #[serde(default)]
        cancelled: bool,
        #[serde(default)]
        is_error: Option<bool>,
        #[serde(default)]
        terminate: Option<bool>,
        #[serde(default)]
        output_sha256: Option<String>,
        #[serde(default)]
        output_bytes: Option<u64>,
    },
    AbortObserved {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        #[serde(default)]
        attempt: Option<u32>,
        #[serde(default)]
        observation: Option<AbortObservation>,
        #[serde(default)]
        initiator: Option<AbortInitiator>,
        #[serde(default)]
        target: Option<AbortTarget>,
        #[serde(default)]
        acknowledged: bool,
        #[serde(default)]
        detail: Option<String>,
    },
    SubagentLifecycle {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        #[serde(default)]
        run_id: Option<String>,
        #[serde(default)]
        attempt: Option<u32>,
        child_run_id: String,
        #[serde(default)]
        parent_tool_call_id: Option<String>,
        #[serde(default)]
        task_index: Option<u32>,
        agent_id: String,
        subagent_lane: String,
        phase: SubagentLifecyclePhase,
        #[serde(default)]
        result_entry_id: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
    StreamCheckpoint {
        #[serde(default)]
        id: String,
        seq: u64,
        lane: String,
        #[serde(default)]
        timestamp: u64,
        run_id: String,
        #[serde(default)]
        attempt: Option<u32>,
        #[serde(default)]
        request_id: String,
        #[serde(default)]
        assistant_entry_id: Option<String>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        reasoning: Option<String>,
        #[serde(default)]
        checkpoint_index: u32,
        #[serde(default)]
        byte_count: u64,
        #[serde(default)]
        fingerprint: String,
    },
    DurableEvent {
        sequence: u64,
        lane: String,
        kind: DurableEventKind,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DurableEventKind {
    Entry {
        role: String,
        parent_id: Option<String>,
    },
    Record,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDecision {
    None,
    ResumeFromLeaf,
    ReplaySafeToolsThenResume,
    AbortUnsafeTool,
    WaitForDeferredResult,
    ExplicitRetryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptedToolDiagnostic {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub replay_safe: bool,
    pub run_id: String,
    pub replay: String,
    pub result_entry_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedWorkDiagnostic {
    pub prompt: String,
    pub queue: String,
    pub entry_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRecoveryDiagnostic {
    pub lane: String,
    pub decision: RecoveryDecision,
    pub open_operation: Option<String>,
    pub status: String,
    pub attempts: usize,
    pub abort_requested: bool,
    pub leaf_id: Option<String>,
    #[serde(default)]
    pub interrupted_tools: Vec<InterruptedToolDiagnostic>,
    #[serde(default)]
    pub queued_work: Vec<QueuedWorkDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceOperation {
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub seq: u64,
    #[serde(default)]
    pub timestamp: u64,
    pub lane: String,
    pub message: AgentMessage,
    pub surface_op: SurfaceOperation,
    #[serde(default)]
    pub terminate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub entries: Vec<Entry>,
}

pub trait SessionStore {
    fn lanes(&self) -> Vec<String>;
    fn plan(&self) -> SessionPlan;
    fn transcript(&self, lane: &str) -> Transcript;
    fn entries(&self) -> &[Entry];
    fn records(&self) -> &[Record];
    fn active_branch_messages(&self, _lane: &str) -> Vec<AgentMessage> {
        self.entries().iter().map(|e| e.message.clone()).collect()
    }
    fn get_persisted_messages(&self) -> Vec<AgentMessage> {
        self.entries().iter().map(|e| e.message.clone()).collect()
    }
    fn facts(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }
    fn name(&self) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct JsonlStore {
    pub entries: Vec<Entry>,
    pub records: Vec<Record>,
    pub plan: SessionPlan,
}

impl JsonlStore {
    pub fn open(_path: &Path) -> Result<Self, String> {
        Ok(Self::default())
    }

    pub fn open_read_only(_path: &Path) -> Result<Self, String> {
        Ok(Self::default())
    }

    pub fn append_entry(&mut self, entry: Entry) -> Result<(), String> {
        self.entries.push(entry);
        Ok(())
    }

    pub fn append_record(&mut self, record: Record) -> Result<(), String> {
        self.records.push(record);
        Ok(())
    }

    pub fn facts(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }
}

impl SessionStore for JsonlStore {
    fn lanes(&self) -> Vec<String> {
        vec!["main".into()]
    }
    fn plan(&self) -> SessionPlan {
        self.plan.clone()
    }
    fn transcript(&self, _lane: &str) -> Transcript {
        Transcript {
            entries: self.entries.clone(),
        }
    }
    fn entries(&self) -> &[Entry] {
        &self.entries
    }
    fn records(&self) -> &[Record] {
        &self.records
    }
}

#[derive(Debug, Clone, Default)]
pub struct TranscriptPage {
    pub items: Vec<TranscriptItem>,
    pub has_older: bool,
    pub next_cursor: Option<usize>,
}

pub fn read_transcript_page(
    _file_path: &Path,
    _cursor: Option<usize>,
    _limit: usize,
) -> Result<TranscriptPage, String> {
    Ok(TranscriptPage::default())
}

pub fn project_session_diagnostics(
    _store: &impl SessionStore,
    _lane: &str,
) -> Result<SessionDiagnostics, String> {
    Ok(SessionDiagnostics::default())
}

#[derive(Debug, Clone, Default)]
pub struct TrajectoryAnomaly {
    pub summary: String,
    pub description: String,
    pub related_refs: Vec<DurableEventRecord>,
}

#[derive(Debug, Clone, Default)]
pub struct TypedTrajectory {
    pub events: Vec<DurableEventRecord>,
    pub anomalies: Vec<TrajectoryAnomaly>,
}

pub fn project_trajectory(_store: &impl SessionStore) -> TypedTrajectory {
    TypedTrajectory::default()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiToolActivity {
    pub id: String,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiChatMessage {
    pub id: String,
    pub role: UiMessageRole,
    pub content: String,
    pub tool_activities: Vec<UiToolActivity>,
    pub reasoning_content: Option<String>,
}

pub fn project_chat_messages(messages: &[AgentMessage]) -> Vec<UiChatMessage> {
    messages
        .iter()
        .enumerate()
        .map(|(i, msg)| {
            let (role, content, reasoning) = match msg {
                AgentMessage::User { content } => (UiMessageRole::User, content.clone(), None),
                AgentMessage::UserWithImages { content, .. } => (UiMessageRole::User, content.clone(), None),
                AgentMessage::Assistant { content, thinking, .. } => (
                    UiMessageRole::Assistant,
                    content.clone().unwrap_or_default(),
                    thinking.clone(),
                ),
                AgentMessage::Tool { content, .. } => (UiMessageRole::System, content.clone(), None),
                AgentMessage::Custom { custom_type, payload } => (
                    UiMessageRole::System,
                    format!("{}: {}", custom_type, payload),
                    None,
                ),
            };
            UiChatMessage {
                id: format!("msg-{i}"),
                role,
                content,
                tool_activities: Vec::new(),
                reasoning_content: reasoning,
            }
        })
        .collect()
}
