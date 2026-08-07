pub mod acp;
pub mod acp_bridge;
pub mod agents;
pub mod capabilities;
pub mod coding_agent;
pub mod commands;
pub mod context;
pub mod extension_broker;
pub mod frontmatter;
pub mod mcp;
pub mod packages;
mod plan;
pub mod policy;
pub mod prompt_templates;
pub mod skills;
pub mod supervisor;
pub mod system_prompt;
pub mod wasi_extension;

pub use acp::{
    AcpAgentCapabilities, AcpAgentConfig, AcpAgentRecord, AcpAgentStatus, AcpAuthMethod,
    AcpClientHandler, AcpConnection, AcpContentBlock, AcpInitializeResult, AcpManager,
    AcpNewSessionResult, AcpPermissionOption, AcpPermissionOptionKind, AcpPermissionOutcome,
    AcpPermissionPolicy, AcpPermissionRequest, AcpPlanEntry, AcpProbeClient,
    AcpReadTextFileRequest, AcpScope, AcpSession, AcpSessionNotification, AcpSessionUpdate,
    AcpSettings, AcpStopReason, AcpToolCall, AcpToolCallStatus, AcpToolKind, AcpWorkspaceClient,
    AcpWriteTextFileRequest, ACP_PROTOCOL_VERSION,
};
pub use acp_bridge::{
    acp_agent_id, acp_model_id, agent_events_for, is_acp_model, ACP_MODEL_PREFIX,
};
pub use agents::{discover_agents, AgentConfig, AgentDiscoveryResult, AgentScope, AgentSource};
pub use capabilities::CapabilityCatalog;
pub use coding_agent::{
    cancel_open_subagent_operations, CodingAgent, CodingAgentCancellation, CodingAgentOptions,
    CodingAgentWorkHandle, ExtensionBeforeToolHook, HarnessWatch,
};
pub use commands::{execute_slash_command, parse_slash_command, CommandAction};
pub use context::{ProjectContext, ProjectInstruction};
pub use policy::ToolPolicy;
pub use prompt_templates::{
    expand_prompt_template, load_prompt_templates, parse_command_args, substitute_args,
    PromptTemplate,
};
pub use supervisor::{
    HarnessSupervisor, ProjectRecord, TaskAgentEvent, TaskKind, TaskRecord, TaskStatus,
};
pub use system_prompt::SystemPromptConfig;
pub use threadlane_mcp::*;
pub use threadlane_skills::*;
pub use threadlane_wasi::broker::*;
pub use threadlane_wasi::packages::*;
pub use threadlane_wasi::*;
