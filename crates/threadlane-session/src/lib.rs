pub mod acp;
pub mod acp_bridge;
pub mod acp_runtime;
pub mod agents;
pub mod commands;
pub mod config;
pub mod context;
pub mod controller;
pub mod error;
pub mod extension_broker;
pub mod orchestrator;
pub mod permission;
mod plan;
pub mod policy;
pub mod project_registry;
pub mod prompt_templates;
pub mod supervisor;
pub mod system_prompt;

// ── SessionController & CodingAgent ──────────────────────────────────
mod capabilities_catalog;
pub mod coding_agent;
pub use capabilities_catalog::CapabilityCatalog;
pub use coding_agent::harness::{
    CodingSessionHarness, HarnessRecord, HarnessWatch, InterruptedSubagentRecoveryState,
};
pub use coding_agent::{
    cancel_open_subagent_operations, AgentRunTask, CodingAgent, CodingAgentCancellation,
    CodingAgentOptions, CodingAgentWorkHandle, HarnessCompositionSnapshot,
    SubagentCancellationGuard, SubagentInnerTool, SubagentInnerToolData, SubagentResult,
    SubagentSessionData,
};
pub use controller::{ExecutionMode, SessionController, SessionStatus};

// ── Re-exports ───────────────────────────────────────────────────────
pub use acp::{
    config_option_for, AcpAgentCapabilities, AcpAgentConfig, AcpAgentRecord, AcpAgentStatus,
    AcpAuthMethod, AcpClientHandler, AcpConfigOption, AcpConfigOptionChoice, AcpConnection,
    AcpContentBlock, AcpInitializeResult, AcpManager, AcpNewSessionResult, AcpPermissionOption,
    AcpPermissionOptionKind, AcpPermissionOutcome, AcpPermissionPolicy, AcpPermissionRequest,
    AcpPermissionResponder, AcpPlanEntry, AcpProbeClient, AcpReadTextFileRequest, AcpScope,
    AcpSession, AcpSessionNotification, AcpSessionUpdate, AcpSettings, AcpStopReason, AcpToolCall,
    AcpToolCallStatus, AcpToolKind, AcpWorkspaceClient, AcpWriteTextFileRequest,
    ACP_CONFIG_CATEGORY_EFFORT, ACP_CONFIG_CATEGORY_MODE, ACP_CONFIG_CATEGORY_MODEL,
    ACP_CONFIG_ID_AGENT, ACP_PROTOCOL_VERSION,
};
pub use acp_bridge::{
    acp_agent_id, acp_model_id, agent_events_for, is_acp_model, ACP_MODEL_PREFIX,
};
pub use acp_runtime::AcpEngine;
pub use commands::{
    available_slash_commands, builtin_commands, execute_slash_command, parse_slash_command,
    CommandAction, SlashCommandInfo,
};
pub use config::{CodingAgentConfig, CodingAgentConfigBuilder};
pub use context::ProjectContext;
pub use permission::{PermissionDecision, PermissionHandle};
pub use policy::ToolPolicy;
pub use project_registry::{
    load_project_registry, register_project, save_project_registry, select_project, ProjectRecord,
};
pub use prompt_templates::{
    expand_prompt_template, load_prompt_templates, parse_command_args, substitute_args,
    PromptTemplate,
};
pub use supervisor::{HarnessSupervisor, TaskAgentEvent, TaskKind, TaskRecord, TaskStatus};
pub use system_prompt::SystemPromptConfig;

// Re-export the runtime crate's public API so downstream crates (GPUI)
// can use a single dependency.
pub use threadlane_mcp::*;
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
