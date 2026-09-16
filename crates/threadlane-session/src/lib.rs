pub mod acp;
pub mod acp_bridge;
pub mod acp_presets;
pub mod acp_runtime;
/// Agent definitions, canonical in `threadlane_skills::agents`.
/// Re-exported here so existing `threadlane_session::agents` paths keep
/// working; new code should import `threadlane_skills::agents` directly.
pub use threadlane_skills::agents;
pub mod browser;
pub mod commands;
pub mod computer;
pub mod config;
pub mod config_dump;
pub mod discovery;
/// Project context discovery, canonical in `threadlane-prompt`.
/// Re-exported here so existing `threadlane_session::context` paths keep
/// working; new code should import `threadlane_prompt` directly.
pub use threadlane_prompt as context;
pub mod controller;
pub mod credentials;
pub mod extension_broker;
pub mod mcp;
/// Prewalk orchestration, canonical in `threadlane_orchestrator`.
/// Re-exported here so existing `threadlane_session::orchestrator` paths keep
/// working; new code should import `threadlane_orchestrator` directly.
pub use threadlane_runtime::orchestrator;
pub mod permission;
pub mod process_environment;
pub mod settings;
pub mod subagent_settings;
pub mod titles;
/// Attached-project registry, canonical in `threadlane-project`.
/// Re-exported here so existing `threadlane_session::project_registry` and
/// `threadlane_session::ProjectRecord` paths keep working; new code should
/// import `threadlane_project` directly.
pub use threadlane_project as project_registry;
/// Model-managed session plans, canonical in `threadlane_plan` (persisted
/// through its `PlanJournal` trait; `threadlane_runtime::plan` adapts the
/// session JSONL).
/// Re-exported here so existing `threadlane_session::plan` paths keep
/// working; new code should import `threadlane_plan` directly.
pub use threadlane_runtime::plan;
/// Execution policy, canonical in `threadlane_runtime::capability`.
/// Re-exported here so existing `threadlane_session::ToolPolicy` paths keep
/// working; new code should import `threadlane_runtime::ToolPolicy` directly.
pub use threadlane_runtime::ToolPolicy;
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
    subagent_workspace, AgentRunTask, CodingAgent, CodingAgentCancellation, CodingAgentOptions,
    CodingAgentWorkHandle, HarnessCompositionSnapshot, SubagentCancellationGuard,
};
pub use controller::{ExecutionMode, SessionController, SessionStatus};
pub type SessionRuntime = SessionController;
pub type SessionRuntimeStatus = SessionStatus;

pub fn runtime_status_text(status: SessionRuntimeStatus) -> Option<String> {
    match status {
        SessionRuntimeStatus::Ready => None,
        SessionRuntimeStatus::Working => Some("Working…".into()),
        SessionRuntimeStatus::Interrupted => {
            Some("Turn interrupted · Safe replay checkpoints available".into())
        }
        SessionRuntimeStatus::Error(error) => Some(error),
    }
}

/// Construct a session controller on the shared Tokio blocking pool. WASI
/// extension loading needs the larger stack and reactor provided there.
pub fn spawn_session_runtime_construction(
    options: CodingAgentOptions,
    mode: ExecutionMode,
) -> tokio::task::JoinHandle<std::sync::Arc<SessionController>> {
    threadlane_runtime::get_runtime().spawn_blocking(move || SessionController::new(options, mode))
}

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
pub use acp_presets::{
    add_acp_agent, configured_acp_agents, remove_acp_agent, set_acp_enabled,
    set_acp_preset_enabled, upgrade_acp_presets, AcpPreset, ACP_PRESETS,
};
pub use acp_runtime::AcpEngine;
pub use browser::{ActTarget, BrowserBridge, BrowserCommand, BrowserRequest};
pub use commands::{available_slash_commands, SlashCommandInfo};
pub use config::CodingAgentConfig;
pub use credentials::{
    opencode_api_key, provider_client_for, provider_credentials, AuthCredentialBridge,
};
pub use permission::{PermissionDecision, PermissionHandle};
pub use question::QuestionHandle;
pub use system_prompt::SystemPromptConfig;
pub use threadlane_project::{
    load_project_registry, register_project, save_project_registry, select_project, ProjectRecord,
};
pub use threadlane_skills::prompts::PromptTemplate;

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
