//! Shared GUI state & background task event types.
//!
//! Panel-specific state slices live in `crate::panels::<panel>::state`.

use mypi_agent::AgentEvent;
use mypi_coding_agent::TaskAgentEvent;
use std::path::PathBuf;

pub use crate::panels::chat::*;
pub use crate::panels::command_palette::*;
pub use crate::panels::plan::*;
pub use crate::panels::sessions::*;

/// Events sent from background tokio tasks to the UI thread.
pub enum GuiAgentEvent {
    TaskEvent(TaskAgentEvent),
    Agent(AgentEvent),
    GenerationAgent {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
        event: AgentEvent,
    },
    DeviceCodePrompt {
        user_code: String,
        url: String,
    },
    DeviceLoginSuccess,
    DeviceLoginError(String),
    AvailableModelsLoaded(Vec<String>),
    CommandOutput {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
        output: String,
    },
    GenerationFinished {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
    },
}
