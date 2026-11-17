use serde::{Deserialize, Serialize};

pub mod acp;
pub mod automation;
pub mod browser;
pub mod daemon;
pub mod editor_lsp;
pub mod events;
pub mod efficiency;
pub mod interaction;
pub mod live;
pub mod messages;
pub mod orchestration;
pub mod repo;
pub mod projection;
pub mod tool;
pub mod transcript;

pub use acp::{
    apply_pending_config_values, config_option_for, AcpConfigOption, AcpConfigOptionChoice,
    ACP_CONFIG_CATEGORY_EFFORT, ACP_CONFIG_CATEGORY_MODE, ACP_CONFIG_CATEGORY_MODEL,
};
pub use browser::{ActTarget, BrowserBridge, BrowserCommand};
pub use events::{
    AgentEvent, HarnessMetrics, SubagentIsolation, SubagentProgressUpdate, SubagentRecoveryStatus,
};
pub use interaction::{
    PermissionRequest, PermissionScope, QuestionAnswer, QuestionItem, QuestionItemAnswer,
    QuestionRequest,
};
pub use live::{LiveOverlayKind, StreamTarget, LIVE_FRAME_MAX_WIDTH};
pub use messages::{
    AgentMessage, AgentToolCall, AgentToolDefinition, AgentToolResult, DeferredHandle,
    ImageAttachment, PlanItem, PlanItemStatus, ReasoningEffort, RetryPrompt, SessionPlan,
    TokenUsage,
};
pub use orchestration::OrchestratorMode;
pub use tool::{
    RecoveredToolReply, ToolExecutionError, ToolExecutionIdentity, ToolExecutor, ToolOutput,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeToolCallFunction {
    pub name: String,
    pub arguments: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeToolCall {
    pub id: String,
    pub r#type: String,
    pub function: RuntimeToolCallFunction,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "thoughtSignature"
    )]
    pub thought_signature: Option<String>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub total_tokens: u32,
}

/// What Threadlane can observe about provider prompt caching. A missing TTL
/// or hit report is unknown, never evidence of a miss or free inference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCacheCapabilities {
    pub accepts_cache_key: bool,
    pub reports_cached_tokens: bool,
    pub ttl_seconds: Option<u32>,
}

impl ProviderCacheCapabilities {
    pub fn observed_hit(&self, usage: Option<&RuntimeUsage>) -> Option<bool> {
        (self.reports_cached_tokens && usage.is_some_and(|usage| usage.cache_read_tokens > 0))
            .then_some(true)
    }
}

#[cfg(test)]
mod cache_capability_tests {
    use super::*;

    #[test]
    fn zero_cached_tokens_is_unknown_not_a_measured_miss() {
        let capabilities = ProviderCacheCapabilities {
            accepts_cache_key: true,
            reports_cached_tokens: true,
            ttl_seconds: None,
        };
        assert_eq!(capabilities.observed_hit(Some(&RuntimeUsage::default())), None);
        let usage = RuntimeUsage {
            cache_read_tokens: 12,
            ..Default::default()
        };
        assert_eq!(capabilities.observed_hit(Some(&usage)), Some(true));
        assert_eq!(ProviderCacheCapabilities::default().observed_hit(Some(&usage)), None);
    }
}
#[derive(Debug, Clone)]
pub enum RuntimeStreamEvent {
    ContentToken(String),
    ReasoningToken(String),
    ToolCallStart {
        name: String,
    },
    ToolCallArgsDelta {
        args_chunk: String,
    },
    Finished {
        tool_calls: Vec<RuntimeToolCall>,
        usage: RuntimeUsage,
    },
    Error(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferredResponse {
    Pending,
    Ready { content: String },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRequest {
    pub model: String,
    pub messages: serde_json::Value,
    pub tools: serde_json::Value,
    pub prompt_cache_key: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[async_trait::async_trait]
pub trait ProviderPort: Send + Sync {
    async fn stream_request(
        &self,
        request: RuntimeRequest,
        events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
    );
    async fn fetch_deferred(
        &self,
        model: &str,
        handle_id: &str,
    ) -> Result<DeferredResponse, String>;
    async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String>;
    fn provider_kind(&self, model: &str) -> &'static str;
    fn cache_capabilities(&self, _model: &str) -> ProviderCacheCapabilities {
        ProviderCacheCapabilities::default()
    }
    /// Rotate the OpenAI-branch credential for subsequent requests. Used
    /// when the session model changes providers mid-task (slash `/model`,
    /// Fusion routing); the shared cell inside `ProviderClient` makes the
    /// rotation visible to in-flight turn loops. Default no-op so test
    /// doubles and non-OpenAI clients compile unchanged.
    fn refresh_openai_credentials(&self, _api_key: String, _account_id: Option<String>) {}
}
