//! Coding agent engine for Threadlane.
//!
//! [`CodingAgent`] is the durable prompt-execution runtime (provider turns,
//! tool dispatch, subagents, queue/steer, cancellation); [`harness`] is the
//! canonical session adapter persisting every intent before work starts.
//! [`controller`] is the interactive session controller, and the
//! support modules (`commands`, `computer`, `credentials`, `mcp`) are the
//! session-owned adapters the engine is built from. Import each module
//! directly; the root re-exports below cover only the handful of names
//! external crates actually use flat.

pub mod broker;
pub mod cancellation;
pub mod capabilities;
pub mod commands;
pub mod computer;
pub mod config_dump;
pub mod context_snapshots;
pub mod controller;
pub mod credentials;
pub(crate) mod durable;
pub mod harness;
pub mod mailbox;
pub mod mcp;
pub mod options;
pub mod runtime;
pub mod scheduler;
pub mod subagents;

pub use cancellation::cancel_open_subagent_operations;
pub use options::CodingAgentOptions;
pub use runtime::CodingAgent;
