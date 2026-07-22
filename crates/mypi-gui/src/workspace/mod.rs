use crate::state::{ChatData, PlanData};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SessionKey {
    pub work_dir: PathBuf,
    pub session_id: String,
}

impl SessionKey {
    pub fn new(work_dir: PathBuf, session_id: impl Into<String>) -> Self {
        Self {
            work_dir: std::fs::canonicalize(&work_dir).unwrap_or(work_dir),
            session_id: session_id.into(),
        }
    }
}

#[derive(Default)]
pub struct WorkspaceUiState {
    pub draft: String,
    pub plan_drawer_open: bool,
}

pub struct SessionWorkspace {
    pub chat: ChatData,
    pub plan: PlanData,
    pub ui: WorkspaceUiState,
}

impl Default for SessionWorkspace {
    fn default() -> Self {
        Self {
            chat: ChatData {
                messages: Vec::new(),
                streaming_text: String::new(),
                streaming_kind: None,
            },
            plan: PlanData {
                available: false,
                enabled: false,
                items: Vec::new(),
            },
            ui: WorkspaceUiState::default(),
        }
    }
}

#[derive(Default)]
pub struct AppState {
    active: Option<SessionKey>,
    workspaces: HashMap<SessionKey, SessionWorkspace>,
}

impl AppState {
    pub fn select(&mut self, key: SessionKey) {
        self.workspaces.entry(key.clone()).or_default();
        self.active = Some(key);
    }

    pub fn workspace(&self, key: &SessionKey) -> Option<&SessionWorkspace> {
        self.workspaces.get(key)
    }

    pub fn workspace_mut(&mut self, key: SessionKey) -> &mut SessionWorkspace {
        self.workspaces.entry(key).or_default()
    }

    pub fn active_workspace(&self) -> Option<&SessionWorkspace> {
        self.active
            .as_ref()
            .and_then(|key| self.workspaces.get(key))
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut SessionWorkspace> {
        let key = self.active.clone()?;
        self.workspaces.get_mut(&key)
    }

    pub fn remove(&mut self, key: &SessionKey) {
        self.workspaces.remove(key);
        if self.active.as_ref() == Some(key) {
            self.active = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChatMessage, MsgRole};
    use std::path::PathBuf;

    #[test]
    fn workspace_retains_data_after_switching_active_session() {
        let first = SessionKey::new(PathBuf::from("/project"), "first");
        let second = SessionKey::new(PathBuf::from("/project"), "second");
        let mut state = AppState::default();

        state.select(first.clone());
        state
            .workspace_mut(first.clone())
            .chat
            .messages
            .push(ChatMessage::Text {
                role: MsgRole::User,
                text: "first draft".into(),
            });
        state.select(second);

        assert_eq!(state.workspace(&first).unwrap().chat.messages.len(), 1);
        assert_eq!(state.active_workspace().unwrap().chat.messages.len(), 0);
    }

    #[test]
    fn workspace_key_distinguishes_identical_session_ids_in_different_projects() {
        let first = SessionKey::new(PathBuf::from("/one"), "shared");
        let second = SessionKey::new(PathBuf::from("/two"), "shared");
        let mut state = AppState::default();

        state.select(first.clone());
        state
            .workspace_mut(first.clone())
            .chat
            .messages
            .push(ChatMessage::Text {
                role: MsgRole::User,
                text: "one".into(),
            });
        state.select(second.clone());

        assert_eq!(state.workspace(&first).unwrap().chat.messages.len(), 1);
        assert_eq!(state.workspace(&second).unwrap().chat.messages.len(), 0);
    }

    #[test]
    fn workspace_retains_its_own_draft_and_plan_drawer_state() {
        let first = SessionKey::new(PathBuf::from("/project"), "first");
        let second = SessionKey::new(PathBuf::from("/project"), "second");
        let mut state = AppState::default();

        state.select(first.clone());
        let first_workspace = state.workspace_mut(first.clone());
        first_workspace.ui.draft = "first draft".into();
        first_workspace.ui.plan_drawer_open = true;

        state.select(second.clone());
        state.workspace_mut(second).ui.draft = "second draft".into();

        state.select(first.clone());
        let restored = state.workspace(&first).unwrap();
        assert_eq!(restored.ui.draft, "first draft");
        assert!(restored.ui.plan_drawer_open);
    }

    #[test]
    fn inactive_workspace_streaming_and_plan_do_not_affect_active_workspace() {
        let first = SessionKey::new(PathBuf::from("/project"), "first");
        let second = SessionKey::new(PathBuf::from("/project"), "second");
        let mut state = AppState::default();
        state.select(first.clone());

        let inactive = state.workspace_mut(second);
        inactive
            .chat
            .push_stream_delta(crate::state::StreamingKind::Assistant, "offscreen");
        inactive.plan.enabled = true;

        let active = state.active_workspace().unwrap();
        assert!(active.chat.streaming_text.is_empty());
        assert!(!active.plan.enabled);
    }

    #[test]
    fn removing_a_workspace_clears_its_active_selection() {
        let key = SessionKey::new(PathBuf::from("/project"), "removed");
        let mut state = AppState::default();
        state.select(key.clone());

        state.remove(&key);

        assert!(state.workspace(&key).is_none());
        assert!(state.active_workspace().is_none());
    }
}
