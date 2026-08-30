use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::permission::PermissionRequest;

pub const ACP_CONFIG_CATEGORY_MODEL: &str = "model";

pub fn is_acp_model(model: &str) -> bool {
    model.starts_with("acp/") || model.starts_with("acp:")
}

pub fn config_option_for(options: &[AcpConfigOption], category: &str) -> Option<AcpConfigOption> {
    options.iter().find(|o| o.category == category).cloned()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpConfigChoice {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpConfigOption {
    pub id: String,
    pub name: String,
    pub category: String,
    pub current_value: Option<String>,
    pub choices: Vec<AcpConfigChoice>,
}

impl AcpConfigOption {
    pub fn current_value(&self) -> Option<&str> {
        self.current_value.as_deref()
    }
    pub fn current_label(&self) -> Option<String> {
        self.current_value.clone()
    }
    pub fn current_detail_label(&self) -> Option<String> {
        self.current_value
            .as_ref()
            .map(|v| format!("{}: {}", self.name, v))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisorNote {
    #[serde(default)]
    pub severity: crate::harness::AdvisorSeverity,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRoles {
    pub roles: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Off,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub fn as_api_str(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Minimal => "Minimal",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::XHigh => "X-High",
            Self::Max => "Max",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessCompositionSnapshot {
    pub active_lane: String,
    pub session_file: Option<String>,
    pub model: String,
    pub provider: String,
    pub skills: Vec<String>,
    pub extensions: Vec<String>,
    pub sandbox_policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub mime_type: String,
    pub data: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub step: String,
    pub status: PlanItemStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default)]
    pub items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsageSummary {
    pub fn accumulate(&mut self, other: &TokenUsageSummary) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

pub type TokenUsage = TokenUsageSummary;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultPayload {
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub function: FunctionCall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum AgentMessage {
    User {
        content: String,
    },
    UserWithImages {
        content: String,
        images: Vec<ImageAttachment>,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deferred_handle: Option<String>,
    },
    Tool {
        tool_call_id: String,
        name: String,
        content: String,
        is_error: bool,
    },
    Custom {
        custom_type: String,
        payload: Value,
    },
}

impl AgentMessage {
    pub fn user(content: impl Into<String>, images: Vec<ImageAttachment>) -> Self {
        if images.is_empty() {
            Self::User {
                content: content.into(),
            }
        } else {
            Self::UserWithImages {
                content: content.into(),
                images,
            }
        }
    }

    pub fn role_str(&self) -> &str {
        match self {
            Self::User { .. } | Self::UserWithImages { .. } => "user",
            Self::Assistant { .. } => "assistant",
            Self::Tool { .. } => "tool",
            Self::Custom { custom_type, .. } => custom_type.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubagentProgressUpdate {
    TextDelta {
        delta: String,
    },
    ReasoningDelta {
        delta: String,
    },
    ToolStarted {
        tool_call_id: String,
        name: String,
        arguments: String,
    },
    ToolUpdated {
        tool_call_id: String,
        #[serde(default)]
        partial_result: String,
        #[serde(default)]
        delta: String,
    },
    ToolFinished {
        tool_call_id: String,
        result: ToolResultPayload,
    },
    Error {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    AgentStart,
    TurnStart {
        turn_number: usize,
    },
    TextDelta {
        delta: String,
    },
    MessageUpdate {
        delta: String,
    },
    ReasoningDelta {
        delta: String,
    },
    ToolExecutionStart {
        tool_call_id: String,
        name: String,
        arguments: String,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        status: String,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        name: String,
        result: ToolResult,
    },
    SubagentQueued {
        run_id: u64,
        task_index: usize,
        agent: String,
        task: String,
    },
    SubagentStarted {
        run_id: u64,
        task_index: usize,
        journal_run_id: String,
        lane: Option<String>,
        agent: String,
        task: String,
        model: Option<String>,
    },
    SubagentUpdate {
        run_id: u64,
        task_index: usize,
        update: SubagentProgressUpdate,
    },
    SubagentFinished {
        run_id: u64,
        task_index: usize,
        journal_run_id: String,
        succeeded: bool,
        error: Option<String>,
    },
    SubagentRecovery {
        run_id: String,
        status: String,
        detail: Option<String>,
    },
    AgentError {
        error: String,
    },
    StreamRuleTriggered {
        rule_name: String,
        reminder: String,
    },
    TurnEnd {
        turn_number: usize,
    },
    AgentEnd {
        usage: Option<TokenUsageSummary>,
    },
}

// ── Client-to-Daemon Requests ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub project_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSessionsRequest {
    pub project_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadSessionLogRequest {
    pub project_path: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadSessionLogResponse {
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendPromptRequest {
    pub session_id: String,
    pub prompt: String,
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueFollowUpRequest {
    pub session_id: String,
    pub prompt: String,
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueSteerRequest {
    pub session_id: String,
    pub prompt: String,
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelRunRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetSessionModelRequest {
    pub session_id: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeSessionRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_sequence: Option<u64>,
}

// ── Responses & DTOs ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub project_path: String,
    pub title: String,
    pub model: String,
    pub created_at: String,
    pub updated_at: String,
    pub is_active: bool,
    /// Daemon-resolved transcript path, so clients never assume the on-disk
    /// session layout. Optional for wire compatibility with older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionDetail {
    pub summary: SessionSummary,
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<SessionPlan>,
    pub latest_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptAcceptedResponse {
    pub session_id: String,
    pub sequence: u64,
    pub run_id: String,
}

// ── Streaming Events ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    SessionStarted {
        session_id: String,
    },
    TurnStarted {
        turn_number: usize,
    },
    TokenDelta {
        delta: String,
    },
    ReasoningDelta {
        delta: String,
    },
    ToolCallStarted {
        tool_call_id: String,
        name: String,
        arguments: String,
    },
    ToolCallUpdated {
        tool_call_id: String,
        partial_result: String,
    },
    ToolCallFinished {
        tool_call_id: String,
        name: String,
        result: ToolResultPayload,
    },
    PlanUpdated {
        plan: SessionPlan,
    },
    AdvisorNote {
        note: AdvisorNote,
    },
    ModelRolesUpdated {
        roles: ModelRoles,
    },
    SubagentStarted {
        run_id: u64,
        lane: String,
        agent: String,
        task: String,
    },
    SubagentUpdated {
        run_id: u64,
        lane: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delta: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
    },
    SubagentFinished {
        run_id: u64,
        lane: String,
        succeeded: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    PermissionRequested {
        request: PermissionRequest,
    },
    TurnCompleted {
        turn_number: usize,
        usage: TokenUsageSummary,
    },
    SessionCompleted {
        session_id: String,
        total_usage: TokenUsageSummary,
    },
    Error {
        message: String,
    },
}

// ── Session Discovery & Hydration Types ─────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionHealth {
    Healthy,
    Working,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub work_dir: std::path::PathBuf,
    pub runtime_work_dir: std::path::PathBuf,
    pub session_file: std::path::PathBuf,
    pub updated_at: u64,
    pub health: SessionHealth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub is_worktree: bool,
    pub worktree_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Advisor(crate::harness::AdvisorSeverity),
    Error,
    ContextMarker,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolActivityInfo {
    pub id: String,
    pub category: String,
    pub title: String,
    pub display_summary: String,
    pub detail: String,
    pub is_expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessageInfo {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub tool_activities: Vec<ToolActivityInfo>,
    pub streaming: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    pub reasoning_expanded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryDiagnostics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub model_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    #[serde(default)]
    pub files_mutated: Vec<String>,
    #[serde(default)]
    pub commands_executed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_estimate: Option<u32>,
    #[serde(default)]
    pub is_anomaly: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<u32>,
    pub category: String,
    pub summary: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub diagnostics: TrajectoryDiagnostics,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetricsInfo {
    pub turns: usize,
    pub tool_calls: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl SessionMetricsInfo {
    pub fn billed_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    pub fn cache_hit_percent(&self) -> Option<u64> {
        let billed_input = self.billed_input_tokens();
        (billed_input > 0).then(|| {
            (((self.cache_read_tokens as u128) * 100 + (billed_input as u128) / 2)
                / billed_input as u128) as u64
        })
    }

    pub fn accumulate_usage(&mut self, usage: &TokenUsageSummary) {
        self.input_tokens = self
            .input_tokens
            .saturating_add(u64::from(usage.input_tokens));
        self.output_tokens = self
            .output_tokens
            .saturating_add(u64::from(usage.output_tokens));
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(u64::from(usage.cache_read_tokens));
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(u64::from(usage.cache_write_tokens));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindowInfo {
    pub current_tokens: u64,
    pub context_limit: u64,
    pub context_limit_is_estimate: bool,
    pub effective_model: String,
    pub compaction_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_compaction_seq: Option<u64>,
    pub provisional: bool,
    pub estimating: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentActivityStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentActivityInfo {
    pub batch_run_id: u64,
    pub task_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    pub agent: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: SubagentActivityStatus,
    pub messages: Vec<ChatMessageInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HydrateSessionRequest {
    pub session_id: String,
    pub session_file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HydrateSessionResponse {
    pub messages: Vec<ChatMessageInfo>,
    pub plan: SessionPlan,
    pub trajectory: Vec<TrajectoryEntry>,
    pub subagents: Vec<SubagentActivityInfo>,
    pub diagnostics: crate::harness::SessionDiagnostics,
    pub metrics: SessionMetricsInfo,
    pub token_usage: TokenUsageSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<ContextWindowInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveSessionRequest {
    pub session_id: String,
    pub project_path: String,
}
