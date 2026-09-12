//! System-prompt builder and project context discovery.
//!
//! `context` finds workspace instruction files by walking up from the work
//! directory; `system_prompt` renders the base prompt from the active tool
//! set plus that context. Extracted from `threadlane-session` so runtimes,
//! CLIs, and tests can build prompts without the session crate.

pub mod context;
pub mod system_prompt;

pub use context::ProjectContext;
pub use system_prompt::{build_system_prompt, SystemPromptBuildOptions, SystemPromptConfig};
