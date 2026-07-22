pub mod agent;
pub mod coding_agent;
pub mod commands;
pub mod compaction;
pub mod context;
pub mod engine;
pub mod events;
pub mod hooks;
pub mod loop_engine;
pub mod plan_mode;
pub mod queue;
pub mod session;
pub mod session_tree;
pub mod types;
pub mod capabilities;
pub mod full_trust_extension;
pub mod packages;
pub mod skills;
pub mod supervisor;
pub mod wasi_extension;
pub mod prompt_templates;

pub use agent::Agent;
pub use coding_agent::{CodingAgent, CodingAgentOptions, PlanModeBeforeHook};
pub use commands::{execute_slash_command, parse_slash_command, CommandAction};
pub use compaction::{compact_messages, CompactionOptions};
pub use context::ProjectContext;
pub use engine::{get_runtime, AgentEngine, AgentUIEvent};
pub use events::AgentEvent;
pub use hooks::*;
pub use loop_engine::AgentLoop;
pub use plan_mode::{HarnessPolicy, HarnessPolicyDecision, PlanItem, PlanModeState};
pub use queue::PendingMessageQueue;
pub use session::{Message, Role, Session};
pub use session_tree::{SessionNode, SessionTree};
pub use capabilities::{CapabilityCatalog, ExtensionMetadata};
pub use full_trust_extension::{compute_executable_revision, FullTrustRunner, TrustStore};
pub use packages::{PackageManifest, PackageManager, PackageRecord, PackageScope};
pub use prompt_templates::{
    expand_prompt_template, load_prompt_templates, load_prompt_templates_from_dir,
    parse_command_args, substitute_args, PromptTemplate,
};
pub use skills::{SkillManager, SkillMetadata, SkillScope};
pub use supervisor::{HarnessSupervisor, ProjectRecord, TaskAgentEvent, TaskRecord, TaskStatus};

pub use types::*;
pub use wasi_extension::{
    WasiCommandDefinition, WasiExtension, WasiExtensionCommandResult, WasiExtensionEffect,
    WasiExtensionInvocation, WasiExtensionManager, WasiExtensionManifest, WasiExtensionResponse,
    WasiToolDefinition,
};


