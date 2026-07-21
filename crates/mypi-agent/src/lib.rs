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
pub mod wasi_extension;

pub use agent::Agent;
pub use coding_agent::{CodingAgent, CodingAgentOptions, PlanModeBeforeHook};
pub use commands::{execute_slash_command, parse_slash_command, CommandAction};
pub use compaction::{compact_messages, CompactionOptions};
pub use context::ProjectContext;
pub use engine::{get_runtime, AgentEngine, AgentUIEvent};
pub use events::AgentEvent;
pub use hooks::*;
pub use loop_engine::AgentLoop;
pub use plan_mode::{PlanItem, PlanModeState};
pub use queue::PendingMessageQueue;
pub use session::{Message, Role, Session};
pub use session_tree::{SessionNode, SessionTree};
pub use types::*;
pub use wasi_extension::{
    WasiCommandDefinition, WasiExtension, WasiExtensionCommandResult, WasiExtensionEffect,
    WasiExtensionInvocation, WasiExtensionManager, WasiExtensionManifest, WasiExtensionResponse,
    WasiToolDefinition,
};
