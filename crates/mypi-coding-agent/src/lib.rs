pub mod capabilities;
pub mod coding_agent;
pub mod commands;
pub mod context;
pub mod full_trust_extension;
pub mod packages;
pub mod prompt_templates;
pub mod skills;
pub mod supervisor;
pub mod wasi_extension;

pub use capabilities::{CapabilityCatalog, ExtensionMetadata};
pub use coding_agent::{CodingAgent, CodingAgentOptions, ExtensionBeforeToolHook, ToolPolicy};
pub use commands::{execute_slash_command, parse_slash_command, CommandAction};
pub use context::ProjectContext;
pub use full_trust_extension::{compute_executable_revision, FullTrustRunner, TrustStore};
pub use packages::{PackageManifest, PackageManager, PackageRecord, PackageScope};
pub use prompt_templates::{
    expand_prompt_template, load_prompt_templates, load_prompt_templates_from_dir,
    parse_command_args, substitute_args, PromptTemplate,
};
pub use skills::{SkillManager, SkillMetadata, SkillScope};
pub use supervisor::{HarnessSupervisor, ProjectRecord, TaskAgentEvent, TaskRecord, TaskStatus};
pub use wasi_extension::{
    WasiCommandDefinition, WasiExtension, WasiExtensionCommandResult, WasiExtensionEffect,
    WasiExtensionInvocation, WasiExtensionManager, WasiExtensionManifest, WasiExtensionResponse,
    WasiToolDefinition,
};
