//! Shared GUI state & background task event types.
//!
//! Panel-specific state slices live in `crate::panels::<panel>::state`.

use mypi_agent::{AgentEvent, AgentMessage};
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
    DeviceCodePrompt {
        user_code: String,
        url: String,
    },
    DeviceLoginSuccess,
    AvailableModelsLoaded(Vec<String>),
    CommandOutput(String),
    /// Session file swap finished; UI should rebuild chat from these messages.
    SessionSwitched {
        session_id: String,
        title: String,
        work_dir: PathBuf,
        messages: Vec<AgentMessage>,
    },
}
