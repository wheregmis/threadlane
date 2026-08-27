use std::path::PathBuf;

use threadlane_protocol::{ImageAttachment, ReasoningEffort};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppAction {
    AttachProject(PathBuf),
    SelectSession {
        work_dir: PathBuf,
        session_id: String,
    },
    SettleSession {
        work_dir: PathBuf,
        session_id: String,
    },
    RemoveSession {
        work_dir: PathBuf,
        session_id: String,
    },
    ToggleProject(PathBuf),
    SetSidebarProjectFilter(Option<PathBuf>),
    BeginNewTask,
    SelectDraftProject(PathBuf),
    SelectWorkMode(crate::state::WorkMode),
    SendPrompt(String),
    SendPromptWithImages {
        text: String,
        images: Vec<ImageAttachment>,
    },
    StageBusyMessage {
        text: String,
        images: Vec<ImageAttachment>,
    },
    QueuePendingMessage,
    SteerPendingMessage,
    DismissPendingMessage,
    ToggleToolActivity(String),
    CancelGeneration,
    SelectModel(String),
    SelectReasoningEffort(ReasoningEffort),
    /// Applies one setting an external ACP agent exposes.
    ///
    /// Carries the agent's own option id rather than a Threadlane concept:
    /// the setting list is agent-defined and open-ended.
    SetAcpConfigOption {
        config_id: String,
        value: String,
    },
    OpenSettings,
    CloseSettings,
    SaveOpenAiKey(String),
    SaveOpenCodeKey(String),
    SetActiveCodexAccount(String),
    RemoveCodexAccount(String),
    ToggleReasoningExpanded(String),
    OpenFileInEditor(String),
    RunTerminalCommand(String),
    OpenProjectPicker,
}
