//! The runner: claims lanes from a hub and executes their runs (Plan.md §3.3, §4.3).
//!
//! The hub decides and the runner executes: [`Runner`] asks for a lane, waits
//! out the debounce window, then repeatedly asks the hub for the next run,
//! drives it on an [`AgentBackend`](threadlane_core::backend::AgentBackend),
//! mirrors its events, and completes or fails it, until the hub has nothing
//! more to run and the lane is released. See `docs/milestone-1.md` §3 and §8.

mod config;
mod error;
mod forge;
mod lane;
mod mirror;
mod runner;
mod tools;
mod workspace;

pub use crate::config::RunnerConfig;
pub use crate::error::RunnerError;
pub use crate::forge::GitForge;
pub use crate::runner::{PROGRESS_PROMPT, Runner};
pub use crate::workspace::GitWorkspaces;
