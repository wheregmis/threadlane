//! Coding agent engine for Threadlane.
//!
//! [`CodingAgent`] is the durable prompt-execution runtime (provider turns,
//! tool dispatch, subagents, queue/steer, cancellation); [`harness`] is the
//! canonical session adapter persisting every intent before work starts.
//! [`controller`] unifies interactive and background execution, and the
//! support modules (`commands`, `computer`, `credentials`, `mcp`) are the
//! session-owned adapters the engine is built from. `threadlane-session`
//! keeps re-export shims until call sites migrate.

pub mod broker;
pub mod cancellation;
pub mod capabilities;
pub mod commands;
pub mod computer;
pub mod config_dump;
pub mod context_snapshots;
pub mod controller;
pub mod credentials;
pub mod durable;
pub mod harness;
pub mod mailbox;
pub mod mcp;
pub mod options;
pub mod runtime;
pub mod scheduler;
pub mod subagents;

pub use cancellation::*;
pub use commands::{available_slash_commands, SlashCommandInfo};
pub use computer::{
    global_previews_dir, watch_display_for_debug, ComputerAct, ComputerApproval,
    ComputerCapability, ComputerDecision, ComputerToolExecutor, TargetedAct, COMPUTER_ACT_TOOL,
    COMPUTER_SCREENSHOT_TOOL, COMPUTER_STATUS_TOOL, COMPUTER_UNAVAILABLE, COMPUTER_WINDOWS_TOOL,
};
pub use controller::{ExecutionMode, SessionController, SessionStatus};
pub use credentials::{
    opencode_api_key, provider_client_for, provider_credentials, refresh_provider_for_model,
    AuthCredentialBridge,
};
pub use harness::{CodingSessionHarness, HarnessRecord, InterruptedSubagentRecoveryState};
pub use mcp::{McpManager, McpToolExecutor};
pub use options::*;
pub use runtime::*;
pub use scheduler::*;
pub use subagents::*;
