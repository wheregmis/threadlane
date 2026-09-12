pub mod acp;
pub mod acp_bridge;
pub mod acp_runtime;
/// Agent definitions, canonical in `threadlane_skills::agents`.
/// Re-exported here so existing `threadlane_session::agents` paths keep
/// working; new code should import `threadlane_skills::agents` directly.
pub use threadlane_skills::agents as agents;
pub mod browser;
pub mod commands;
pub mod computer;
/// Live computer-use feed, canonical in `threadlane-protocol::live`.
/// Re-exported here so existing `threadlane_session::computer_live` paths
/// keep working; new code should import `threadlane_protocol::live` directly.
pub use threadlane_protocol::live as computer_live;
#[cfg(target_os = "macos")]
pub(crate) mod computer_stream;
pub mod config;
/// Project context discovery, canonical in `threadlane-prompt`.
/// Re-exported here so existing `threadlane_session::context` paths keep
/// working; new code should import `threadlane_prompt` directly.
pub use threadlane_prompt as context;
pub mod controller;
pub mod credentials;
pub mod extension_broker;
pub mod mcp;
/// Prewalk orchestration, canonical in `threadlane_runtime::orchestrator`.
/// Re-exported here so existing `threadlane_session::orchestrator` paths keep
/// working; new code should import `threadlane_runtime::orchestrator` directly.
pub use threadlane_runtime::orchestrator as orchestrator;
pub mod permission;
mod plan;
/// Execution policy, canonical in `threadlane_runtime::capability`.
/// Re-exported here so existing `threadlane_session::ToolPolicy` paths keep
/// working; new code should import `threadlane_runtime::ToolPolicy` directly.
pub use threadlane_runtime::ToolPolicy;
/// Attached-project registry, canonical in `threadlane-project`.
/// Re-exported here so existing `threadlane_session::project_registry` and
/// `threadlane_session::ProjectRecord` paths keep working; new code should
/// import `threadlane_project` directly.
pub use threadlane_project as project_registry;
/// Prompt templates, canonical in `threadlane_skills::prompts`.
/// Re-exported here so existing `threadlane_session::prompt_templates` paths
/// keep working; new code should import `threadlane_skills::prompts` directly.
pub use threadlane_skills::prompts as prompt_templates;
pub mod question;
pub mod supervisor;
/// System-prompt builder, canonical in `threadlane-prompt`.
/// Re-exported here so existing `threadlane_session::system_prompt` paths
/// keep working; new code should import `threadlane_prompt` directly.
pub use threadlane_prompt as system_prompt;

// ── SessionController & CodingAgent ──────────────────────────────────
pub mod coding_agent;
pub use coding_agent::harness::{
    CodingSessionHarness, HarnessRecord, InterruptedSubagentRecoveryState,
};
pub use coding_agent::{
    AgentRunTask, CodingAgent, CodingAgentCancellation, CodingAgentOptions, CodingAgentWorkHandle,
    HarnessCompositionSnapshot, SubagentCancellationGuard, subagent_workspace,
};
pub use controller::{ExecutionMode, SessionController, SessionStatus};

// ── Re-exports ───────────────────────────────────────────────────────
pub use acp::{
    apply_pending_config_values, config_option_for, AcpAgentCapabilities, AcpAgentConfig,
    AcpAgentRecord, AcpAgentStatus, AcpAuthMethod, AcpConfigOption, AcpConfigOptionChoice,
    AcpConnection, AcpContentBlock, AcpInitializeResult, AcpManager, AcpPreloadedModels, AcpScope,
    AcpSession, AcpSessionNotification, AcpSessionUpdate, AcpSettings, AcpToolCall,
    AcpToolCallStatus, AcpToolKind, ACP_CONFIG_CATEGORY_EFFORT, ACP_CONFIG_CATEGORY_MODE,
    ACP_CONFIG_CATEGORY_MODEL,
};
pub use acp_bridge::{acp_agent_id, acp_model_id, is_acp_model};
pub use acp_runtime::AcpEngine;
pub use browser::{ActTarget, BrowserBridge, BrowserCommand, BrowserRequest};
pub use commands::{available_slash_commands, SlashCommandInfo};
pub use config::CodingAgentConfig;
pub use credentials::{
    opencode_api_key, provider_client_for, provider_credentials, AuthCredentialBridge,
};
pub use permission::{PermissionDecision, PermissionHandle};
pub use threadlane_project::{
    load_project_registry, register_project, save_project_registry, select_project, ProjectRecord,
};
pub use threadlane_skills::prompts::PromptTemplate;
pub use question::QuestionHandle;
pub use system_prompt::SystemPromptConfig;

// Re-export the runtime crate's public API so downstream crates (GPUI)
// can use a single dependency.
pub use mcp::*;
pub use threadlane_runtime::*;
pub use threadlane_skills::*;
pub use threadlane_wasi::broker::*;
pub use threadlane_wasi::packages::*;
pub use threadlane_wasi::*;

/// Narrow adapters for cross-crate integration tests.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    use std::sync::Arc;

    use threadlane_protocol::ProviderPort;

    use crate::coding_agent::{CodingAgent, CodingAgentOptions};

    pub fn coding_agent_with_provider(
        options: CodingAgentOptions,
        provider: Arc<dyn ProviderPort>,
    ) -> CodingAgent {
        CodingAgent::new_with_provider(options, provider)
    }
}
