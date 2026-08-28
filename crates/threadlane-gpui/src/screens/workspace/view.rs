use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::resizable::{h_resizable, resizable_panel, v_resizable, ResizableState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::status_bar::StatusBar;
use gpui_component::{v_flex, ActiveTheme, Icon, IconName, Root, Selectable, Sizable};

actions!(
    threadlane_workspace,
    [
        ToggleCommandPalette,
        ToggleSidebar,
        ToggleRightPanel,
        ToggleTerminal,
        BeginNewTask,
        OpenSettings,
        CancelActiveGeneration,
        SelectChatTab,
        SelectTrajectoryTab,
        SelectEditorTab,
        FocusComposer,
    ]
);
use threadlane_protocol::git::GitStatus;

use crate::app::actions::AppAction;
use crate::app::controller;
use crate::screens::chat::ChatListView;
use crate::screens::right_panel::RightPanelView;
use crate::screens::settings::SettingsView;
use crate::screens::sidebar::SidebarView;
use crate::screens::terminal::TerminalView;
use crate::services::sessions::SessionRuntime;
use crate::services::updater::{self, UpdaterEvent};
use crate::state::{
    runtime_status_text, AppState, SessionHydrationRequest, SessionInfo, SessionProjectionResult,
    WorkspacePage,
};
use threadlane_protocol::UpdateStatus;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", ToggleCommandPalette, None),
        KeyBinding::new("ctrl-k", ToggleCommandPalette, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-b", ToggleSidebar, None),
        KeyBinding::new("cmd-r", ToggleRightPanel, None),
        KeyBinding::new("ctrl-r", ToggleRightPanel, None),
        KeyBinding::new("cmd-j", ToggleTerminal, None),
        KeyBinding::new("ctrl-j", ToggleTerminal, None),
        KeyBinding::new("cmd-n", BeginNewTask, None),
        KeyBinding::new("ctrl-n", BeginNewTask, None),
        KeyBinding::new("cmd-1", SelectChatTab, None),
        KeyBinding::new("ctrl-1", SelectChatTab, None),
        KeyBinding::new("cmd-2", SelectTrajectoryTab, None),
        KeyBinding::new("ctrl-2", SelectTrajectoryTab, None),
        KeyBinding::new("cmd-3", SelectEditorTab, None),
        KeyBinding::new("ctrl-3", SelectEditorTab, None),
        KeyBinding::new("cmd-l", FocusComposer, None),
        KeyBinding::new("ctrl-l", FocusComposer, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("escape", CancelActiveGeneration, None),
        KeyBinding::new("cmd-s", crate::screens::editor::SaveFile, None),
        KeyBinding::new("ctrl-s", crate::screens::editor::SaveFile, None),
    ]);
}

enum GitEvent {
    Loaded {
        work_dir: PathBuf,
        result: Result<GitStatus, String>,
    },
    PrLoaded {
        work_dir: PathBuf,
        branch: String,
        result: Result<Option<threadlane_protocol::git::GitHubPrInfo>, String>,
    },
}

enum WorkspacePumpEvent {
    Git(GitEvent),
    Updater(UpdaterEvent),
    Sessions(PathBuf, Vec<SessionInfo>),
    Model,
}

async fn next_workspace_event(
    git_rx: &mut tokio::sync::mpsc::UnboundedReceiver<GitEvent>,
    updater_rx: &mut tokio::sync::mpsc::UnboundedReceiver<UpdaterEvent>,
    sessions_rx: &mut tokio::sync::mpsc::UnboundedReceiver<(PathBuf, Vec<SessionInfo>)>,
    model_rx: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
) -> Option<WorkspacePumpEvent> {
    tokio::select! {
        event = git_rx.recv() => event.map(WorkspacePumpEvent::Git),
        event = updater_rx.recv() => event.map(WorkspacePumpEvent::Updater),
        event = sessions_rx.recv() => event.map(|(work_dir, sessions)| WorkspacePumpEvent::Sessions(work_dir, sessions)),
        event = model_rx.recv() => event.map(|()| WorkspacePumpEvent::Model),
    }
}

struct TerminalGroup {
    tabs: Vec<Entity<TerminalView>>,
    active_tab: usize,
}

fn git_result_matches_active(requested: &Path, active: &Path) -> bool {
    requested == active
}

fn active_project_git_status<'a>(
    active_work_dir: Option<&Path>,
    statuses: &'a HashMap<PathBuf, GitStatus>,
) -> Option<&'a GitStatus> {
    active_work_dir.and_then(|work_dir| statuses.get(work_dir))
}

fn session_pr_target_is_active(
    targets: &HashSet<(PathBuf, String)>,
    target: &(PathBuf, String),
) -> bool {
    targets.contains(target)
}

pub struct WorkspaceView {
    model: Entity<AppState>,
    sidebar: Entity<SidebarView>,
    chat_list: Entity<ChatListView>,
    settings: Entity<SettingsView>,
    right_panel: Entity<RightPanelView>,
    fallback_terminal: Option<Entity<TerminalView>>,
    terminal_groups: HashMap<PathBuf, TerminalGroup>,
    sidebar_collapsed: bool,
    right_panel_visible: bool,
    bottom_panel_visible: bool,
    command_palette_open: bool,
    command_state: Entity<CommandState>,
    project_picker_open: bool,
    project_picker_current_path: Option<String>,
    project_picker_display_path: String,
    project_picker_parent_path: Option<String>,
    project_picker_entries: Vec<threadlane_protocol::project::HostDirectoryInfo>,
    project_picker_selected_index: usize,
    project_picker_scroll_handle: ScrollHandle,
    project_picker_focus: FocusHandle,
    last_git_work_dir: Option<PathBuf>,
    last_git_pr_targets: HashSet<(PathBuf, String)>,
    sidebar_resizable_state: Entity<ResizableState>,
    right_panel_resizable_state: Entity<ResizableState>,
    bottom_panel_resizable_state: Entity<ResizableState>,
    git_event_tx: tokio::sync::mpsc::UnboundedSender<GitEvent>,
    updater_tx: tokio::sync::mpsc::UnboundedSender<UpdaterEvent>,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    pub(crate) fn spawn_session_hydration(
        model: Entity<AppState>,
        request: SessionHydrationRequest,
        cx: &mut AsyncApp,
    ) {
        cx.spawn(async move |cx| {
            let runtime_task = request
                .runtime_options
                .clone()
                .map(|(work_dir, model, _roles)| {
                    let session_id = request.session_id.clone();
                    let session_file = request.session_file.clone();
                    cx.background_executor().spawn(async move {
                        let mut rt = SessionRuntime::new(session_id, work_dir, session_file);
                        rt.selected_model = model;
                        std::sync::Arc::new(rt)
                    })
                });
            let session_id = request.session_id.clone();
            let session_file = request.session_file.clone();
            let session_file_str = session_file.to_string_lossy().to_string();
            let hydration_result = cx
                .background_executor()
                .spawn(async move {
                    let client = crate::services::daemon_client::get_daemon_client().await?;
                    client
                        .hydrate_session(threadlane_protocol::session::HydrateSessionRequest {
                            session_id,
                            session_file: session_file_str,
                            project_path: None,
                        })
                        .await
                })
                .await;

            let runtime = match runtime_task {
                Some(task) => Some(task.await),
                None => None,
            };
            let _ = model.update(cx, |state, cx| {
                if !state.active_session_matches(&request.session_id, &request.session_file) {
                    return;
                }
                match hydration_result {
                    Ok(resp) => {
                        if request.reload_messages {
                            state.apply_session_messages(
                                &request.session_id,
                                &request.session_file,
                                resp.messages.clone(),
                            );
                        }
                        let proj_result = SessionProjectionResult::from(resp);
                        state.apply_session_hydration(
                            &request.session_id,
                            &request.session_file,
                            proj_result,
                        );
                        state.session_status = state.session_status_for_file(&request.session_file);
                    }
                    Err(error) => {
                        state.session_status = Some(format!("Could not load session: {error}"));
                    }
                }
                if let Some(runtime) = runtime {
                    let runtime = state
                        .session_runtimes
                        .entry(request.session_file.clone())
                        .or_insert(runtime);
                    state.is_generating = runtime.is_generating();
                    state.selected_model = runtime.selected_model.clone();
                    if let Some(status) = runtime_status_text(runtime.status()) {
                        state.session_status = Some(status);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn build(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let model = cx.new(|_cx| AppState::loading());
        let daemon_model = model.clone();
        cx.spawn(async move |cx| {
            let result = async {
                let client = crate::services::daemon_client::get_daemon_client().await?;
                let projects = client.list_projects().await?.projects;
                let mut sessions = Vec::with_capacity(projects.len());
                for project in &projects {
                    sessions.push((
                        project.path.clone(),
                        client.list_session_infos(&project.path).await?,
                    ));
                }
                let models = client.list_models().await?.models;
                Ok::<_, String>((projects, sessions, models))
            }
            .await;

            let _ = daemon_model.update(cx, |state, cx| {
                match result {
                    Ok((projects, sessions, models)) => {
                        state.apply_daemon_snapshot(projects, sessions, models);
                        state.session_status = None;
                    }
                    Err(error) => {
                        state.session_status = Some(format!("Daemon unavailable: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        let sidebar = cx.new(|cx| SidebarView::new(model.clone(), window, cx));
        let chat_list = cx.new(|cx| ChatListView::new(model.clone(), window, cx));
        let settings = cx.new(|cx| SettingsView::new(model.clone(), window, cx));
        let right_panel = cx.new(|cx| RightPanelView::new(model.clone(), window, cx));
        let sidebar_resizable_state = cx.new(|_cx| ResizableState::default());
        let right_panel_resizable_state = cx.new(|_cx| ResizableState::default());
        let bottom_panel_resizable_state = cx.new(|_cx| ResizableState::default());
        let command_state = cx.new(|cx| CommandState::new(window, cx));
        let (git_event_tx, mut git_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (updater_tx, mut updater_rx) = tokio::sync::mpsc::unbounded_channel();
        let (model_wake_tx, mut model_wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session_refresh_rx = model
            .update(cx, |state, _cx| state.session_refresh_rx.take())
            .expect("session refresh receiver was already taken");
        let _ = model_wake_tx.send(());

        #[cfg(target_os = "macos")]
        if updater::is_configured() {
            updater::check(updater_tx.clone());
        }

        let model_clone = model.clone();
        let view = cx.new(|cx| {
            let sub = cx.observe(&model_clone, move |this: &mut Self, model, cx| {
                this.sync_git_status_with_active_project(cx);
                if let Some(cmd) =
                    model.update(cx, |state, _cx| state.requested_terminal_command.take())
                {
                    this.bottom_panel_visible = true;
                    let term = if let Some(work_dir) = model.read(cx).active_work_dir.clone() {
                        this.get_or_create_active_terminal(&work_dir, cx)
                    } else {
                        this.fallback_terminal(cx)
                    };
                    term.update(cx, |term, _cx| {
                        let trimmed = cmd.trim_end();
                        term.send_input(&format!("{trimmed}\n"));
                    });
                }
                if model.update(cx, |state, _cx| {
                    if state.requested_project_picker {
                        state.requested_project_picker = false;
                        true
                    } else {
                        false
                    }
                }) {
                    this.open_project_picker(None, cx);
                }
                let _ = model_wake_tx.send(());
                cx.notify();
            });

            cx.spawn(async move |this, cx| {
                while let Some(event) = next_workspace_event(
                    &mut git_event_rx,
                    &mut updater_rx,
                    &mut session_refresh_rx,
                    &mut model_wake_rx,
                )
                .await
                {
                    let mut git_events = Vec::new();
                    let mut updater_events = Vec::new();
                    let mut session_refreshes = Vec::new();
                    match event {
                        WorkspacePumpEvent::Git(event) => git_events.push(event),
                        WorkspacePumpEvent::Updater(event) => updater_events.push(event),
                        WorkspacePumpEvent::Sessions(work_dir, sessions) => {
                            session_refreshes.push((work_dir, sessions));
                        }
                        WorkspacePumpEvent::Model => {}
                    }
                    git_events.extend(std::iter::from_fn(|| git_event_rx.try_recv().ok()));
                    updater_events.extend(std::iter::from_fn(|| updater_rx.try_recv().ok()));
                    session_refreshes
                        .extend(std::iter::from_fn(|| session_refresh_rx.try_recv().ok()));
                    while model_wake_rx.try_recv().is_ok() {}
                    let hydration_requests = this
                        .update(cx, |this, cx| {
                            this.model.update(cx, |state, _cx| {
                                std::mem::take(&mut state.pending_hydrations)
                            })
                        })
                        .unwrap_or_default();
                    for request in hydration_requests {
                        let model = this.update(cx, |this, _cx| this.model.clone()).ok();
                        if let Some(model) = model {
                            Self::spawn_session_hydration(model, request, cx);
                        }
                    }
                    let has_events = !git_events.is_empty()
                        || !updater_events.is_empty()
                        || !session_refreshes.is_empty();
                    let _ = this.update(cx, |this, cx| {
                        let mut changed = has_events;
                        this.model.update(cx, |state, cx| {
                            for (work_dir, sessions) in session_refreshes {
                                changed |= state.apply_session_refresh(work_dir, sessions);
                            }
                            if changed {
                                cx.notify();
                            }
                        });
                        for event in git_events {
                            this.apply_git_event(event, cx);
                        }
                        for UpdaterEvent::Status(status) in updater_events {
                            this.model.update(cx, |state, cx| {
                                state.update_status = status;
                                state.update_notice_dismissed = false;
                                cx.notify();
                            });
                        }
                        if changed {
                            cx.notify();
                        }
                    });
                }
            })
            .detach();

            let right_panel_sub = cx.observe(&right_panel, |_this: &mut Self, _panel, cx| {
                cx.notify();
            });

            Self {
                model,
                sidebar,
                chat_list,
                settings,
                right_panel,
                fallback_terminal: None,
                terminal_groups: HashMap::new(),
                sidebar_collapsed: false,
                right_panel_visible: false,
                bottom_panel_visible: false,
                command_palette_open: false,
                command_state,
                project_picker_open: false,
                project_picker_current_path: None,
                project_picker_display_path: String::new(),
                project_picker_parent_path: None,
                project_picker_entries: Vec::new(),
                project_picker_selected_index: 0,
                project_picker_scroll_handle: ScrollHandle::new(),
                project_picker_focus: cx.focus_handle(),
                last_git_work_dir: None,
                last_git_pr_targets: HashSet::new(),
                sidebar_resizable_state,
                right_panel_resizable_state,
                bottom_panel_resizable_state,
                git_event_tx,
                updater_tx,
                _subscriptions: vec![sub, right_panel_sub],
            }
        });

        view.update(cx, |view, cx| {
            let hydration_requests = view.model.update(cx, |state, _cx| {
                std::mem::take(&mut state.pending_hydrations)
            });
            if !hydration_requests.is_empty() {
                let model = view.model.clone();
                cx.spawn(async move |_view, cx| {
                    for request in hydration_requests {
                        Self::spawn_session_hydration(model.clone(), request, cx);
                    }
                })
                .detach();
            }
            view.sync_git_status_with_active_project(cx);
        });

        let view_handle = view.downgrade();
        let shortcut_subscription = cx.intercept_keystrokes(move |event, window, cx| {
            let keystroke = &event.keystroke;
            if keystroke.key.eq_ignore_ascii_case("k")
                && (keystroke.modifiers.platform || keystroke.modifiers.control)
                && !keystroke.modifiers.alt
                && !keystroke.modifiers.shift
            {
                if let Some(view) = view_handle.upgrade() {
                    view.update(cx, |view, cx| {
                        view.toggle_command_palette(&ToggleCommandPalette, window, cx);
                    });
                    cx.stop_propagation();
                }
            }
        });
        view.update(cx, |view, _cx| {
            view._subscriptions.push(shortcut_subscription);
        });
        view
    }

    fn open_git_review(&mut self, cx: &mut Context<Self>) {
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_review(cx);
        });
        self.refresh_git_status(cx);
        cx.notify();
    }

    fn open_git_branches(&mut self, cx: &mut Context<Self>) {
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_branch_popover(cx);
        });
        self.refresh_git_status(cx);
        cx.notify();
    }

    fn open_git_new_branch(&mut self, cx: &mut Context<Self>) {
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_new_branch_dialog(cx);
        });
        self.refresh_git_status(cx);
        cx.notify();
    }

    fn open_git_merge(&mut self, cx: &mut Context<Self>) {
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_merge_dialog(cx);
        });
        self.refresh_git_status(cx);
        cx.notify();
    }

    fn get_or_create_active_terminal(
        &mut self,
        project: &PathBuf,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let group = self.get_or_create_terminal_group(project, cx);
        group.tabs[group.active_tab].clone()
    }

    fn get_or_create_terminal_group(
        &mut self,
        project: &PathBuf,
        cx: &mut Context<Self>,
    ) -> &mut TerminalGroup {
        let group = self
            .terminal_groups
            .entry(project.clone())
            .or_insert_with(|| TerminalGroup {
                tabs: vec![cx.new(|cx| TerminalView::new(project.clone(), cx))],
                active_tab: 0,
            });
        if group.tabs.is_empty() {
            group
                .tabs
                .push(cx.new(|cx| TerminalView::new(project.clone(), cx)));
            group.active_tab = 0;
        }
        group.active_tab = group.active_tab.min(group.tabs.len().saturating_sub(1));
        group
    }

    fn add_terminal_tab(&mut self, project: PathBuf, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new(project.clone(), cx));
        let group = self
            .terminal_groups
            .entry(project)
            .or_insert(TerminalGroup {
                tabs: Vec::new(),
                active_tab: 0,
            });
        group.tabs.push(terminal);
        group.active_tab = group.tabs.len() - 1;
        cx.notify();
    }

    fn fallback_terminal(&mut self, cx: &mut Context<Self>) -> Entity<TerminalView> {
        self.fallback_terminal
            .get_or_insert_with(|| {
                let project =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                cx.new(|cx| TerminalView::new(project, cx))
            })
            .clone()
    }

    fn select_terminal_tab(&mut self, project: &PathBuf, tab: usize, cx: &mut Context<Self>) {
        if let Some(group) = self.terminal_groups.get_mut(project) {
            group.active_tab = tab.min(group.tabs.len().saturating_sub(1));
            cx.notify();
        }
    }

    fn close_terminal_tab(&mut self, project: &PathBuf, tab: usize, cx: &mut Context<Self>) {
        if let Some(group) = self.terminal_groups.get_mut(project) {
            if tab >= group.tabs.len() {
                return;
            }
            if group.tabs.len() > 1 {
                let terminal = group.tabs.remove(tab);
                terminal.update(cx, |terminal, cx| terminal.close(cx));
                if tab < group.active_tab {
                    group.active_tab -= 1;
                } else if tab == group.active_tab {
                    group.active_tab = group.active_tab.min(group.tabs.len() - 1);
                }
            } else {
                let terminal = group.tabs.pop().expect("terminal tab exists");
                terminal.update(cx, |terminal, cx| terminal.close(cx));
                group.tabs = vec![cx.new(|cx| TerminalView::new(project.clone(), cx))];
                group.active_tab = 0;
                self.bottom_panel_visible = false;
            }
            cx.notify();
        }
    }

    fn close_other_terminal_tabs(
        &mut self,
        project: &PathBuf,
        keep_tab: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(group) = self.terminal_groups.get_mut(project) {
            if keep_tab < group.tabs.len() && group.tabs.len() > 1 {
                let keep_elem = group.tabs.remove(keep_tab);
                let removed = std::mem::replace(&mut group.tabs, vec![keep_elem]);
                for terminal in removed {
                    terminal.update(cx, |terminal, cx| terminal.close(cx));
                }
                group.active_tab = 0;
                cx.notify();
            }
        }
    }

    fn toggle_command_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_palette_open = !self.command_palette_open;
        if self.command_palette_open {
            self.command_state.update(cx, |state, cx| {
                state.set_query("", window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    /// Executes a command-palette action key. This is the single source of truth
    /// for palette action dispatch, shared by keyboard activation and click.
    fn execute_palette_action(
        &mut self,
        action_key: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model = self.model.clone();
        match action_key {
            "new" => {
                model.update(cx, |state, _cx| {
                    controller::dispatch(state, AppAction::BeginNewTask);
                });
            }
            "attach" => {
                self.open_project_picker(Some(window), cx);
            }
            "git" => self.open_git_review(cx),
            "git_branch" => self.open_git_branches(cx),
            "git_new_branch" => self.open_git_new_branch(cx),
            "git_merge" => self.open_git_merge(cx),
            "git_stash_pop" => {
                self.open_git_review(cx);
                self.right_panel.update(cx, |panel, cx| {
                    panel.restore_current_stash(window, cx);
                });
            }
            "git_pull" => {
                self.open_git_review(cx);
                self.right_panel.update(cx, |panel, cx| {
                    panel.run_git_action(crate::screens::right_panel::GitAction::Pull, window, cx);
                });
            }
            "settings" => {
                model.update(cx, |state, _cx| {
                    controller::dispatch(state, AppAction::OpenSettings);
                });
            }
            "sidebar" => {
                self.sidebar_collapsed = !self.sidebar_collapsed;
            }
            "panel" => {
                self.right_panel_visible = !self.right_panel_visible;
            }
            "goal" | "model" | "compact" => {
                let value = if action_key == "compact" {
                    "/compact".to_string()
                } else {
                    format!("/{action_key} ")
                };
                self.chat_list.update(cx, |chat, cx| {
                    chat.input_state.update(cx, |input, cx| {
                        input.set_value(value, window, cx);
                    });
                });
            }
            _ => {}
        }
        cx.notify();
    }

    fn sync_git_status_with_active_project(&mut self, cx: &App) {
        self.sync_session_prs(cx);
        let active_work_dir = self.model.read(cx).active_work_dir.clone();
        if self.last_git_work_dir == active_work_dir {
            return;
        }

        self.last_git_work_dir = active_work_dir.clone();

        if let Some(work_dir) = active_work_dir {
            self.spawn_git_status_refresh(work_dir);
        }
    }

    fn sync_session_prs(&mut self, cx: &App) {
        let targets = self
            .model
            .read(cx)
            .projects
            .iter()
            .flat_map(|project| {
                project.sessions.iter().filter_map(|session| {
                    session
                        .git_branch
                        .as_ref()
                        .map(|branch| (session.work_dir.clone(), branch.clone()))
                })
            })
            .collect::<HashSet<_>>();
        let new_targets = targets
            .difference(&self.last_git_pr_targets)
            .cloned()
            .collect::<Vec<_>>();
        self.last_git_pr_targets = targets;
        for (work_dir, branch) in new_targets {
            self.spawn_session_pr_refresh(work_dir, branch);
        }
    }

    fn spawn_session_pr_refresh(&self, work_dir: PathBuf, branch: String) {
        let tx = self.git_event_tx.clone();
        std::thread::spawn(move || {
            let result = crate::services::git::inspect_pr_for_branch(&work_dir, &branch)
                .map_err(|error| error.to_string());
            let _ = tx.send(GitEvent::PrLoaded {
                work_dir,
                branch,
                result,
            });
        });
    }

    fn schedule_session_pr_refresh(target: (PathBuf, String), cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(31))
                .await;
            let _ = this.update(cx, |this, _cx| {
                if session_pr_target_is_active(&this.last_git_pr_targets, &target) {
                    this.spawn_session_pr_refresh(target.0, target.1);
                }
            });
        })
        .detach();
    }

    fn refresh_git_status(&mut self, cx: &App) {
        let Some(work_dir) = self.model.read(cx).active_work_dir.clone() else {
            self.last_git_work_dir = None;
            return;
        };

        self.last_git_work_dir = Some(work_dir.clone());
        self.spawn_git_status_refresh(work_dir);
    }

    fn spawn_git_status_refresh(&self, work_dir: PathBuf) {
        let tx = self.git_event_tx.clone();
        std::thread::spawn(move || {
            // Refresh remote-tracking refs before calculating ahead/behind and PR state.
            // A failed fetch should not hide the local Git status (offline use is valid).
            let _ = crate::services::git::sync_remote(&work_dir);
            let result =
                crate::services::git::inspect(&work_dir).map_err(|error| error.to_string());
            let _ = tx.send(GitEvent::Loaded { work_dir, result });
        });
    }

    fn apply_git_event(&mut self, event: GitEvent, cx: &mut Context<Self>) {
        let (work_dir, result) = match event {
            GitEvent::PrLoaded {
                work_dir,
                branch,
                result,
            } => {
                let target = (work_dir.clone(), branch.clone());
                if let Ok(pr) = result {
                    self.model.update(cx, |state, cx| {
                        state.git_prs.insert((work_dir, branch), pr);
                        cx.notify();
                    });
                }
                Self::schedule_session_pr_refresh(target, cx);
                return;
            }
            GitEvent::Loaded { work_dir, result } => (work_dir, result),
        };
        let Some(active_work_dir) = self.model.read(cx).active_work_dir.clone() else {
            return;
        };
        if !git_result_matches_active(&work_dir, &active_work_dir) {
            return;
        }

        if let Ok(status) = &result {
            self.model.update(cx, |state, cx| {
                state.git_statuses.insert(work_dir.clone(), status.clone());
                if let Some(branch) = status.branch.as_ref() {
                    state
                        .git_prs
                        .insert((work_dir.clone(), branch.clone()), status.pr.clone());
                }
                cx.notify();
            });
        }

        cx.notify();
    }

    fn render_update_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let status = {
            let state = self.model.read(cx);
            if state.update_notice_dismissed {
                return None;
            }
            state.update_status.clone()
        };

        let (title, detail) = match &status {
            UpdateStatus::Available(info) => (
                format!("Threadlane {} is available", info.version),
                "Download the verified update in the background.".to_string(),
            ),
            UpdateStatus::Downloading { progress, .. } => (
                "Downloading update".to_string(),
                format!("{}% complete", (progress.clamp(0.0, 1.0) * 100.0).round()),
            ),
            UpdateStatus::ReadyToInstall { info, .. } => (
                format!("Threadlane {} is ready", info.version),
                "Install the update and relaunch Threadlane.".to_string(),
            ),
            UpdateStatus::Installing => (
                "Installing update".to_string(),
                "Threadlane will relaunch when installation finishes.".to_string(),
            ),
            UpdateStatus::Error(error) => {
                let truncated: String = error.chars().take(160).collect();
                let suffix = if error.chars().count() > 160 {
                    "…"
                } else {
                    ""
                };
                ("Update failed".to_string(), format!("{truncated}{suffix}"))
            }
            _ => return None,
        };

        let action = match &status {
            UpdateStatus::Available(info) => {
                let tx = self.updater_tx.clone();
                let info = info.clone();
                Some(
                    Button::new("update-download")
                        .label("Download")
                        .primary()
                        .on_click(move |_event, _window, _cx| {
                            updater::download(info.clone(), tx.clone());
                        }),
                )
            }
            UpdateStatus::ReadyToInstall { info, bytes } => {
                let tx = self.updater_tx.clone();
                let info = info.clone();
                let bytes = bytes.clone();
                Some(
                    Button::new("update-install")
                        .label("Install and relaunch")
                        .primary()
                        .on_click(move |_event, _window, _cx| {
                            updater::install(info.clone(), bytes.clone(), tx.clone());
                        }),
                )
            }
            UpdateStatus::Error(_) => {
                let tx = self.updater_tx.clone();
                Some(
                    Button::new("update-retry")
                        .label("Retry")
                        .outline()
                        .on_click(move |_event, _window, _cx| updater::check(tx.clone())),
                )
            }
            _ => None,
        };
        let theme = cx.theme().colors;
        let model = self.model.clone();

        Some(
            div()
                .absolute()
                .right(px(16.0))
                .bottom(px(16.0))
                .w(px(420.0))
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .p_4()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(title),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(detail),
                        ),
                )
                .children(action)
                .children(
                    matches!(status, UpdateStatus::Available(_) | UpdateStatus::Error(_)).then(
                        || {
                            Button::new("update-dismiss")
                                .icon(IconName::Close)
                                .tooltip("Dismiss")
                                .ghost()
                                .xsmall()
                                .on_click(move |_event, _window, cx| {
                                    model.update(cx, |state, cx| {
                                        state.update_notice_dismissed = true;
                                        cx.notify();
                                    });
                                })
                        },
                    ),
                )
                .into_any_element(),
        )
    }

    fn render_command_palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();
        let state = model.read(cx);

        let commands: [(&str, &str, &str, IconName, &[&str]); 14] = [
            (
                "New Task",
                "Start a fresh session",
                "new",
                IconName::Plus,
                &["task", "fresh", "session", "new"],
            ),
            (
                "Add Project",
                "Attach a project folder to your workspace",
                "attach",
                IconName::FolderOpen,
                &["folder", "workspace", "attach", "open", "project"],
            ),
            (
                "Goal Planning (/goal)",
                "Autonomous goal loop extension",
                "goal",
                IconName::Bot,
                &["goal", "planning", "loop", "agent", "autonomous"],
            ),
            (
                "Model Selection (/model)",
                "Switch model or provider",
                "model",
                IconName::Cpu,
                &["model", "llm", "switch", "provider", "select"],
            ),
            (
                "Compact History (/compact)",
                "Compact context conversation",
                "compact",
                IconName::Minimize,
                &["compact", "history", "context", "clean"],
            ),
            (
                "Git Review & Commit",
                "Review changed files and commit",
                "git",
                IconName::Github,
                &["git", "diff", "review", "commit", "stage"],
            ),
            (
                "Git: Switch Branch",
                "Switch or checkout a Git branch",
                "git_branch",
                IconName::Github,
                &["git", "branch", "switch", "checkout"],
            ),
            (
                "Git: New Branch",
                "Create a new branch from current HEAD",
                "git_new_branch",
                IconName::Plus,
                &["git", "branch", "new", "create"],
            ),
            (
                "Git: Merge Branch",
                "Merge another branch into current branch",
                "git_merge",
                IconName::Redo,
                &["git", "merge", "branch", "integrate"],
            ),
            (
                "Git: Restore Stashed Changes",
                "Restore changes previously stashed on this branch",
                "git_stash_pop",
                IconName::Undo2,
                &["git", "stash", "pop", "restore", "unstash"],
            ),
            (
                "Git: Pull Origin",
                "Pull latest commits from remote origin",
                "git_pull",
                IconName::Redo,
                &["git", "pull", "origin", "fetch", "sync"],
            ),
            (
                "Toggle Sidebar",
                "Show or hide your projects and tasks",
                "sidebar",
                IconName::PanelLeft,
                &["sidebar", "toggle", "hide", "show", "projects"],
            ),
            (
                "Toggle Right Panel",
                "Show review / files / terminal",
                "panel",
                IconName::PanelRight,
                &["panel", "right", "terminal", "review", "toggle"],
            ),
            (
                "Settings",
                "Configure API keys and providers",
                "settings",
                IconName::Settings,
                &["settings", "keys", "provider", "preferences", "config"],
            ),
        ];

        let mut commands_group = CommandGroup::new().label("Commands & Actions");
        for (name, desc, _action_key, icon, keywords) in &commands {
            let name_str = name.to_string();
            let desc_str = desc.to_string();
            let item = CommandItem::new()
                .label(*name)
                .icon(icon.clone())
                .keywords(keywords.iter().copied())
                .child(move |_window, cx| {
                    let colors = cx.theme().colors;
                    v_flex()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(name_str.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.muted_foreground)
                                .child(desc_str.clone()),
                        )
                });
            commands_group = commands_group.item(item);
        }

        let mut session_entries = Vec::new();
        let mut sessions_group = CommandGroup::new().label("Sessions");
        for project in &state.projects {
            for session in &project.sessions {
                session_entries.push((project.work_dir.clone(), session.id.clone()));
                let title = session.title.clone();
                let project_name = project.name.clone();
                let item = CommandItem::new()
                    .label(title.clone())
                    .icon(IconName::SquareTerminal)
                    .keywords([project.name.clone(), session.id.clone()])
                    .child(move |_window, cx| {
                        let colors = cx.theme().colors;
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_foreground)
                                    .child(project_name.clone()),
                            )
                    });
                sessions_group = sessions_group.item(item);
            }
        }

        let view = cx.weak_entity();
        let view_cancel = cx.weak_entity();

        div()
            .id("command-palette-backdrop")
            .absolute()
            .inset_0()
            .bg(theme.background.opacity(0.5))
            .flex()
            .items_start()
            .justify_center()
            .pt(px(80.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    this.command_palette_open = false;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("command-palette-modal")
                    .w(px(560.0))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_lg()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_event, _window, _cx| {})
                    .child(
                        Command::new(&self.command_state)
                            .bordered(false)
                            .placeholder("Type a command or search sessions…")
                            .max_h(px(420.0))
                            .group(commands_group)
                            .group(sessions_group)
                            .on_cancel(move |_window, cx| {
                                let _ = view_cancel.update(cx, |this, cx| {
                                    this.command_palette_open = false;
                                    cx.notify();
                                });
                            })
                            .on_confirm(move |index, window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.command_palette_open = false;
                                    if index.section == 0 {
                                        if let Some((_, _, action_key, _, _)) =
                                            commands.get(index.row)
                                        {
                                            this.execute_palette_action(action_key, window, cx);
                                        }
                                    } else if index.section == 1 {
                                        if let Some((work_dir, session_id)) =
                                            session_entries.get(index.row)
                                        {
                                            let work_dir = work_dir.clone();
                                            let session_id = session_id.clone();
                                            this.model.update(cx, |state, _cx| {
                                                controller::dispatch(
                                                    state,
                                                    AppAction::SelectSession {
                                                        work_dir,
                                                        session_id,
                                                    },
                                                );
                                            });
                                        }
                                    }
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn open_project_picker(
        &mut self,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.project_picker_open = true;
        self.command_palette_open = false;
        if let Some(window) = window {
            self.project_picker_focus.focus(window, cx);
        }
        self.browse_project_picker_dir(None, cx);
        cx.notify();
    }

    pub(crate) fn browse_project_picker_dir(
        &mut self,
        path: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let view = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            // First attempt daemon RPC
            let daemon_res = match crate::services::daemon_client::get_daemon_client().await {
                Ok(client) => client.browse_directories(path.as_deref()).await,
                Err(e) => Err(e),
            };

            let resp = match daemon_res {
                Ok(resp) => resp,
                Err(_) => {
                    // Fallback to direct host filesystem browse
                    let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
                    let target = match path.as_deref() {
                        Some(p) if !p.is_empty() => {
                            if p == "~" || p.starts_with("~/") {
                                if p == "~" {
                                    home_dir.clone()
                                } else {
                                    home_dir.join(&p[2..])
                                }
                            } else {
                                PathBuf::from(p)
                            }
                        }
                        _ => home_dir.clone(),
                    };

                    let canonical = target.canonicalize().unwrap_or(target);
                    let current_path = canonical.to_string_lossy().to_string();
                    let display_path = if let Ok(rel) = canonical.strip_prefix(&home_dir) {
                        if rel.as_os_str().is_empty() {
                            "~/".to_string()
                        } else {
                            format!("~/{}/", rel.to_string_lossy())
                        }
                    } else {
                        format!("{}/", current_path)
                    };
                    let parent_path = canonical.parent().map(|p| p.to_string_lossy().to_string());

                    let mut entries = Vec::new();
                    if let Ok(read_dir) = std::fs::read_dir(&canonical) {
                        for item in read_dir.flatten() {
                            let Ok(ft) = item.file_type() else { continue };
                            let is_dir = ft.is_dir() || (ft.is_symlink() && item.path().is_dir());
                            if !is_dir {
                                continue;
                            }
                            let name = item.file_name().to_string_lossy().to_string();
                            if name.starts_with('.') {
                                continue;
                            }
                            let p = item.path();
                            let is_git = p.join(".git").exists();
                            let is_project = p.join(".threadlane").exists();
                            entries.push(threadlane_protocol::project::HostDirectoryInfo {
                                name,
                                path: p.to_string_lossy().to_string(),
                                is_dir: true,
                                is_git,
                                is_project,
                            });
                        }
                    }
                    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

                    threadlane_protocol::project::BrowseDirectoriesResponse {
                        current_path,
                        display_path,
                        parent_path,
                        entries,
                    }
                }
            };

            let _ = view.update(cx, |this, cx| {
                this.project_picker_current_path = Some(resp.current_path);
                this.project_picker_display_path = resp.display_path;
                this.project_picker_parent_path = resp.parent_path;
                this.project_picker_entries = resp.entries;
                this.project_picker_selected_index = 0;
                this.project_picker_scroll_handle.scroll_to_item(0);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_project_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;

        let display_path = if self.project_picker_display_path.is_empty() {
            "~/".to_string()
        } else {
            self.project_picker_display_path.clone()
        };

        let has_parent = self.project_picker_parent_path.is_some();
        let entries = self.project_picker_entries.clone();
        let selected_index = self.project_picker_selected_index;

        div()
            .id("project-picker-backdrop")
            .track_focus(&self.project_picker_focus)
            .key_context("ProjectPicker")
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
                let has_parent = this.project_picker_parent_path.is_some();
                let entries_count = this.project_picker_entries.len();
                let max_index = if has_parent {
                    entries_count
                } else {
                    entries_count.saturating_sub(1)
                };

                match event.keystroke.key.as_str() {
                    "escape" => {
                        this.project_picker_open = false;
                        cx.notify();
                    }
                    "up" => {
                        if this.project_picker_selected_index > 0 {
                            this.project_picker_selected_index -= 1;
                            this.project_picker_scroll_handle
                                .scroll_to_item(this.project_picker_selected_index);
                            cx.notify();
                        }
                    }
                    "down" => {
                        if this.project_picker_selected_index < max_index {
                            this.project_picker_selected_index += 1;
                            this.project_picker_scroll_handle
                                .scroll_to_item(this.project_picker_selected_index);
                            cx.notify();
                        }
                    }
                    "backspace" => {
                        if let Some(parent) = this.project_picker_parent_path.clone() {
                            this.browse_project_picker_dir(Some(parent), cx);
                        }
                    }
                    "enter" => {
                        if event.keystroke.modifiers.platform || event.keystroke.modifiers.control {
                            // Cmd+Enter / Ctrl+Enter: Attach current or selected directory
                            let target = if has_parent && this.project_picker_selected_index > 0 {
                                this.project_picker_entries
                                    .get(this.project_picker_selected_index - 1)
                                    .map(|e| PathBuf::from(&e.path))
                            } else if !has_parent {
                                this.project_picker_entries
                                    .get(this.project_picker_selected_index)
                                    .map(|e| PathBuf::from(&e.path))
                            } else {
                                None
                            }
                            .or_else(|| {
                                this.project_picker_current_path.clone().map(PathBuf::from)
                            });

                            if let Some(target) = target {
                                this.project_picker_open = false;
                                let model = this.model.clone();
                                model.update(cx, |state, cx| {
                                    controller::dispatch(state, AppAction::AttachProject(target));
                                    cx.notify();
                                });
                                cx.notify();
                            }
                        } else {
                            // Enter: Navigate into selected directory
                            if has_parent && this.project_picker_selected_index == 0 {
                                if let Some(parent) = this.project_picker_parent_path.clone() {
                                    this.browse_project_picker_dir(Some(parent), cx);
                                }
                            } else {
                                let entry_idx = if has_parent {
                                    this.project_picker_selected_index - 1
                                } else {
                                    this.project_picker_selected_index
                                };
                                if let Some(entry) = this.project_picker_entries.get(entry_idx) {
                                    let entry_path = entry.path.clone();
                                    this.browse_project_picker_dir(Some(entry_path), cx);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }))
            .absolute()
            .inset_0()
            .bg(theme.background.opacity(0.5))
            .flex()
            .items_start()
            .justify_center()
            .pt(px(80.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    this.project_picker_open = false;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id("project-picker-modal")
                    .w(px(560.0))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .shadow_lg()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        // Header
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .children(has_parent.then(|| {
                                        Button::new("project-picker-back")
                                            .icon(IconName::ArrowLeft)
                                            .tooltip("Go up to parent directory (Backspace)")
                                            .ghost()
                                            .xsmall()
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                if let Some(parent) =
                                                    this.project_picker_parent_path.clone()
                                                {
                                                    this.browse_project_picker_dir(
                                                        Some(parent),
                                                        cx,
                                                    );
                                                }
                                            }))
                                    }))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(display_path),
                                    ),
                            )
                            .child(
                                div().flex().items_center().gap_2().child(
                                    Button::new("project-picker-add-btn")
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1p5()
                                                .child("Add Project")
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme.muted_foreground)
                                                        .child("Cmd+Enter"),
                                                ),
                                        )
                                        .small()
                                        .primary()
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            let has_parent =
                                                this.project_picker_parent_path.is_some();
                                            let target = if has_parent
                                                && this.project_picker_selected_index > 0
                                            {
                                                this.project_picker_entries
                                                    .get(this.project_picker_selected_index - 1)
                                                    .map(|e| PathBuf::from(&e.path))
                                            } else if !has_parent {
                                                this.project_picker_entries
                                                    .get(this.project_picker_selected_index)
                                                    .map(|e| PathBuf::from(&e.path))
                                            } else {
                                                None
                                            }
                                            .or_else(|| {
                                                this.project_picker_current_path
                                                    .clone()
                                                    .map(PathBuf::from)
                                            });

                                            if let Some(target) = target {
                                                this.project_picker_open = false;
                                                let model = this.model.clone();
                                                model.update(cx, |state, cx| {
                                                    controller::dispatch(
                                                        state,
                                                        AppAction::AttachProject(target),
                                                    );
                                                    cx.notify();
                                                });
                                                cx.notify();
                                            }
                                        })),
                                ),
                            ),
                    )
                    .child(
                        // Subtitle
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_4()
                            .pt_3()
                            .pb_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("Directories")
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{} items", entries.len())),
                            ),
                    )
                    .child(
                        // Directory list
                        div()
                            .id("project-picker-list")
                            .flex_col()
                            .relative()
                            .track_scroll(&self.project_picker_scroll_handle)
                            .overflow_y_scroll()
                            .vertical_scrollbar(&self.project_picker_scroll_handle)
                            .max_h(px(340.0))
                            .px_2()
                            .py_1()
                            // Parent directory row
                            .children(has_parent.then(|| {
                                let is_selected = selected_index == 0;
                                let parent_path = self.project_picker_parent_path.clone();

                                div()
                                    .id("dir-item-parent")
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(if is_selected {
                                        theme.selection
                                    } else {
                                        gpui::transparent_black()
                                    })
                                    .hover(|s| s.bg(theme.accent.opacity(0.1)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _event, _window, cx| {
                                            this.project_picker_selected_index = 0;
                                            if let Some(p) = parent_path.clone() {
                                                this.browse_project_picker_dir(Some(p), cx);
                                            }
                                        }),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                Icon::new(IconName::Folder)
                                                    .size(px(16.0))
                                                    .text_color(theme.muted_foreground),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(theme.muted_foreground)
                                                    .child(".. (Parent Directory)"),
                                            ),
                                    )
                            }))
                            // Directory entries
                            .children(entries.iter().enumerate().map(|(i, entry)| {
                                let list_idx = if has_parent { i + 1 } else { i };
                                let is_selected = list_idx == selected_index;
                                let entry_path = entry.path.clone();
                                let entry_name = entry.name.clone();
                                let is_git = entry.is_git;
                                let is_project = entry.is_project;

                                let click_path = entry_path.clone();
                                let arrow_path = entry_path.clone();

                                div()
                                    .id(SharedString::from(format!("dir-item-{i}")))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(if is_selected {
                                        theme.selection
                                    } else {
                                        gpui::transparent_black()
                                    })
                                    .hover(|s| s.bg(theme.accent.opacity(0.1)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _event, _window, cx| {
                                            this.browse_project_picker_dir(
                                                Some(click_path.clone()),
                                                cx,
                                            );
                                        }),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                Icon::new(IconName::Folder)
                                                    .size(px(16.0))
                                                    .text_color(if is_selected {
                                                        theme.accent
                                                    } else {
                                                        theme.muted_foreground
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(if is_selected {
                                                        FontWeight::MEDIUM
                                                    } else {
                                                        FontWeight::NORMAL
                                                    })
                                                    .text_color(theme.foreground)
                                                    .child(entry_name),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .children((is_git || is_project).then(|| {
                                                div()
                                                    .px_1p5()
                                                    .py_0p5()
                                                    .rounded_sm()
                                                    .bg(theme.accent.opacity(0.15))
                                                    .text_xs()
                                                    .text_color(theme.accent)
                                                    .child(if is_project {
                                                        "project"
                                                    } else {
                                                        "git"
                                                    })
                                            }))
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "open-dir-{i}"
                                                )))
                                                .icon(IconName::ArrowRight)
                                                .tooltip("Open folder")
                                                .ghost()
                                                .xsmall()
                                                .on_click(cx.listener(
                                                    move |this, _event, _window, cx| {
                                                        this.browse_project_picker_dir(
                                                            Some(arrow_path.clone()),
                                                            cx,
                                                        );
                                                    },
                                                )),
                                            ),
                                    )
                            }))
                            // Empty state
                            .children((entries.is_empty() && !has_parent).then(|| {
                                div()
                                    .px_4()
                                    .py_6()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .child(
                                        Icon::new(IconName::Folder)
                                            .size(px(24.0))
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child("No subdirectories found"),
                                    )
                            })),
                    )
                    .child(
                        // Footer
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_4()
                            .py_2()
                            .border_t_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("↑ ↓ Select")
                                    .child("Enter Open")
                                    .child("Backspace Back")
                                    .child("Cmd+Enter Add")
                                    .child("Esc Close"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Daemon Host"),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.model.read(cx);
        let theme = cx.theme().colors;

        let git_status =
            active_project_git_status(state.active_work_dir.as_deref(), &state.git_statuses);

        let branch = git_status
            .and_then(|s| s.branch.clone())
            .unwrap_or_else(|| "no branch".to_string());
        let (additions, deletions) = git_status.map_or((0, 0), |s| {
            s.files
                .iter()
                .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions))
        });
        let dirty_count = git_status.map_or(0, |s| s.files.len());

        // An external ACP agent chooses its own model, so the selection alone
        // does not say what actually ran; show what the agent reports.
        let model_name = if state.available_models().is_empty() {
            "Connect a provider".to_string()
        } else if let Some(agent_model) = state.active_acp_model_label() {
            format!("{} · {agent_model}", state.selected_model)
        } else if let Some(opt) = state
            .available_models()
            .iter()
            .find(|m| m.id == state.selected_model)
        {
            opt.label.clone()
        } else if state.selected_model.is_empty() {
            "Connect a provider".to_string()
        } else {
            "Connect a provider".to_string()
        };

        let active_project = state
            .active_work_dir
            .as_ref()
            .and_then(|wd| {
                state
                    .projects
                    .iter()
                    .find(|p| &p.work_dir == wd)
                    .map(|p| p.name.clone())
            })
            .or_else(|| {
                state
                    .active_work_dir
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "No Project".into());

        let pr_badge = git_status.and_then(|s| s.pr.as_ref()).map(|pr| {
            let pr_url = pr.url.clone();
            let pr_num = pr.number;
            let failing_checks = pr.failing_checks;
            let pending_checks = pr.pending_checks;
            let ci_icon = if failing_checks > 0 {
                IconName::Close
            } else if pending_checks > 0 {
                IconName::Asterisk
            } else {
                IconName::Check
            };
            Button::new("status-pr-badge")
                .icon(ci_icon)
                .label(format!("PR #{pr_num}"))
                .ghost()
                .xsmall()
                .tooltip(if failing_checks > 0 {
                    format!("PR #{pr_num} ({failing_checks} failing checks) — Open in browser")
                } else if pending_checks > 0 {
                    format!("PR #{pr_num} (CI in progress) — Open in browser")
                } else {
                    format!("PR #{pr_num} (CI passed) — Open in browser")
                })
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    if pr_url.is_empty() {
                        this.open_git_review(cx);
                    } else {
                        cx.open_url(&pr_url);
                    }
                }))
        });

        StatusBar::new()
            .left(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("status-git-branch")
                            .icon(IconName::Github)
                            .label(format!("{active_project} · {branch}"))
                            .ghost()
                            .xsmall()
                            .tooltip("Switch or manage Git branches")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.open_git_branches(cx);
                            })),
                    )
                    .children((dirty_count > 0).then(|| {
                        div()
                            .id("status-git-changes")
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.8))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.open_git_review(cx);
                            }))
                            .children((additions > 0).then(|| {
                                div()
                                    .text_color(theme.success)
                                    .child(format!("+{additions}"))
                            }))
                            .children((deletions > 0).then(|| {
                                div()
                                    .text_color(theme.danger)
                                    .child(format!("−{deletions}"))
                            }))
                    }))
                    .children(pr_badge),
            )
            .right(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("status-model-badge")
                            .icon(IconName::Cpu)
                            .label(model_name.to_string())
                            .ghost()
                            .xsmall()
                            .tooltip("Switch model")
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.execute_palette_action("model", window, cx);
                            })),
                    )
                    .child(
                        Button::new("status-terminal-toggle")
                            .icon(if self.bottom_panel_visible {
                                IconName::PanelBottomOpen
                            } else {
                                IconName::PanelBottom
                            })
                            .label("Terminal")
                            .ghost()
                            .selected(self.bottom_panel_visible)
                            .xsmall()
                            .tooltip(if self.bottom_panel_visible {
                                "Hide terminal"
                            } else {
                                "Show terminal"
                            })
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.bottom_panel_visible = !this.bottom_panel_visible;
                                if this.bottom_panel_visible {
                                    let project = this.model.read(cx).active_work_dir.clone();
                                    let terminal = project
                                        .as_ref()
                                        .and_then(|project| this.terminal_groups.get(project))
                                        .and_then(|group| group.tabs.get(group.active_tab))
                                        .cloned()
                                        .unwrap_or_else(|| this.fallback_terminal(cx));
                                    let focus = terminal.read(cx).focus_handle(cx);
                                    focus.focus(window, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
    }

    fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        let inset = if self.sidebar_collapsed {
            px(110.0)
        } else {
            px(14.0)
        };
        self.chat_list.update(cx, |chat, cx| {
            chat.header_left_padding = inset;
            cx.notify();
        });
        cx.notify();
    }

    fn toggle_right_panel_action(
        &mut self,
        _: &ToggleRightPanel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.right_panel_visible = !self.right_panel_visible;
        cx.notify();
    }

    fn toggle_terminal_action(
        &mut self,
        _: &ToggleTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.bottom_panel_visible = !self.bottom_panel_visible;
        if self.bottom_panel_visible {
            let project = self.model.read(cx).active_work_dir.clone();
            let terminal = project
                .as_ref()
                .and_then(|project| self.terminal_groups.get(project))
                .and_then(|group| group.tabs.get(group.active_tab))
                .cloned()
                .unwrap_or_else(|| self.fallback_terminal(cx));
            let focus = terminal.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn begin_new_task_action(
        &mut self,
        _: &BeginNewTask,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model.update(cx, |state, _cx| {
            controller::dispatch(state, AppAction::BeginNewTask);
        });
        cx.notify();
    }

    fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model.update(cx, |state, _cx| {
            controller::dispatch(state, AppAction::OpenSettings);
        });
        cx.notify();
    }

    fn cancel_active_generation_action(
        &mut self,
        _: &CancelActiveGeneration,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_generating = self.model.read(cx).is_generating;
        if is_generating {
            self.model.update(cx, |state, cx| {
                controller::dispatch(state, AppAction::CancelGeneration);
                cx.notify();
            });
        }
    }

    fn select_chat_tab_action(
        &mut self,
        _: &SelectChatTab,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_list.update(cx, |chat, cx| {
            chat.set_tab(crate::screens::chat::CentralTab::Chat, cx);
        });
    }

    fn select_trajectory_tab_action(
        &mut self,
        _: &SelectTrajectoryTab,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_list.update(cx, |chat, cx| {
            chat.set_tab(crate::screens::chat::CentralTab::Trajectory, cx);
        });
    }

    fn select_editor_tab_action(
        &mut self,
        _: &SelectEditorTab,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_list.update(cx, |chat, cx| {
            chat.set_tab(crate::screens::chat::CentralTab::Editor, cx);
        });
    }

    fn focus_composer_action(
        &mut self,
        _: &FocusComposer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_list.update(cx, |chat, cx| {
            chat.focus_composer(window, cx);
        });
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace_page = self.model.read(cx).workspace_page;
        let terminal_project = self.model.read(cx).active_work_dir.clone();
        let (terminal_tabs, active_terminal_tab, active_terminal) =
            if let Some(project) = &terminal_project {
                let group = self.get_or_create_terminal_group(project, cx);
                (
                    group.tabs.clone(),
                    group.active_tab,
                    group.tabs[group.active_tab].clone(),
                )
            } else {
                let fallback = self.fallback_terminal(cx);
                (vec![fallback.clone()], 0, fallback)
            };
        let sidebar_tooltip = if self.sidebar_collapsed {
            "Expand sidebar"
        } else {
            "Collapse sidebar"
        };
        let theme = cx.theme().colors;

        let chat_page_content = {
            let upper_content = if self.right_panel_visible {
                h_resizable("workspace-chat-right-split")
                    .with_state(&self.right_panel_resizable_state)
                    .child(resizable_panel().child(self.chat_list.clone()))
                    .child(
                        resizable_panel()
                            .size(px(300.0))
                            .size_range(px(240.0)..px(800.0))
                            .child(self.right_panel.clone()),
                    )
                    .into_any_element()
            } else {
                self.chat_list.clone().into_any_element()
            };

            let main_content = if self.bottom_panel_visible {
                let tab_buttons = terminal_tabs.iter().enumerate().map(|(tab, _)| {
                    let select_project = terminal_project.clone();
                    let close_project = terminal_project.clone();
                    let other_project = terminal_project.clone();
                    let new_tab_project = terminal_project.clone();
                    let restart_terminal = terminal_tabs[tab].clone();
                    let select_view = cx.entity().clone();
                    let close_view = cx.entity().clone();
                    let other_view = cx.entity().clone();
                    let new_view = cx.entity().clone();
                    let is_selected = tab == active_terminal_tab;
                    let total_tabs = terminal_tabs.len();

                    div()
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .child(
                            Button::new(SharedString::from(format!("terminal-tab-{tab}")))
                                .label(format!("Shell {}", tab + 1))
                                .icon(IconName::SquareTerminal)
                                .ghost()
                                .selected(is_selected)
                                .xsmall()
                                .on_click(move |_event, _window, cx| {
                                    if let Some(project) = &select_project {
                                        select_view.update(cx, |this, cx| {
                                            this.select_terminal_tab(project, tab, cx)
                                        });
                                    }
                                })
                                .context_menu(move |menu, _window, _cx| {
                                    let c_proj = close_project.clone();
                                    let c_view = close_view.clone();
                                    let mut menu =
                                        menu.item(PopupMenuItem::new("Close Shell").on_click(
                                            move |_event, _window, cx| {
                                                if let Some(project) = &c_proj {
                                                    c_view.update(cx, |this, cx| {
                                                        this.close_terminal_tab(project, tab, cx);
                                                    });
                                                }
                                            },
                                        ));

                                    if total_tabs > 1 {
                                        let o_proj = other_project.clone();
                                        let o_view = other_view.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new("Close Other Tabs").on_click(
                                                move |_event, _window, cx| {
                                                    if let Some(project) = &o_proj {
                                                        o_view.update(cx, |this, cx| {
                                                            this.close_other_terminal_tabs(
                                                                project, tab, cx,
                                                            );
                                                        });
                                                    }
                                                },
                                            ),
                                        );
                                    }

                                    let r_term = restart_terminal.clone();
                                    menu = menu.item(PopupMenuItem::new("Restart Shell").on_click(
                                        move |_event, _window, cx| {
                                            r_term.update(cx, |t, cx| t.restart(cx));
                                        },
                                    ));

                                    let n_proj = new_tab_project.clone();
                                    let n_view = new_view.clone();
                                    menu.item(PopupMenuItem::new("New Terminal Tab").on_click(
                                        move |_event, _window, cx| {
                                            if let Some(project) = &n_proj {
                                                n_view.update(cx, |this, cx| {
                                                    this.add_terminal_tab(project.clone(), cx);
                                                });
                                            }
                                        },
                                    ))
                                }),
                        )
                        .child({
                            let close_p = terminal_project.clone();
                            let close_v = cx.entity().clone();
                            Button::new(SharedString::from(format!("terminal-tab-close-{tab}")))
                                .icon(IconName::Close)
                                .tooltip("Close shell")
                                .ghost()
                                .xsmall()
                                .on_click(move |_event, _window, cx| {
                                    if let Some(project) = &close_p {
                                        close_v.update(cx, |this, cx| {
                                            this.close_terminal_tab(project, tab, cx)
                                        });
                                    }
                                })
                        })
                });

                let active_terminal_clear = active_terminal.clone();
                let active_terminal_restart = active_terminal.clone();
                let new_project = terminal_project.clone();
                let new_view = cx.entity().clone();
                let close_panel_view = cx.entity().clone();

                let project_badge = {
                    let name = terminal_project
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("Terminal");
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .bg(theme.secondary)
                        .child(
                            Icon::new(IconName::SquareTerminal)
                                .xsmall()
                                .text_color(theme.primary),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(name.to_string()),
                        )
                };

                let toolbar_actions = div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(new_project.clone().map(|project| {
                        Button::new("terminal-new-tab")
                            .icon(IconName::Plus)
                            .tooltip("New terminal tab")
                            .ghost()
                            .xsmall()
                            .on_click(move |_event, _window, cx| {
                                new_view.update(cx, |this, cx| {
                                    this.add_terminal_tab(project.clone(), cx)
                                });
                            })
                    }))
                    .child(
                        Button::new("terminal-clear-btn")
                            .icon(IconName::Undo2)
                            .tooltip("Clear terminal")
                            .ghost()
                            .xsmall()
                            .on_click(move |_event, _window, cx| {
                                active_terminal_clear.update(cx, |t, cx| t.clear(cx));
                            }),
                    )
                    .child(
                        Button::new("terminal-restart-btn")
                            .icon(IconName::Redo)
                            .tooltip("Restart shell")
                            .ghost()
                            .xsmall()
                            .on_click(move |_event, _window, cx| {
                                active_terminal_restart.update(cx, |t, cx| t.restart(cx));
                            }),
                    )
                    .child(
                        Button::new("terminal-close-panel-btn")
                            .icon(IconName::Close)
                            .tooltip("Hide terminal (Cmd+J)")
                            .ghost()
                            .xsmall()
                            .on_click(move |_event, _window, cx| {
                                close_panel_view.update(cx, |this, cx| {
                                    this.bottom_panel_visible = false;
                                    cx.notify();
                                });
                            }),
                    );

                let terminal_panel = div()
                    .size_full()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .bg(theme.background)
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .h(px(34.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .px_2()
                            .gap_2()
                            .overflow_x_scrollbar()
                            .bg(theme.tab_bar)
                            .border_b_1()
                            .border_color(theme.border)
                            .child(project_badge)
                            .child(div().w(px(1.0)).h(px(16.0)).bg(theme.border))
                            .children(tab_buttons)
                            .child(div().flex_1())
                            .child(toolbar_actions),
                    )
                    .child(div().flex_1().min_h_0().child(active_terminal));

                v_resizable("workspace-main-bottom-split")
                    .with_state(&self.bottom_panel_resizable_state)
                    .child(resizable_panel().child(upper_content))
                    .child(
                        resizable_panel()
                            .size(px(280.0))
                            .size_range(px(120.0)..px(800.0))
                            .child(terminal_panel),
                    )
                    .into_any_element()
            } else {
                upper_content
            };

            if !self.sidebar_collapsed {
                h_resizable("workspace-sidebar-main-split")
                    .with_state(&self.sidebar_resizable_state)
                    .child(
                        resizable_panel()
                            .size(px(240.0))
                            .size_range(px(160.0)..px(500.0))
                            .child(self.sidebar.clone()),
                    )
                    .child(resizable_panel().child(main_content))
                    .into_any_element()
            } else {
                main_content
            }
        };

        let page_content = match workspace_page {
            WorkspacePage::Chat => chat_page_content.into_any_element(),
            WorkspacePage::Settings => self.settings.clone().into_any_element(),
        };

        let view_with_status_bar = div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(page_content))
            .child(self.render_status_bar(cx));

        let git_dialog_layer = self
            .right_panel
            .update(cx, |panel, cx| panel.render_git_dialog_layer(cx));

        div()
            .relative()
            .flex()
            .w_full()
            .h_full()
            .on_action(cx.listener(Self::toggle_command_palette))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::toggle_right_panel_action))
            .on_action(cx.listener(Self::toggle_terminal_action))
            .on_action(cx.listener(Self::begin_new_task_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::cancel_active_generation_action))
            .on_action(cx.listener(Self::select_chat_tab_action))
            .on_action(cx.listener(Self::select_trajectory_tab_action))
            .on_action(cx.listener(Self::select_editor_tab_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .bg(theme.background)
            .child(view_with_status_bar)
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("command-palette-btn")
                    .icon(IconName::SquareTerminal)
                    .tooltip("Command Palette (Cmd+K)")
                    .ghost()
                    .selected(self.command_palette_open)
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .right(px(48.0))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.command_palette_open = !this.command_palette_open;
                        if this.command_palette_open {
                            this.command_state.update(cx, |state, cx| {
                                state.set_query("", window, cx);
                                state.focus(window, cx);
                            });
                        }
                        cx.notify();
                    }))
            }))
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("right-panel-toggle")
                    .icon(IconName::PanelRight)
                    .tooltip(if self.right_panel_visible {
                        "Hide right panel"
                    } else {
                        "Show right panel"
                    })
                    .ghost()
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .right(px(12.0))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.right_panel_visible = !this.right_panel_visible;
                        cx.notify();
                    }))
            }))
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("sidebar-collapse-toggle")
                    .icon(IconName::PanelLeft)
                    .tooltip(sidebar_tooltip)
                    .ghost()
                    .xsmall()
                    .absolute()
                    .top(px(9.0))
                    .left(px(76.0))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.sidebar_collapsed = !this.sidebar_collapsed;
                        let inset = if this.sidebar_collapsed {
                            px(110.0)
                        } else {
                            px(14.0)
                        };
                        this.chat_list.update(cx, |chat, cx| {
                            chat.header_left_padding = inset;
                            cx.notify();
                        });
                        cx.notify();
                    }))
            }))
            .children(
                self.command_palette_open
                    .then(|| self.render_command_palette(cx)),
            )
            .children(
                self.project_picker_open
                    .then(|| self.render_project_picker(cx)),
            )
            .children(git_dialog_layer)
            .children(self.render_update_notice(cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        active_project_git_status, git_result_matches_active, next_workspace_event,
        session_pr_target_is_active, GitEvent, WorkspacePumpEvent,
    };
    use crate::services::updater::UpdaterEvent;
    use crate::state::SessionInfo;
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use threadlane_protocol::git::GitStatus;

    #[test]
    fn status_bar_uses_shared_status_for_active_project() {
        let active = PathBuf::from("/projects/current");
        let mut statuses = HashMap::new();
        statuses.insert(
            active.clone(),
            GitStatus {
                branch: Some("fresh".into()),
                ..GitStatus::default()
            },
        );

        assert_eq!(
            active_project_git_status(Some(active.as_path()), &statuses)
                .and_then(|status| status.branch.as_deref()),
            Some("fresh")
        );
    }

    #[test]
    fn git_result_is_accepted_only_for_active_work_dir() {
        let active = Path::new("/projects/current");
        let stale = Path::new("/projects/previous");

        assert!(git_result_matches_active(active, active));
        assert!(!git_result_matches_active(stale, active));
    }

    #[test]
    fn session_pr_refresh_stops_after_the_branch_is_no_longer_used() {
        let target = (
            PathBuf::from("/projects/current"),
            "feature/one".to_string(),
        );
        let targets = HashSet::from([target.clone()]);

        assert!(session_pr_target_is_active(&targets, &target));
        assert!(!session_pr_target_is_active(&HashSet::new(), &target));
    }

    #[tokio::test]
    async fn workspace_pump_waits_for_a_real_producer_event() {
        let (_git_tx, mut git_rx) = tokio::sync::mpsc::unbounded_channel::<GitEvent>();
        let (_updater_tx, mut updater_rx) = tokio::sync::mpsc::unbounded_channel::<UpdaterEvent>();
        let (_sessions_tx, mut sessions_rx) =
            tokio::sync::mpsc::unbounded_channel::<(PathBuf, Vec<SessionInfo>)>();
        let (model_tx, mut model_rx) = tokio::sync::mpsc::unbounded_channel();

        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            next_workspace_event(
                &mut git_rx,
                &mut updater_rx,
                &mut sessions_rx,
                &mut model_rx,
            ),
        )
        .await
        .is_err());
        model_tx.send(()).unwrap();
        assert!(matches!(
            next_workspace_event(
                &mut git_rx,
                &mut updater_rx,
                &mut sessions_rx,
                &mut model_rx,
            )
            .await,
            Some(WorkspacePumpEvent::Model)
        ));
    }
}
