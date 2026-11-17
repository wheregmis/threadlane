//! Window-local visit order. Keys use daemon-owned paths verbatim.
use std::path::PathBuf;
use threadlane_protocol::daemon::SessionProjectionKey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct VisitKey {
    pub work_dir: PathBuf,
    pub session: SessionProjectionKey,
}

#[derive(Default)]
pub(super) struct Visits {
    pub recent: Vec<VisitKey>,
    active: Option<VisitKey>,
}
impl Visits {
    fn record(&mut self, active: Option<VisitKey>) {
        if self.active == active {
            return;
        }
        self.active = active.clone();
        if let Some(key) = active {
            self.recent.retain(|visit| visit != &key);
            self.recent.insert(0, key);
            self.recent.truncate(5);
        }
    }
}

use super::{SwitchSession, WorkspaceView};
use gpui::{prelude::*, *};
use gpui_component::{Disableable, IconName, WindowExt};
use std::sync::Arc;
use threadlane_client::DaemonClient;
use threadlane_ui_state::{actions::AppAction, controller, AppState, SessionInfo};

/// Holding the client prevents address reuse from masquerading as the same context.
pub(super) struct SessionNavigation {
    client: Arc<dyn DaemonClient>,
    visits: Visits,
    generation: u64,
}
pub(super) struct SessionPicker {
    generation: u64,
    epoch: u64,
    recent: Vec<VisitKey>,
    other: Vec<VisitKey>,
    error: Option<&'static str>,
}

fn inventory(state: &AppState) -> Vec<VisitKey> {
    state
        .projects
        .iter()
        .flat_map(|project| {
            project.sessions.iter().map(|session| VisitKey {
                work_dir: project.work_dir.clone(),
                session: SessionProjectionKey {
                    session_id: session.id.clone(),
                    session_file: session.session_file.clone(),
                },
            })
        })
        .collect()
}
pub(super) fn resolve<'a>(state: &'a AppState, key: &VisitKey) -> Option<(&'a str, &'a SessionInfo)> {
    if !state.session_inventory_available(&key.work_dir) {
        return None;
    }
    let project = state
        .projects
        .iter()
        .find(|project| project.work_dir == key.work_dir)?;
    // SelectSession resolves by ID. Reject ambiguous IDs rather than routing to a different file.
    let mut matches = project
        .sessions
        .iter()
        .filter(|session| session.id == key.session.session_id);
    let session = matches.next()?;
    (matches.next().is_none() && session.session_file == key.session.session_file)
        .then_some((project.name.as_str(), session))
}
fn ordered_groups(
    visits: &[VisitKey],
    inventory: &[VisitKey],
    active: Option<&VisitKey>,
) -> (Vec<VisitKey>, Vec<VisitKey>) {
    let recent: Vec<_> = visits
        .iter()
        .filter(|key| Some(*key) != active && inventory.contains(key))
        .cloned()
        .collect();
    let other = inventory
        .iter()
        .filter(|key| Some(*key) != active && !recent.contains(key))
        .cloned()
        .collect();
    (recent, other)
}
impl SessionNavigation {
    pub(super) fn new(state: &AppState) -> Self {
        let mut navigation = Self {
            client: state.daemon_client.clone(),
            visits: Visits::default(),
            generation: 0,
        };
        navigation.observe(state);
        navigation
    }
    pub(super) fn observe(&mut self, state: &AppState) {
        if !Arc::ptr_eq(&self.client, &state.daemon_client) {
            self.client = state.daemon_client.clone();
            self.visits = Visits::default();
            self.generation = self.generation.wrapping_add(1);
        }
        // Disconnect is not an inventory deletion or a visit to a different session.
        if !state.daemon_client.is_connected() {
            return;
        }
        let active = if state.is_new_task {
            None
        } else {
            state
                .active_work_dir
                .clone()
                .zip(state.active_session_projection_key())
                .map(|(work_dir, session)| VisitKey { work_dir, session })
        };
        if (active.is_some() || state.is_new_task || state.active_session_id.is_none())
            && state
                .active_work_dir
                .as_ref()
                .is_none_or(|dir| state.session_inventory_available(dir))
        {
            self.visits.record(active);
        }
        // Remote reconnect can precede its metadata snapshot. Retain those few keys;
        // resolution excludes unavailable targets without destroying visit history.
        let available = inventory(state);
        self.visits.recent.retain(|key| {
            !state.session_inventory_available(&key.work_dir) || available.contains(key)
        });
    }
    fn snapshot(&mut self, state: &AppState) -> SessionPicker {
        self.generation = self.generation.wrapping_add(1);
        let (recent, other) = ordered_groups(
            &self.visits.recent,
            &inventory(state),
            self.visits.active.as_ref(),
        );
        SessionPicker {
            generation: self.generation,
            epoch: state.daemon_client.file_search_connection_epoch(),
            recent,
            other,
            error: None,
        }
    }
    fn valid_context(&self, picker: &SessionPicker, state: &AppState) -> bool {
        Arc::ptr_eq(&self.client, &state.daemon_client)
            && picker.generation == self.generation
            && picker.epoch == state.daemon_client.file_search_connection_epoch()
            && state.daemon_client.is_connected()
    }
}

impl WorkspaceView {
    pub(super) fn switch_session_action(
        &mut self,
        _: &SwitchSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) || window.has_active_sheet(cx) {
            return;
        }
        let state = self.model.read(cx);
        if self.session_picker.is_some() {
            return;
        }
        self.session_navigation.observe(state);
        let picker = self.session_navigation.snapshot(state);
        self.exit_conversation_search(window, cx);
        if !self.command_palette_open {
            self.command_palette_previous_focus = window.focused(cx);
        }
        self.session_picker = Some(picker);
        self.command_palette_open = true;
        // A fresh interaction state cannot inherit a commands-list row selection.
        self.command_state = cx.new(|cx| gpui_component::command::CommandState::new(window, cx));
        self.command_state_subscription = cx.observe(&self.command_state, |_, _, cx| cx.notify());
        self.command_state
            .update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn confirm_session_picker(
        &mut self,
        key: &VisitKey,
        generation: u64,
        epoch: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.session_picker.as_ref() else {
            return;
        };
        if window.has_active_dialog(cx) || window.has_active_sheet(cx) {
            return;
        }
        let state = self.model.read(cx);
        let valid = picker.generation == generation
            && picker.epoch == epoch
            && self.session_navigation.valid_context(picker, state)
            && resolve(state, key).is_some();
        if !valid {
            self.session_navigation.observe(state);
            let mut refreshed = self.session_navigation.snapshot(state);
            refreshed.error = Some("This session is no longer available");
            self.session_picker = Some(refreshed);
            cx.notify();
            return;
        }
        self.close_command_palette(window, cx);
        self.model.update(cx, |state, cx| {
            controller::dispatch(
                state,
                AppAction::SelectSession {
                    work_dir: key.work_dir.clone(),
                    session_id: key.session.session_id.clone(),
                },
            );
            cx.notify();
        });
        self.chat_list.update(cx, |chat, cx| {
            chat.set_tab(threadlane_ui_chat::CentralTab::Chat, cx)
        });
        self.focus_composer_action(&super::FocusComposer, window, cx);
        cx.notify();
    }

    pub(super) fn render_session_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let picker = self.session_picker.as_ref().expect("session picker mode");
        let state = self.model.read(cx);
        let ready = self.session_navigation.valid_context(picker, state);
        let query = !self.command_state.read(cx).query(cx).trim().is_empty();
        let groups = [&picker.recent, &picker.other].map(|keys| {
            keys.iter()
                .map(|key| {
                    if let Some((project, session)) = resolve(state, key) {
                        threadlane_ui_kit::palette_session_item(
                            &session.title,
                            project,
                            session.git_branch.as_deref(),
                            &session.id,
                        )
                        .disabled(!ready)
                    } else {
                        threadlane_ui_kit::palette_item(
                            key.session.session_id.clone(),
                            if state.session_inventory_available(&key.work_dir) {
                                "This session is no longer available"
                            } else {
                                "Session metadata is unavailable"
                            },
                            IconName::SquareTerminal,
                        )
                        .disabled(!ready || !state.session_inventory_available(&key.work_dir))
                    }
                })
                .collect::<Vec<_>>()
        });
        let [recent, other] = groups;
        let stale = picker
            .recent
            .iter()
            .chain(&picker.other)
            .any(|key| resolve(state, key).is_none());
        let footer = picker.error.unwrap_or(if !ready {
            "Session metadata is unavailable. Reopen after reconnecting."
        } else if stale {
            "This session is no longer available"
        } else if picker.recent.is_empty() {
            "Sessions you open will appear here"
        } else {
            "↑↓ to choose · Enter to switch · Esc to cancel"
        });
        let keys = [picker.recent.clone(), picker.other.clone()];
        let (generation, epoch) = (picker.generation, picker.epoch);
        let confirm = cx.weak_entity();
        let cancel = cx.weak_entity();
        let backdrop = cx.weak_entity();
        let query_view = cx.weak_entity();
        let clear_view = cx.weak_entity();
        let command = threadlane_ui_kit::session_switcher_command(
            &self.command_state,
            recent,
            other,
            query,
            footer,
        )
        .on_query(move |_, _, cx| {
            let _ = query_view.update(cx, |_, cx| cx.notify());
        })
        .empty(move |_, _, cx| {
            let clear = clear_view.clone();
            threadlane_ui_kit::session_switcher_empty(
                query,
                move |window, cx| {
                    let _ = clear.update(cx, |this, cx| {
                        this.command_state
                            .update(cx, |state, cx| state.set_query("", window, cx));
                        cx.notify();
                    });
                },
                cx,
            )
        })
        .on_confirm(move |index, window, cx| {
            if let Some(key) = keys
                .get(index.section)
                .and_then(|group| group.get(index.row))
            {
                let _ = confirm.update(cx, |this, cx| {
                    this.confirm_session_picker(key, generation, epoch, window, cx)
                });
            }
        })
        .on_cancel(move |window, cx| {
            let _ = cancel.update(cx, |this, cx| {
                this.close_command_palette(window, cx);
                cx.notify();
            });
        });
        threadlane_ui_kit::session_switcher_frame(
            command,
            move |window, cx| {
                let _ = backdrop.update(cx, |this, cx| {
                    this.close_command_palette(window, cx);
                    cx.notify();
                });
            },
            cx,
        )
        .into_any_element()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{VisitKey, Visits};
    fn key(id: &str) -> VisitKey {
        VisitKey {
            work_dir: "/project".into(),
            session: threadlane_protocol::daemon::SessionProjectionKey {
                session_id: id.into(),
                session_file: format!("/sessions/{id}.jsonl").into(),
            },
        }
    }
    #[test]
    fn visits_follow_transitions_not_notifications_and_cap_at_five() {
        let mut visits = Visits::default();
        for id in ["a", "b", "c", "c"] {
            visits.record(Some(key(id)));
        }
        assert_eq!(visits.recent, vec![key("c"), key("b"), key("a")]);
        visits.record(Some(key("b")));
        assert_eq!(visits.recent, vec![key("b"), key("c"), key("a")]);
        for id in ["d", "e", "f"] {
            visits.record(Some(key(id)));
        }
        assert_eq!(visits.recent.len(), 5);
        visits.record(None);
        assert_eq!(visits.recent[0], key("f"));
    }
    pub(crate) fn state() -> threadlane_ui_state::AppState {
        use threadlane_protocol::daemon::{
            ProjectInfo, SessionCompletionSummary, SessionHealth, SessionInfo,
        };
        let mut state = threadlane_ui_state::AppState::for_tests();
        state.projects = vec![ProjectInfo {
            name: "Project".into(),
            work_dir: "/project".into(),
            is_expanded: true,
            sessions: ["a", "b", "c", "d"]
                .map(|id| SessionInfo {
                    id: id.into(),
                    title: id.into(),
                    work_dir: "/project".into(),
                    runtime_work_dir: "/project".into(),
                    session_file: key(id).session.session_file,
                    updated_at: 0,
                    health: SessionHealth::Healthy,
                    git_branch: Some("main".into()),
                    github_issue: None,
                    is_worktree: false,
                    worktree_available: true,
                    completion_summary: SessionCompletionSummary::Unknown,
                })
                .to_vec(),
        }];
        state.active_work_dir = Some("/project".into());
        state.is_new_task = false;
        state
    }
    #[test]
    fn snapshot_prefers_previous_visit_and_freezes_inventory_order() {
        let mut state = state();
        let mut navigation = super::SessionNavigation::new(&state);
        for id in ["a", "b", "c"] {
            state.active_session_id = Some(id.into());
            navigation.observe(&state);
        }
        let snapshot = navigation.snapshot(&state);
        assert_eq!(snapshot.recent, vec![key("b"), key("a")]);
        assert_eq!(snapshot.other, vec![key("d")]);
        state.projects[0].sessions.reverse();
        state.projects[0].sessions[0].title = "Changed by streaming".into();
        navigation.observe(&state);
        assert_eq!(snapshot.recent, vec![key("b"), key("a")]);
        state.active_session_id = Some("b".into());
        navigation.observe(&state);
        assert_eq!(navigation.snapshot(&state).recent[0], key("c"));
    }
    #[test]
    fn identity_rejects_replaced_files_ambiguous_ids_and_detach() {
        let mut state = state();
        assert!(super::resolve(&state, &key("a")).is_some());
        state.projects[0].sessions[0].session_file = "/elsewhere/a.jsonl".into();
        assert!(super::resolve(&state, &key("a")).is_none());
        let duplicate = state.projects[0].sessions[1].clone();
        state.projects[0].sessions.push(duplicate);
        assert!(super::resolve(&state, &key("b")).is_none());
        let mut other_project = key("c");
        other_project.work_dir = "/other".into();
        assert!(super::resolve(&state, &other_project).is_none());
        state.projects.clear();
        assert!(super::resolve(&state, &key("c")).is_none());
    }
    #[test]
    fn daemon_replacement_invalidates_open_snapshot_and_resets_visits() {
        let mut state = state();
        state.active_session_id = Some("a".into());
        let mut navigation = super::SessionNavigation::new(&state);
        let snapshot = navigation.snapshot(&state);
        assert!(navigation.valid_context(&snapshot, &state));
        state.daemon_client = threadlane_client::LocalDaemon::new(state.daemon_core.clone());
        assert!(!navigation.valid_context(&snapshot, &state));
        state.active_session_id = Some("b".into());
        navigation.observe(&state);
        assert!(!navigation.valid_context(&snapshot, &state));
        assert_eq!(navigation.visits.recent, vec![key("b")]);
    }
    #[test]
    fn remote_waits_for_metadata_and_prunes_only_authoritative_removal() {
        let mut state = state();
        state.active_session_id = Some("a".into());
        let mut navigation = super::SessionNavigation::new(&state);
        state.daemon_remote = true;
        assert!(super::resolve(&state, &key("a")).is_none());
        navigation.observe(&state);
        assert_eq!(navigation.visits.recent, vec![key("a")]);
        let project = state.projects[0].clone();
        state.drain_chat_stream(vec![
            threadlane_protocol::daemon::SessionEvent::ProjectChanged { project },
        ]);
        assert!(super::resolve(&state, &key("a")).is_some());
        let mut project = state.projects[0].clone();
        project.sessions.retain(|session| session.id != "a");
        state.drain_chat_stream(vec![
            threadlane_protocol::daemon::SessionEvent::ProjectChanged { project },
        ]);
        navigation.observe(&state);
        assert!(navigation.visits.recent.is_empty());
        assert!(super::resolve(&state, &key("a")).is_none());
    }
    #[test]
    fn draft_and_settings_do_not_add_visits() {
        let mut state = state();
        state.active_session_id = Some("a".into());
        let mut navigation = super::SessionNavigation::new(&state);
        state.workspace_page = threadlane_ui_state::WorkspacePage::Settings;
        navigation.observe(&state);
        assert_eq!(navigation.visits.recent, vec![key("a")]);
        state.is_new_task = true;
        state.active_session_id = Some("draft".into());
        navigation.observe(&state);
        assert_eq!(navigation.visits.recent, vec![key("a")]);
    }
}

#[cfg(test)]
#[path = "session_switcher_tests.rs"]
pub(crate) mod interaction_tests;
