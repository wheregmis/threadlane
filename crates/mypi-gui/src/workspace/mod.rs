use crate::state::ChatData;
use mypi_agent::ImageAttachment;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SessionKey {
    pub work_dir: PathBuf,
    pub session_id: String,
}

impl SessionKey {
    const DRAFT_ID: &'static str = "draft";

    pub fn new(work_dir: PathBuf, session_id: impl Into<String>) -> Self {
        Self {
            work_dir: std::fs::canonicalize(&work_dir).unwrap_or(work_dir),
            session_id: session_id.into(),
        }
    }

    pub fn project_draft(work_dir: PathBuf) -> Self {
        Self::new(work_dir, Self::DRAFT_ID)
    }

    pub fn is_draft(&self) -> bool {
        self.session_id == Self::DRAFT_ID
    }
}

#[derive(Default)]
pub struct WorkspaceUiState {
    pub draft: String,
    pub attachments: Vec<ImageAttachment>,
}

pub struct SessionWorkspace {
    pub chat: ChatData,
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

    pub fn active_key(&self) -> Option<&SessionKey> {
        self.active.as_ref()
    }

    pub fn is_active(&self, key: &SessionKey) -> bool {
        self.active.as_ref() == Some(key)
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

    pub fn keys_for_project<'a>(
        &'a self,
        work_dir: &'a std::path::Path,
    ) -> impl Iterator<Item = &'a SessionKey> {
        self.workspaces
            .keys()
            .filter(move |key| key.work_dir == work_dir)
    }

    pub fn move_workspace(&mut self, from: &SessionKey, to: SessionKey) {
        let workspace = self.workspaces.remove(from).unwrap_or_default();
        self.workspaces.insert(to.clone(), workspace);
        if self.active.as_ref() == Some(from) {
            self.active = Some(to);
        }
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
    fn workspace_retains_its_own_draft() {
        let first = SessionKey::new(PathBuf::from("/project"), "first");
        let second = SessionKey::new(PathBuf::from("/project"), "second");
        let mut state = AppState::default();

        state.select(first.clone());
        let first_workspace = state.workspace_mut(first.clone());
        first_workspace.ui.draft = "first draft".into();

        state.select(second.clone());
        state.workspace_mut(second).ui.draft = "second draft".into();

        state.select(first.clone());
        let restored = state.workspace(&first).unwrap();
        assert_eq!(restored.ui.draft, "first draft");
    }

    #[test]
    fn inactive_workspace_streaming_does_not_affect_active_workspace() {
        let first = SessionKey::new(PathBuf::from("/project"), "first");
        let second = SessionKey::new(PathBuf::from("/project"), "second");
        let mut state = AppState::default();
        state.select(first.clone());

        let inactive = state.workspace_mut(second);
        inactive
            .chat
            .push_stream_delta(crate::state::StreamingKind::Assistant, "offscreen");

        let active = state.active_workspace().unwrap();
        assert!(active.chat.streaming_text.is_empty());
    }

    #[test]
    fn interleaved_cross_project_events_stay_in_their_workspaces() {
        let first = SessionKey::new(PathBuf::from("/one"), "session-a");
        let second = SessionKey::new(PathBuf::from("/two"), "session-b");
        let mut state = AppState::default();
        state.select(second.clone());

        state
            .workspace_mut(first.clone())
            .chat
            .push_stream_delta(crate::state::StreamingKind::Assistant, "from-a");
        state.workspace_mut(second.clone()).chat.push_tool(
            "tool-b".into(),
            "read_file".into(),
            r#"{"path":"b.txt"}"#.into(),
        );
        state.workspace_mut(first.clone()).chat.flush_streaming();
        state
            .workspace_mut(second.clone())
            .chat
            .push_stream_delta(crate::state::StreamingKind::Assistant, "from-b");

        let first_chat = &state.workspace(&first).unwrap().chat;
        let second_chat = &state.workspace(&second).unwrap().chat;
        assert_eq!(first_chat.messages.len(), 1);
        assert!(first_chat.streaming_text.is_empty());
        assert_eq!(second_chat.messages.len(), 1);
        assert_eq!(second_chat.streaming_text, "from-b");
        assert!(
            matches!(first_chat.messages[0], ChatMessage::Text { ref text, .. } if text == "from-a")
        );
        assert!(
            matches!(second_chat.messages[0], ChatMessage::Tool { ref id, .. } if id == "tool-b")
        );
        assert_eq!(state.active_key(), Some(&second));
    }

    #[test]
    fn project_drafts_are_isolated_and_can_move_to_real_sessions() {
        let first = SessionKey::project_draft(PathBuf::from("/one"));
        let second = SessionKey::project_draft(PathBuf::from("/two"));
        assert!(first.is_draft());
        assert_ne!(first, second);

        let mut state = AppState::default();
        state.select(first.clone());
        state.workspace_mut(first.clone()).ui.draft = "one draft".into();
        state.select(second.clone());
        state.workspace_mut(second.clone()).ui.draft = "two draft".into();

        let real = SessionKey::new(PathBuf::from("/two"), "session-2");
        state.move_workspace(&second, real.clone());
        assert_eq!(state.workspace(&first).unwrap().ui.draft, "one draft");
        assert_eq!(state.workspace(&real).unwrap().ui.draft, "two draft");
        assert_eq!(state.active_key(), Some(&real));
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
