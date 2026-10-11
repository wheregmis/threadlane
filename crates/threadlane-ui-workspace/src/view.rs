use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::command::{CommandGroup, CommandState};
use gpui_component::resizable::ResizableState;
use gpui_component::status_bar::StatusBar;
use gpui_component::{ActiveTheme, IconName, Selectable, Sizable};

actions!(
    threadlane_workspace,
    [
        ToggleCommandPalette,
        SwitchSession,
        ToggleRightPanel,
        ToggleTerminal,
        OpenSettings,
        CancelActiveGeneration,
        SelectChatTab,
        SelectTrajectoryTab,
        SelectEditorTab,
        FocusComposer,
        QuitThreadlane,
        HideThreadlane,
        HideOtherApplications,
        ShowAllApplications,
        MinimizeWindow,
        ZoomWindow,
    ]
);
// `BeginNewTask` and `ToggleSidebar` are shared with the sidebar, so the
// action types live in `threadlane-ui-sidebar`.
use threadlane_ui_sidebar::{BeginNewTask, ToggleSidebar};
use threadlane_git::GitStatus;

use threadlane_ui_state::{actions::AppAction, controller};
use threadlane_ui_chat::{ChatListView, ConversationFindHandoff, TrajectoryView};
use threadlane_ui_github::GitHubView;
use threadlane_ui_automation::AutomationsView;
use gpui_component::WindowExt;
use threadlane_ui_right_panel::RightPanelView;
use threadlane_ui_settings::SettingsView;
use threadlane_ui_sidebar::SidebarView;
use threadlane_ui_terminal::{
    FindInTerminalOutput, LinkDestination, OpenTerminalLink, SelectionStatus, TerminalView,
};
use threadlane_ui_state::updater::{self, UpdaterEvent};
use threadlane_coding_agent::controller::runtime_status_text;
use threadlane_ui_state::{
    AppState, SessionHydrationRequest, SessionInfo, WorkspacePage,
};
use threadlane_ui_state::projection::{
    coding_agent_options, compute_full_session_projection, compute_latest_run_completion,
    compute_session_messages,
};
use threadlane_updater::UpdateStatus;

#[path = "conversation_search.rs"]
mod conversation_search;
use conversation_search::*;
#[path = "session_switcher.rs"]
mod session_switcher;
use session_switcher::{SessionNavigation, SessionPicker};

fn close_command_palette(open: &mut bool, previous_focus: &mut Option<FocusHandle>, window: &mut Window, cx: &mut App) {
    *open = false;
    if let Some(focus) = previous_focus.take() {
        focus.focus(window, cx);
    }
}

fn open_active_close_confirmation(
    window: &mut Window,
    cx: &mut App,
    model: Entity<AppState>,
    work: Vec<threadlane_ui_state::ActiveCloseWork>,
) {
    let mut list = work.iter().take(8).map(|item| format!("{} — {} · {}", item.title, item.project, item.status)).collect::<Vec<_>>();
    if work.len() > list.len() { list.push(format!("…and {} more", work.len() - list.len())); }
    let description = format!(
        "Closing this window quits Threadlane and interrupts its active work. External commands may not stop immediately.\n\n{}",
        list.join("\n")
    );
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let expected: Vec<_> = work.iter().map(|item| item.identity.clone()).collect();
        let current_model = model.clone();
        dialog.title("Close Threadlane while work is active?")
            .description(description.clone())
            .confirm()
            .show_cancel(true)
            .ok_text("Close anyway")
            .ok_variant(gpui_component::button::ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                let current_work = current_model.read(cx).active_close_work();
                let current: Vec<_> = current_work.iter().map(|item| item.identity.clone()).collect();
                if threadlane_ui_state::close_work_needs_refresh(&current, &expected) {
                    window.close_dialog(cx);
                    let model = current_model.clone();
                    window.defer(cx, move |window, cx| {
                        open_active_close_confirmation(window, cx, model, current_work.clone());
                    });
                    return true;
                }
                window.remove_window();
                true
            })
    });
}

fn open_github_from_palette(state: &mut AppState, notify: impl FnOnce()) {
    controller::dispatch(state, AppAction::OpenGitHub);
    notify();
}

fn install_window_close_handler(
    window: &mut Window,
    cx: &mut App,
    request_close: impl Fn(&mut Window, &mut App) -> bool + Clone + 'static,
) {
    window.on_window_should_close(cx, request_close.clone());
    let window_handle = window.window_handle();
    cx.on_action(move |_: &QuitThreadlane, cx| {
        // Also stop the startup fallback when there is no focused window.
        cx.stop_propagation();
        let request_close = request_close.clone();
        // An action can arrive while this window is already being updated.
        cx.defer(move |cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                if request_close(window, cx) {
                    window.remove_window();
                }
            });
        });
    });
}

pub fn init(cx: &mut App) {
    threadlane_ui_kit::init_editor(cx);
    threadlane_ui_automation::init(cx);
    threadlane_ui_github::view::init(cx);
    threadlane_ui_terminal::init(cx);
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-q", QuitThreadlane, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-h", HideThreadlane, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-h", HideOtherApplications, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-k", SwitchSession, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-k", SwitchSession, None),
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
        KeyBinding::new("escape", CancelActiveGeneration, Some("ThreadlaneWorkspace")),
        KeyBinding::new("cmd-s", threadlane_ui_editor::SaveFile, None),
        KeyBinding::new("ctrl-s", threadlane_ui_editor::SaveFile, None),
    ]);
    // Startup owns no active work. Once loaded, the workspace installs the
    // guarded Quit handler below, including when its window is unfocused.
    cx.on_action(|_: &QuitThreadlane, cx| cx.quit());
    #[cfg(target_os = "macos")]
    {
        cx.on_action(|_: &HideThreadlane, cx| cx.hide());
        cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
        cx.on_action(|_: &MinimizeWindow, cx| {
            if let Some(window) = cx.active_window() {
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, _| window.minimize_window());
                });
            }
        });
        cx.on_action(|_: &ZoomWindow, cx| {
            if let Some(window) = cx.active_window() {
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, _| window.zoom_window());
                });
            }
        });
        use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
        cx.set_menus([
            Menu::new("Threadlane").items([
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Threadlane", HideThreadlane),
                MenuItem::action("Hide Others", HideOtherApplications),
                MenuItem::action("Show All", ShowAllApplications),
                MenuItem::separator(),
                MenuItem::action("Quit Threadlane", QuitThreadlane),
            ]),
            Menu::new("File").items([
                MenuItem::action("New Chat", BeginNewTask),
                MenuItem::action("Save File", threadlane_ui_editor::SaveFile),
            ]),
            Menu::new("Edit").items([
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            ]),
            Menu::new("View").items([
                MenuItem::action("Command Palette", ToggleCommandPalette),
                MenuItem::action("Switch Session…", SwitchSession),
                MenuItem::separator(),
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Toggle Right Panel", ToggleRightPanel),
                MenuItem::action("Toggle Terminal", ToggleTerminal),
                MenuItem::separator(),
                MenuItem::action("Chat", SelectChatTab),
                MenuItem::action("Trajectory", SelectTrajectoryTab),
                MenuItem::action("Editor", SelectEditorTab),
                MenuItem::action("Focus Composer", FocusComposer),
            ]),
            Menu::new("Window").items([
                MenuItem::action("Minimize", MinimizeWindow),
                MenuItem::action("Zoom", ZoomWindow),
            ]),
        ]);
    }
}

enum GitEvent {
    Loaded {
        work_dir: PathBuf,
        result: Result<GitStatus, String>,
    },
    PrLoaded {
        work_dir: PathBuf,
        branch: String,
        result: Result<Option<threadlane_git::GitHubPrInfo>, String>,
    },
}

enum WorkspacePumpEvent {
    Git(GitEvent),
    Updater(UpdaterEvent),
    Sessions(u64, PathBuf, Vec<SessionInfo>),
    Model,
}

async fn next_workspace_event(
    git_rx: &mut tokio::sync::mpsc::UnboundedReceiver<GitEvent>,
    updater_rx: &mut tokio::sync::mpsc::UnboundedReceiver<UpdaterEvent>,
    sessions_rx: &mut tokio::sync::mpsc::UnboundedReceiver<(u64, PathBuf, Vec<SessionInfo>)>,
    model_rx: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
) -> Option<WorkspacePumpEvent> {
    tokio::select! {
        event = git_rx.recv() => event.map(WorkspacePumpEvent::Git),
        event = updater_rx.recv() => event.map(WorkspacePumpEvent::Updater),
        event = sessions_rx.recv() => event.map(|(generation, work_dir, sessions)| WorkspacePumpEvent::Sessions(generation, work_dir, sessions)),
        event = model_rx.recv() => event.map(|()| WorkspacePumpEvent::Model),
    }
}

fn terminal_link_is_current(
    displayed: Option<EntityId>,
    source: EntityId,
    visible: bool,
    page: WorkspacePage,
    url: &str,
) -> bool {
    visible && page == WorkspacePage::Chat && displayed == Some(source)
        && threadlane_ui_terminal::is_web_url(url)
}

struct TerminalGroup {
    tabs: Vec<Entity<TerminalView>>,
    active_tab: usize,
}

/// Largest terminal excerpt the handoff writes into a chat draft, in
/// UTF-8 bytes. Matches the "Select less terminal output" notice.
const TERMINAL_EXCERPT_LIMIT: usize = 32 * 1024;

/// `None` when the selection can be handed to the chat draft; otherwise
/// the user-facing reason the command is disabled or a stale activation
/// is rejected. Textual, never color-only.
fn terminal_excerpt_block_reason(status: Option<SelectionStatus>) -> Option<&'static str> {
    match status {
        None => Some("Select terminal output first"),
        Some(status) if !status.has_text => {
            Some("The terminal selection is empty — select output text first")
        }
        Some(status) if status.excerpt_len > TERMINAL_EXCERPT_LIMIT => {
            Some("Select less terminal output (maximum 32 KiB)")
        }
        Some(_) => None,
    }
}

/// The labeled, safely fenced plain-text block appended to the draft.
/// `launched_in` is the shell's launch directory, not its current cwd.
/// The fence grows past the excerpt's longest backtick run so the block
/// parses as one unit; whitespace inside is preserved.
fn format_terminal_excerpt(shell: usize, launched_in: &Path, text: &str) -> String {
    let fence = threadlane_ui_kit::safe_fence(text);
    format!(
        "Terminal · Shell {shell} · launched in {}\n{fence}\n{}\n{fence}",
        launched_in.display(),
        text.trim_end_matches('\n')
    )
}

/// Which file-editor host the **Add selection to chat** command surface
/// acts on.
enum EditorSelectionTarget {
    /// The chat view's embedded central Editor tab.
    CentralEditor,
    /// The right panel's open Files document.
    FilesPanel,
}

/// A threadlane-managed worktree lives at `<project>/.threadlane/worktrees/<name>`.
fn path_is_threadlane_worktree(cwd: &Path) -> bool {
    cwd.components()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0].as_os_str() == ".threadlane" && pair[1].as_os_str() == "worktrees")
}

/// Toolbar chip label for a shell's working directory. Worktrees keep
/// their `.threadlane/worktrees/<name>` tail — the bare leaf is a session
/// id — while every other directory shows its last component.
fn shell_cwd_label(cwd: &Path) -> String {
    let components: Vec<_> = cwd.components().collect();
    if let Some(index) = components.windows(2).position(|pair| {
        pair[0].as_os_str() == ".threadlane" && pair[1].as_os_str() == "worktrees"
    }) {
        let tail: PathBuf = components[index + 1..].iter().map(|c| c.as_os_str()).collect();
        return format!(".threadlane/{}", tail.display());
    }
    cwd.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned())
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

fn session_pr_refresh_delay(succeeded: bool, active: bool, open: bool) -> std::time::Duration {
    // Full review data is needed by auto-address, but historical sessions must
    // not poll it at the same cadence as the branch the user is working on.
    std::time::Duration::from_secs(match (succeeded, active, open) {
        (false, _, _) => 10 * 60,
        (true, true, true) => 2 * 60,
        (true, false, true) => 10 * 60,
        (true, _, false) => 30 * 60,
    })
}

pub struct WorkspaceView {
    window_handle: AnyWindowHandle,
    last_link_terminal: Option<Entity<TerminalView>>,
    focus_handle: FocusHandle,
    rendered_page: WorkspacePage,
    model: Entity<AppState>,
    sidebar: Entity<SidebarView>,
    chat_list: Entity<ChatListView>,
    github: Entity<GitHubView>,
    automations: Entity<AutomationsView>,
    settings: Entity<SettingsView>,
    right_panel: Entity<RightPanelView>,
    fallback_terminal: Option<Entity<TerminalView>>,
    terminal_groups: HashMap<PathBuf, TerminalGroup>,
    sidebar_collapsed: bool,
    right_panel_visible: bool,
    bottom_panel_visible: bool,
    command_palette_open: bool,
    command_palette_previous_focus: Option<FocusHandle>,
    command_state: Entity<CommandState>,
    command_state_subscription: Subscription,
    /// Some while the palette is showing project conversation search
    /// instead of the commands list; `conversation_search.rs` owns the mode.
    conversation_search: Option<ConversationSearch>,
    session_navigation: SessionNavigation,
    session_picker: Option<SessionPicker>,
    recent_palette_actions: Vec<&'static str>,
    last_git_work_dir: Option<PathBuf>,
    last_git_pr_targets: HashSet<(PathBuf, String)>,
    sidebar_resizable_state: Entity<ResizableState>,
    right_panel_resizable_state: Entity<ResizableState>,
    bottom_panel_resizable_state: Entity<ResizableState>,
    // Only divider drags change these rem-based preferences; window constraints do not.
    preferred_panel_sizes: [f32; 3],
    panel_layout: Option<(gpui::Size<Pixels>, Pixels, [bool; 3])>,
    git_event_tx: tokio::sync::mpsc::UnboundedSender<GitEvent>,
    updater_tx: tokio::sync::mpsc::UnboundedSender<UpdaterEvent>,
    /// Two-step close confirm for shells holding output: (project, tab).
    /// A misclick arms instead of destroying build/test scrollback; the
    /// second click confirms.
    pending_terminal_close: Option<(PathBuf, EntityId)>,
    terminal_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    fn spawn_session_hydration(
        model: Entity<AppState>,
        request: SessionHydrationRequest,
        cx: &mut AsyncApp,
    ) {
        cx.spawn(async move |cx| {
            // Remote mode: the daemon owns projection and runtime construction;
            // the HydrateSession command answers with a SessionSnapshot event.
            let remote = model.update(cx, |state, _cx| {
                if state.daemon_remote {
                    state.dispatch_command(
                        threadlane_protocol::daemon::SessionCommand::HydrateSession {
                            request: request.clone(),
                        },
                    );
                }
                state.daemon_remote
            });
            if remote {
                return;
            }
            let hydrate_work_dir = request
                .runtime_options
                .as_ref()
                .map(|options| options.work_dir.clone());
            // Runtime construction loads WASI extensions through wasmi and must
            // not run on GPUI's 512 KiB GCD worker stacks. The blocking-pool
            // task starts now and overlaps the transcript projection below.
            let (daemon_core, browser_bridge) = model.update(cx, |state, _cx| {
                (state.daemon_core.clone(), state.browser_bridge.clone())
            });
            let runtime_task = request.runtime_options.clone().map(|options| {
                let runtime_options = coding_agent_options(
                    options.work_dir.clone(),
                    request.session_file.clone(),
                    options.model.clone(),
                    options.model_roles.clone(),
                    browser_bridge,
                );
                daemon_core.get_or_create_runtime_async(
                    request.session_id.clone(),
                    options.work_dir,
                    request.session_file.clone(),
                    runtime_options,
                    None,
                )
            });
            if request.reload_messages {
                // The completion token is captured strictly before the
                // transcript load so an acknowledged result can never claim
                // content the presented transcript did not contain; a failed
                // capture carries `None` and simply cannot acknowledge.
                let capture_file = request.session_file.clone();
                let presented_completion = cx
                    .background_executor()
                    .spawn(async move {
                        compute_latest_run_completion(&capture_file).ok().flatten()
                    })
                    .await;
                let history_file = request.session_file.clone();
                let history = cx
                    .background_executor()
                    .spawn(async move { compute_session_messages(&history_file) })
                    .await;
                let _ = model.update(cx, |state, cx| {
                    state.finish_session_hydration(&request.session_id, &request.session_file);
                    if !state.active_session_matches(&request.session_id, &request.session_file) {
                        return;
                    }
                    match history {
                        Ok(messages) => state.apply_session_messages(
                            &request.session_id,
                            &request.session_file,
                            messages,
                            presented_completion,
                        ),
                        Err(error) => {
                            state.session_status = Some(format!("Could not load session: {error}"))
                        }
                    }
                    cx.notify();
                });
            }
            let session_file = request.session_file.clone();
            let result = cx
                .background_executor()
                .spawn(async move { compute_full_session_projection(&session_file) })
                .await;
            let runtime = match runtime_task {
                Some(task) => Some(task.await.map_err(|error| error.to_string())),
                None => None,
            };
            let _ = model.update(cx, |state, cx| {
                if !state.active_session_matches(&request.session_id, &request.session_file) {
                    return;
                }
                match result {
                    Ok(result) => {
                        state.apply_session_hydration(
                            &request.session_id,
                            &request.session_file,
                            result,
                        );
                        state.session_status = state.session_status_for_file(&request.session_file);
                    }
                    Err(error) => {
                        state.session_status = Some(format!("Could not load session: {error}"))
                    }
                }
                match runtime {
                    Some(Ok(runtime)) => {
                        let runtime = state.register_session_runtime(
                            hydrate_work_dir.clone().unwrap_or_default(),
                            request.session_file.clone(),
                            runtime,
                        );
                        state.is_generating = runtime.is_generating();
                        state.selected_model = runtime.selected_model.clone();
                        state.reasoning_effort = runtime.reasoning_effort();
                        if let Some(status) = runtime_status_text(runtime.status()) {
                            state.session_status = Some(status);
                        }
                        state.set_reasoning_effort(state.reasoning_effort);
                    }
                    Some(Err(error)) => {
                        state.session_status =
                            Some(format!("Could not start session runtime: {error}"));
                    }
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn build(state: AppState, window: &mut Window, cx: &mut App) -> Entity<Self> {
        let model = cx.new(|_cx| state);
        if let Some((generation, task)) =
            model.update(cx, |state, _cx| state.pairing_restore_task())
        {
            let pairing_model = model.clone();
            cx.spawn(async move |cx| {
                let result = match task.await {
                    Ok(result) => result,
                    Err(error) => Err(format!("pairing restore failed: {error}")),
                };
                let _ = pairing_model.update(cx, |state, cx| {
                    state.finish_pairing_restore(generation, result);
                    cx.notify();
                });
            })
            .detach();
        }
        let sidebar = cx.new(|cx| SidebarView::new(model.clone(), window, cx));
        let chat_list = cx.new(|cx| ChatListView::new(model.clone(), window, cx));
        let github = cx.new(|cx| GitHubView::new(model.clone(), window, cx));
        let automations = cx.new(|cx| AutomationsView::new(model.clone(), cx));
        let automation_updates = model.update(cx, |state, _| state.start_automations());
        let settings = cx.new(|cx| SettingsView::new(model.clone(), window, cx));
        let right_panel = cx.new(|cx| RightPanelView::new(model.clone(), window, cx));
        // The trajectory is a chat-crate view hosted by the right panel;
        // injected as AnyView so the panel crate stays chat-free.
        let trajectory_view = cx.new(|cx| TrajectoryView::new(model.clone(), window, cx));
        right_panel.update(cx, |panel, _cx| {
            panel.set_trajectory_view(trajectory_view.clone().into());
        });
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
        if threadlane_updater::is_configured() {
            updater::check(updater_tx.clone());
        }

        let model_clone = model.clone();
        let view = cx.new(|cx| {
            if let Some(mut automation_updates) = automation_updates {
                cx.spawn_in(window, async move |this, cx| {
                    let mut last_notification = None;
                    while automation_updates.changed().await.is_ok() {
                        let projection = automation_updates.borrow_and_update().clone();
                        if this.update_in(cx, |this: &mut Self, window, cx| {
                            if let Some((id, message)) = &projection.notification {
                                if last_notification.as_ref() != Some(id) {
                                    window.push_notification(message.clone(), cx);
                                    last_notification = Some(id.clone());
                                }
                            }
                            this.model.update(cx, |state, cx| { state.apply_automation_projection(projection); cx.notify(); });
                        }).is_err() { break; }
                    }
                }).detach();
            }
            let focus_handle = cx.focus_handle();
            focus_handle.focus(window, cx);
            let sub = cx.observe(&model_clone, move |this: &mut Self, model, cx| {
                this.session_navigation.observe(model.read(cx));
                this.sync_git_status_with_active_project(cx);
                if let Some(cmd) =
                    model.update(cx, |state, _cx| state.requested_terminal_command.take())
                {
                    this.bottom_panel_visible = true;
                    let state = model.read(cx);
                    let group_key = state.terminal_group_key();
                    let work_dir = state.active_git_work_dir();
                    let unavailable = state.active_worktree_unavailable();
                    let term = match (group_key, work_dir) {
                        (Some(group_key), Some(work_dir)) => Some(
                            this.get_or_create_active_terminal(&group_key, &work_dir, cx),
                        ),
                        _ => (!unavailable).then(|| this.fallback_terminal(cx)),
                    };
                    if let Some(term) = term {
                        term.update(cx, |term, _cx| {
                            let trimmed = cmd.trim_end();
                            term.send_input(&format!("{trimmed}\n"));
                        });
                    } else {
                        model.update(cx, |state, _cx| {
                            state.session_status = Some("The active worktree is unavailable".into());
                        });
                    }
                }
                if let Some(work_dir) =
                    model.update(cx, |state, _cx| state.requested_terminal_work_dir.take())
                {
                    this.bottom_panel_visible = true;
                    let state = model.read(cx);
                    let group_key = state.terminal_group_key();
                    let project = state.active_git_work_dir();
                    let unavailable = state.active_worktree_unavailable();
                    if let (Some(group_key), Some(_)) = (group_key, project) {
                        this.add_terminal_tab_for_project(group_key, work_dir, cx);
                    } else if !unavailable {
                        this.get_or_create_active_terminal(&work_dir, &work_dir, cx);
                    }
                }
                this.invalidate_conversation_search(cx);
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
                        WorkspacePumpEvent::Sessions(generation, work_dir, sessions) => {
                            session_refreshes.push((generation, work_dir, sessions));
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
                            this.model
                                .update(cx, |state, _cx| state.take_pending_hydrations())
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
                            for (generation, work_dir, sessions) in session_refreshes {
                                changed |=
                                    state.apply_session_refresh(work_dir, sessions, generation);
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

            let right_panel_selection_sub = cx.subscribe_in(
                &right_panel,
                window,
                |this, _panel, request: &threadlane_ui_kit::EditorSelectionRequest, window, cx| {
                    this.insert_files_selection_to_chat(request.clone(), window, cx);
                },
            );

            let command_state_sub =
                cx.observe(&command_state, |_this: &mut Self, _command_state, cx| {
                    cx.notify();
                });

            Self {
                window_handle: window.window_handle(),
                last_link_terminal: None,
                focus_handle,
                rendered_page: model.read(cx).workspace_page,
                session_navigation: SessionNavigation::new(model.read(cx)),
                session_picker: None,
                model,
                sidebar,
                chat_list,
                github,
                automations,
                settings,
                right_panel,
                fallback_terminal: None,
                terminal_groups: HashMap::new(),
                sidebar_collapsed: false,
                right_panel_visible: false,
                bottom_panel_visible: false,
                command_palette_open: false,
                command_palette_previous_focus: None,
                command_state,
                command_state_subscription: command_state_sub,
                conversation_search: None,
                recent_palette_actions: Vec::new(),
                last_git_work_dir: None,
                last_git_pr_targets: HashSet::new(),
                sidebar_resizable_state,
                right_panel_resizable_state,
                bottom_panel_resizable_state,
                preferred_panel_sizes: [16.5, 22.0, 14.0],
                panel_layout: None,
                git_event_tx,
                updater_tx,
                pending_terminal_close: None,
                terminal_subscriptions: Vec::new(),
                _subscriptions: vec![sub, right_panel_sub, right_panel_selection_sub],
            }
        });
        view.update(cx, |view, cx| {
            let weak_view = cx.weak_entity();
            let request_close = move |window: &mut Window, cx: &mut App| {
                if cx.windows().len() != 1 {
                    return true;
                }
                let Some(work) = weak_view
                    .update(cx, |view, cx| view.model.read(cx).active_close_work())
                    .ok()
                else {
                    return true;
                };
                if work.is_empty() {
                    return true;
                }
                if window.has_active_dialog(cx) {
                    return false;
                }

                let Some(model) = weak_view.update(cx, |view, _| view.model.clone()).ok() else {
                    return false;
                };
                open_active_close_confirmation(window, cx, model, work);
                false
            };
            install_window_close_handler(window, cx, request_close);
            let hydration_requests = view
                .model
                .update(cx, |state, _cx| state.take_pending_hydrations());
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
            view.github
                .update(cx, |github, cx| github.sync_active_project(cx));
            // Pull the live Zen model list in the background so new
            // `opencode-go/*` models appear in the picker without a restart.
            let discovery_model = view.model.clone();
            cx.spawn(async move |_view, cx| {
                threadlane_ui_settings::refresh_discovered_models_and_update(discovery_model, cx)
                    .await;
            })
            .detach();
            // Same for the OpenAI list and the Antigravity inventory: live
            // results merge additively (seeds are the offline guarantee) and
            // unknown Antigravity entries drop out once confirmed retired.
            // TTL-guarded, so project switches just revalidate.
            let openai_model = view.model.clone();
            cx.spawn(async move |_view, cx| {
                threadlane_ui_settings::refresh_openai_models_and_update(openai_model, cx).await;
            })
            .detach();
            let antigravity_model = view.model.clone();
            cx.spawn(async move |_view, cx| {
                threadlane_ui_settings::refresh_antigravity_models_and_update(antigravity_model, cx)
                    .await;
            })
            .detach();
            // Connect each external agent once in the background and cache
            // the models it offers, so every session's picker can offer them
            // before its own engine spawns. Revalidation stays here: a
            // failing agent keeps its cached models while settings shows why.
            let acp_model = view.model.clone();
            let acp_project = view.model.read(cx).active_work_dir.clone();
            cx.spawn(async move |_view, cx| {
                threadlane_ui_settings::refresh_acp_models_and_update(acp_model, cx, acp_project)
                    .await;
            })
            .detach();
        });

        let view_handle = view.downgrade();
        let shortcut_subscription = cx.intercept_keystrokes(move |event, window, cx| {
            let keystroke = &event.keystroke;
            let switch_session = keystroke.modifiers.shift;
            let modifier = if switch_session && cfg!(target_os = "macos") {
                keystroke.modifiers.platform
            } else {
                keystroke.modifiers.platform || keystroke.modifiers.control
            };
            if keystroke.key.eq_ignore_ascii_case("k") && modifier && !keystroke.modifiers.alt {
                if switch_session && event.context_stack.iter().any(|context| {
                    context.contains("PopupMenu") || context.contains("Dialog") || context.contains("Sheet")
                }) {
                    cx.stop_propagation();
                    return;
                }
                if let Some(view) = view_handle.upgrade() {
                    view.update(cx, |view, cx| {
                        if switch_session {
                            view.switch_session_action(&SwitchSession, window, cx);
                        } else {
                            view.toggle_command_palette(&ToggleCommandPalette, window, cx);
                        }
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
        self.model.update(cx, |state, cx| {
            state.workspace_page = WorkspacePage::Chat;
            cx.notify();
        });
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_review(cx);
        });
        self.refresh_git_status(cx);
        cx.notify();
    }

    fn open_agents_panel(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |state, cx| {
            state.workspace_page = WorkspacePage::Chat;
            cx.notify();
        });
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_surface(threadlane_ui_right_panel::Surface::Agents, cx);
        });
        cx.notify();
    }

    fn open_trajectory_panel(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |state, cx| {
            state.workspace_page = WorkspacePage::Chat;
            cx.notify();
        });
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_surface(threadlane_ui_right_panel::Surface::Trajectory, cx);
        });
        cx.notify();
    }

    fn open_git_branches(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |state, cx| {
            state.workspace_page = WorkspacePage::Chat;
            cx.notify();
        });
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_branch_popover(cx);
        });
        self.refresh_git_status(cx);
        cx.notify();
    }

    fn open_git_new_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model.update(cx, |state, cx| {
            state.workspace_page = WorkspacePage::Chat;
            cx.notify();
        });
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_new_branch_dialog(window, cx);
        });
        self.refresh_git_status(cx);
        cx.notify();
    }

    fn open_git_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model.update(cx, |state, cx| {
            state.workspace_page = WorkspacePage::Chat;
            cx.notify();
        });
        self.right_panel_visible = true;
        self.right_panel.update(cx, |panel, cx| {
            panel.open_merge_dialog(window, cx);
        });
        self.refresh_git_status(cx);
        cx.notify();
    }

    /// Creates a project terminal and tracks its selection validity so
    /// the "Add selection to chat" affordance refreshes when the
    /// available/rejected state could flip — not on every PTY frame.
    /// The signal covers presence, non-whitespace content, and the size
    /// cap, so extending a whitespace drag into real text or output
    /// scrolling under a static selection still re-renders.
    fn new_terminal_with_tracking(
        &self,
        cwd: PathBuf,
        cx: &mut Context<Self>,
    ) -> (Entity<TerminalView>, Vec<Subscription>) {
        // A remote daemon owns terminals: the PTY runs on the host over the
        // wire. Local mode keeps the in-process PTY backend.
        let backend = self.model.read(cx).daemon_remote.then(|| {
            Arc::new(crate::remote_terminal::RemoteTerminalBackend::new(
                self.model.read(cx).terminal_bus(),
            ))
        });
        let terminal = cx.new(|cx| match backend {
            Some(backend) => TerminalView::new_with_backend(cwd, backend, cx),
            None => TerminalView::new(cwd, cx),
        });
        let mut last_availability = None;
        let subscription = cx.observe(&terminal, move |_, terminal, cx| {
            let availability = terminal
                .read(cx)
                .selection_status()
                .map(|status| (status.has_text, status.excerpt_len > TERMINAL_EXCERPT_LIMIT));
            if availability != last_availability {
                last_availability = availability;
                cx.notify();
            }
        });
        let links = cx.subscribe(&terminal, |this, terminal, request: &OpenTerminalLink, cx| {
            let owner = cx.weak_entity();
            let window = this.window_handle;
            let request = request.clone();
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        this.open_terminal_link(terminal, request, window, cx)
                    });
                });
            });
        });
        (terminal, vec![subscription, links])
    }

    /// The terminal shown in the bottom panel at this moment, with the
    /// project group it belongs to (`None` for the unattached fallback
    /// shell). `None` while the worktree is unavailable or no shell
    /// exists yet.
    fn displayed_terminal(&self, cx: &App) -> Option<(Option<PathBuf>, Entity<TerminalView>)> {
        let state = self.model.read(cx);
        if state.active_worktree_unavailable() {
            return None;
        }
        match state.terminal_group_key() {
            Some(key) => self
                .terminal_groups
                .get(&key)
                .and_then(|group| group.tabs.get(group.active_tab).cloned())
                .map(|terminal| (Some(key), terminal)),
            None => self
                .fallback_terminal
                .clone()
                .map(|terminal| (None, terminal)),
        }
    }

    fn open_terminal_link(
        &mut self,
        terminal: Entity<TerminalView>,
        request: OpenTerminalLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !terminal_link_is_current(
            self.displayed_terminal(cx).map(|(_, active)| active.entity_id()),
            terminal.entity_id(),
            self.bottom_panel_visible,
            self.model.read(cx).workspace_page,
            &request.url,
        ) {
            return;
        }
        match request.destination {
            LinkDestination::DefaultBrowser => cx.open_url(&request.url),
            LinkDestination::Threadlane => {
                let result = self.right_panel.update(cx, |panel, cx| {
                    panel.open_terminal_url(&request.url, window, cx)
                });
                match result {
                    Ok(()) => {
                        self.right_panel_visible = true;
                        cx.notify();
                    }
                    Err(error) => {
                        window.push_notification(error, cx);
                        terminal.update(cx, |terminal, cx| {
                            terminal.retry_link(request.url, window, cx)
                        });
                    }
                }
            }
        }
    }

    /// Appends the displayed terminal's selection to the chat draft.
    /// Terminal and group bound at activation are re-validated against
    /// what is displayed now, and the chat view re-validates its draft
    /// target after draft sync — a session or project switch that raced
    /// the click leaves draft and selection untouched. Never sends,
    /// queues, or stages anything.
    fn add_terminal_selection_to_chat(
        &mut self,
        terminal: Entity<TerminalView>,
        group_key: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (current_group, destination, is_fallback) = {
            let state = self.model.read(cx);
            (
                state.terminal_group_key(),
                (
                    state.active_work_dir.clone(),
                    state.active_session_id.clone(),
                ),
                self.fallback_terminal
                    .as_ref()
                    .is_some_and(|fallback| fallback.entity_id() == terminal.entity_id()),
            )
        };
        let still_displayed = match &group_key {
            Some(key) => {
                current_group.as_ref() == Some(key)
                    && self
                        .terminal_groups
                        .get(key)
                        .and_then(|group| group.tabs.get(group.active_tab))
                        .is_some_and(|active| active.entity_id() == terminal.entity_id())
            }
            None => current_group.is_none() && is_fallback,
        };
        if !still_displayed {
            window.push_notification(
                "The terminal changed — the selection was not added",
                cx,
            );
            return;
        }
        if let Some(reason) =
            terminal_excerpt_block_reason(terminal.read(cx).selection_status())
        {
            window.push_notification(reason, cx);
            return;
        }
        let snapshot = terminal
            .read(cx)
            .selection_snapshot()
            .expect("block reason proves a snapshot exists");
        let shell = group_key
            .as_ref()
            .and_then(|key| self.terminal_groups.get(key))
            .and_then(|group| {
                group
                    .tabs
                    .iter()
                    .position(|tab| tab.entity_id() == terminal.entity_id())
                    .map(|index| index + 1)
            })
            .unwrap_or(1);
        let excerpt = format_terminal_excerpt(shell, &snapshot.launched_in, &snapshot.text);
        let added = self.chat_list.update(cx, |chat, cx| {
            chat.append_draft_text_for(destination, &excerpt, window, cx)
        });
        if !added {
            window.push_notification(
                "The chat draft changed — the selection was not added",
                cx,
            );
        }
    }

    /// Which file-editor host the **Add selection to chat** command acts
    /// on. A visible host wins; when neither is visible the embedded
    /// central editor still qualifies — its buffer and selection persist
    /// behind the Chat tab. `Err` carries the textual disabled reason.
    fn editor_selection_target(&self, cx: &App) -> Result<EditorSelectionTarget, SharedString> {
        let chat = self.chat_list.read(cx);
        let panel = self.right_panel.read(cx);
        let central_reason = chat.editor_selection_block_reason(cx);
        let files_reason = panel.files_selection_block_reason(cx);
        let files_visible = self.right_panel_visible && panel.editable_file_open();
        if chat.editor_is_current_tab() {
            return match central_reason {
                None => Ok(EditorSelectionTarget::CentralEditor),
                Some(reason) => Err(reason),
            };
        }
        if files_visible {
            return match files_reason {
                None => Ok(EditorSelectionTarget::FilesPanel),
                Some(reason) => Err(reason),
            };
        }
        match central_reason {
            None => Ok(EditorSelectionTarget::CentralEditor),
            Some(central) => Err(central),
        }
    }

    /// Appends a Files-panel editor selection to the chat draft. Source
    /// buffer, selection range, checkout, and destination were captured at
    /// activation; all are revalidated so a raced close or project switch
    /// leaves the draft untouched.
    fn insert_files_selection_to_chat(
        &mut self,
        request: threadlane_ui_kit::EditorSelectionRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let still_current = self
            .right_panel
            .update(cx, |panel, cx| panel.selection_request_is_current(&request, cx));
        if !still_current {
            window.push_notification(
                "The editor selection changed — it was not added",
                cx,
            );
            return;
        }
        let excerpt = threadlane_ui_kit::format_editor_excerpt(
            &request.relative_path,
            &request.snapshot,
            request.dirty,
        );
        let added = self.chat_list.update(cx, |chat, cx| {
            chat.append_draft_text_for(request.destination.clone(), &excerpt, window, cx)
        });
        if !added {
            window.push_notification(
                "The chat draft changed — the selection was not added",
                cx,
            );
        }
    }

    fn get_or_create_active_terminal(
        &mut self,
        group_key: &PathBuf,
        cwd: &PathBuf,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let group = self.get_or_create_terminal_group_with_cwd(group_key, cwd, cx);
        group.tabs[group.active_tab].clone()
    }

    fn get_or_create_terminal_group(
        &mut self,
        project: &PathBuf,
        cx: &mut Context<Self>,
    ) -> &mut TerminalGroup {
        self.get_or_create_terminal_group_with_cwd(project, project, cx)
    }

    fn get_or_create_terminal_group_with_cwd(
        &mut self,
        group_key: &PathBuf,
        cwd: &PathBuf,
        cx: &mut Context<Self>,
    ) -> &mut TerminalGroup {
        if self
            .terminal_groups
            .get(group_key)
            .is_none_or(|group| group.tabs.is_empty())
        {
            let (terminal, subscription) = self.new_terminal_with_tracking(cwd.clone(), cx);
            self.terminal_subscriptions.extend(subscription);
            let group = self
                .terminal_groups
                .entry(group_key.clone())
                .or_insert_with(|| TerminalGroup {
                    tabs: Vec::new(),
                    active_tab: 0,
                });
            group.tabs.push(terminal);
            group.active_tab = 0;
        }
        let group = self
            .terminal_groups
            .get_mut(group_key)
            .expect("terminal group was just created");
        group.active_tab = group.active_tab.min(group.tabs.len().saturating_sub(1));
        group
    }

    fn add_terminal_tab(
        &mut self,
        group_key: PathBuf,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (terminal, subscription) = self.new_terminal_with_tracking(cwd, cx);
        self.terminal_subscriptions.extend(subscription);
        terminal.read(cx).focus_handle(cx).focus(window, cx);
        let group = self
            .terminal_groups
            .entry(group_key)
            .or_insert(TerminalGroup {
                tabs: Vec::new(),
                active_tab: 0,
            });
        group.tabs.push(terminal);
        group.active_tab = group.tabs.len() - 1;
        cx.notify();
    }

    fn add_terminal_tab_for_project(
        &mut self,
        project: PathBuf,
        work_dir: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let (terminal, subscription) = self.new_terminal_with_tracking(work_dir, cx);
        self.terminal_subscriptions.extend(subscription);
        let group = self.get_or_create_terminal_group(&project, cx);
        group.tabs.push(terminal);
        group.active_tab = group.tabs.len() - 1;
        cx.notify();
    }

    fn fallback_terminal(&mut self, cx: &mut Context<Self>) -> Entity<TerminalView> {
        if self.fallback_terminal.is_none() {
            let project =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let (terminal, subscription) = self.new_terminal_with_tracking(project, cx);
            self.terminal_subscriptions.extend(subscription);
            self.fallback_terminal = Some(terminal);
        }
        self.fallback_terminal
            .clone()
            .expect("fallback terminal exists")
    }

    fn request_terminal_tab(
        &mut self,
        action: threadlane_ui_kit::TerminalTabAction,
        project: Option<&PathBuf>,
        cwd: Option<&PathBuf>,
        terminal: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use threadlane_ui_kit::TerminalTabAction;
        let tab = project
            .and_then(|project| self.terminal_groups.get(project))
            .and_then(|group| {
                group
                    .tabs
                    .iter()
                    .position(|candidate| candidate.entity_id() == terminal.entity_id())
            });
        match action {
            TerminalTabAction::Select => {
                if let (Some(project), Some(tab)) = (project, tab) {
                    self.select_terminal_tab(project, tab, window, cx);
                }
            }
            TerminalTabAction::Close => {
                if let (Some(project), Some(tab)) = (project, tab) {
                    self.close_terminal_tab(project, tab, cx);
                }
            }
            TerminalTabAction::CloseOthers => {
                if let (Some(project), Some(tab)) = (project, tab) {
                    self.close_other_terminal_tabs(project, tab, cx);
                }
            }
            TerminalTabAction::Restart => {
                if tab.is_some()
                    || self
                        .fallback_terminal
                        .as_ref()
                        .is_some_and(|fallback| fallback.entity_id() == terminal.entity_id())
                {
                    terminal.update(cx, |terminal, cx| terminal.restart(cx));
                }
            }
            TerminalTabAction::NewTab => {
                if let (Some(project), Some(cwd)) = (project, cwd) {
                    self.add_terminal_tab(project.clone(), cwd.clone(), window, cx);
                }
            }
        }
    }

    fn request_terminal_action(
        &mut self,
        action: threadlane_ui_kit::TerminalAction,
        project: Option<PathBuf>,
        cwd: Option<PathBuf>,
        terminal: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use threadlane_ui_kit::TerminalAction;
        if action == TerminalAction::Hide {
            self.bottom_panel_visible = false;
            cx.notify();
            return;
        }
        if !self
            .displayed_terminal(cx)
            .is_some_and(|(_, active)| active.entity_id() == terminal.entity_id())
        {
            return;
        }
        match action {
            TerminalAction::NewTab => {
                if let Some(project) = project {
                    self.add_terminal_tab(project.clone(), cwd.unwrap_or(project), window, cx);
                }
            }
            TerminalAction::Clear => terminal.update(cx, |terminal, cx| terminal.clear(cx)),
            TerminalAction::Restart => terminal.update(cx, |terminal, cx| terminal.restart(cx)),
            TerminalAction::Find => terminal.update(cx, |terminal, cx| {
                terminal.open_find(&FindInTerminalOutput, window, cx)
            }),
            TerminalAction::OpenLinks => {
                terminal.update(cx, |terminal, cx| terminal.open_links(window, cx))
            }
            TerminalAction::AddSelectionToChat => {
                self.add_terminal_selection_to_chat(terminal, project, window, cx)
            }
            _ => {}
        }
    }

    fn select_terminal_tab(
        &mut self,
        project: &PathBuf,
        tab: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(group) = self.terminal_groups.get_mut(project) {
            group.active_tab = tab.min(group.tabs.len().saturating_sub(1));
            if let Some(terminal) = group.tabs.get(group.active_tab) {
                terminal.read(cx).focus_handle(cx).focus(window, cx);
            }
            cx.notify();
        }
    }

    fn close_terminal_tab(&mut self, project: &PathBuf, tab: usize, cx: &mut Context<Self>) {
        // Two-step confirm when the shell holds output: the first click arms,
        // the second destroys. A clean shell closes immediately.
        let Some(terminal_id) = self.terminal_groups.get(project)
            .and_then(|group| group.tabs.get(tab)).map(|terminal| terminal.entity_id()) else { return; };
        let dirty = self
            .terminal_groups
            .get(project)
            .and_then(|group| group.tabs.get(tab))
            .is_some_and(|terminal| terminal.read(cx).has_output());
        if dirty && self.pending_terminal_close != Some((project.clone(), terminal_id)) {
            self.pending_terminal_close = Some((project.clone(), terminal_id));
            cx.notify();
            return;
        }
        self.pending_terminal_close = None;
        // Replacement shells open in the session checkout, not the project
        // root, when this group belongs to the active project.
        let replacement_cwd = self
            .model
            .read(cx)
            .active_git_work_dir()
            .filter(|_| {
                self.model.read(cx).terminal_group_key().as_ref() == Some(project)
            })
            .unwrap_or_else(|| project.clone());
        let needs_replacement = match self.terminal_groups.get_mut(project) {
            Some(group) if tab < group.tabs.len() => {
                if group.tabs.len() > 1 {
                    group.tabs.remove(tab);
                    if tab < group.active_tab {
                        group.active_tab -= 1;
                    } else if tab == group.active_tab {
                        group.active_tab = group.active_tab.min(group.tabs.len() - 1);
                    }
                    false
                } else {
                    true
                }
            }
            Some(_) => return,
            None => return,
        };
        if needs_replacement {
            let (terminal, subscription) =
                self.new_terminal_with_tracking(replacement_cwd.clone(), cx);
            self.terminal_subscriptions.extend(subscription);
            if let Some(group) = self.terminal_groups.get_mut(project) {
                group.tabs = vec![terminal];
                group.active_tab = 0;
            }
            self.bottom_panel_visible = false;
        }
        cx.notify();
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
                group.tabs = vec![keep_elem];
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
        if self.command_palette_open {
            self.close_command_palette(window, cx);
        } else {
            self.command_palette_previous_focus = window.focused(cx);
            self.command_palette_open = true;
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
        if action_key != "go_task" {
            self.recent_palette_actions.retain(|key| *key != action_key);
            self.recent_palette_actions.insert(0, action_key);
            self.recent_palette_actions.truncate(5);
        }
        match action_key {
            "new" => {
                self.begin_new_task_action(&BeginNewTask, window, cx);
            }
            "attach" => {
                cx.spawn(async move |_this, cx| {
                    let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                        return;
                    };
                    let path = folder.path().to_path_buf();
                    let _ = model.update(cx, |state, cx| {
                        controller::dispatch(state, AppAction::AttachProject(path));
                        cx.notify();
                    });
                })
                .detach();
            }
            "find_files" => threadlane_ui_right_panel::file_search::open(self.model.clone(), window, cx),
            "git" => self.open_git_review(cx),
            "automations" => {
                self.model.update(cx, |state, cx| { controller::dispatch(state, AppAction::OpenAutomations); cx.notify(); });
            }
            "github" => {
                model.update(cx, |state, cx| {
                    open_github_from_palette(state, || cx.notify());
                });
            }
            "git_branch" => self.open_git_branches(cx),
            "git_new_branch" => self.open_git_new_branch(window, cx),
            "git_merge" => self.open_git_merge(window, cx),
            "git_stash_pop" => {
                self.open_git_review(cx);
                self.right_panel.update(cx, |panel, cx| {
                    panel.restore_current_stash(window, cx);
                });
            }
            "git_pull" => {
                self.open_git_review(cx);
                self.right_panel.update(cx, |panel, cx| {
                    panel.run_git_action(threadlane_ui_right_panel::GitAction::Pull, window, cx);
                });
            }
            "settings" => {
                self.open_settings_action(&OpenSettings, window, cx);
            }
            "sidebar" => {
                self.toggle_sidebar_action(&ToggleSidebar, window, cx);
            }
            "panel" => {
                self.right_panel_visible = !self.right_panel_visible;
            }
            "go_task" => {
                self.switch_session_action(&SwitchSession, window, cx);
                return;
            }
            "search_conversations" => {
                // Reopen the palette in conversation-search mode (the
                // confirm path already closed it and restored focus).
                self.command_palette_previous_focus = window.focused(cx);
                self.command_palette_open = true;
                self.command_state.update(cx, |state, cx| {
                    state.focus(window, cx);
                });
                self.enter_conversation_search(window, cx);
                return; // keep palette open in the new mode
            }
            "open_file" => {
                self.right_panel_visible = true;
                self.right_panel.update(cx, |panel, cx| {
                    panel.open_surface(threadlane_ui_right_panel::Surface::Files, cx);
                });
            }
            "run_terminal" => {
                self.toggle_terminal_action(&ToggleTerminal, window, cx);
            }
            "open_terminal_link" => {
                self.command_palette_open = false;
                self.command_palette_previous_focus = None;
                if let Some((_, terminal)) = self.displayed_terminal(cx) {
                    self.bottom_panel_visible = true;
                    terminal.update(cx, |terminal, cx| terminal.open_links(window, cx));
                } else {
                    window.push_notification("Open a terminal first", cx);
                }
            }
            "add_terminal_selection" => {
                match self.displayed_terminal(cx) {
                    Some((group_key, terminal)) => {
                        self.add_terminal_selection_to_chat(terminal, group_key, window, cx)
                    }
                    None => window.push_notification("No terminal is visible", cx),
                }
            }
            "add_editor_selection" => match self.editor_selection_target(cx) {
                Ok(EditorSelectionTarget::CentralEditor) => self
                    .chat_list
                    .update(cx, |chat, cx| chat.add_editor_selection_to_chat(window, cx)),
                Ok(EditorSelectionTarget::FilesPanel) => self
                    .right_panel
                    .update(cx, |panel, cx| panel.request_add_selection_to_chat(window, cx)),
                Err(reason) => window.push_notification(reason.to_string(), cx),
            },
            "open_issue" => {
                model.update(cx, |state, cx| {
                    open_github_from_palette(state, || cx.notify());
                });
            }
            "switch_worktree" => {
                use threadlane_ui_state::WorkMode;
                model.update(cx, |state, cx| {
                    let new_mode = match state.draft_work_mode {
                        WorkMode::Local => WorkMode::Worktree,
                        WorkMode::Worktree => WorkMode::Local,
                    };
                    state.set_work_mode(new_mode);
                    cx.notify();
                });
            }
            "ask_agent" => {
                self.focus_composer_action(&FocusComposer, window, cx);
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
        let active_git_work_dir = self.model.read(cx).active_git_work_dir();
        if self.last_git_work_dir == active_git_work_dir {
            return;
        }

        self.last_git_work_dir = active_git_work_dir.clone();

        if let Some(work_dir) = active_git_work_dir {
            self.spawn_git_status_refresh(work_dir, cx);
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
            self.spawn_session_pr_refresh(work_dir, branch, cx);
        }
    }

    fn spawn_session_pr_refresh(&self, work_dir: PathBuf, branch: String, cx: &App) {
        let tx = self.git_event_tx.clone();
        let client = self.model.read(cx).daemon_client.clone();
        cx.background_executor()
            .spawn(async move {
                let result = threadlane_ui_state::project_io::inspect_pr_for_branch(
                    &client,
                    &work_dir,
                    branch.clone(),
                )
                .await;
                let _ = tx.send(GitEvent::PrLoaded {
                    work_dir,
                    branch,
                    result,
                });
            })
            .detach();
    }

    fn schedule_session_pr_refresh(
        target: (PathBuf, String),
        delay: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, _cx| {
                if session_pr_target_is_active(&this.last_git_pr_targets, &target) {
                    this.spawn_session_pr_refresh(target.0, target.1, _cx);
                }
            });
        })
        .detach();
    }

    fn refresh_git_status(&mut self, cx: &App) {
        let Some(work_dir) = self.model.read(cx).active_git_work_dir() else {
            self.last_git_work_dir = None;
            return;
        };

        self.last_git_work_dir = Some(work_dir.clone());
        self.spawn_git_status_refresh(work_dir, cx);
    }

    fn spawn_git_status_refresh(&self, work_dir: PathBuf, cx: &App) {
        let tx = self.git_event_tx.clone();
        let client = self.model.read(cx).daemon_client.clone();
        let executor = cx.background_executor().clone();
        cx.background_executor()
            .spawn(async move {
                // A remote client's handshake can still be in flight when
                // the first refresh fires — `supports_project_io` stays
                // false until the peer's version lands, and a one-shot
                // inspect would record that transient as the status
                // error. Wait for it; `supports_command_requests` already
                // true means the handshake *completed* at a pre-v3
                // version, which should fail the inspect fast instead of
                // waiting out the window. While still unsettled after the
                // wait, retry the inspect once before reporting.
                for attempt in 0..2 {
                    for _ in 0..20 {
                        if client.supports_project_io()
                            || client.supports_command_requests()
                        {
                            break;
                        }
                        executor.timer(std::time::Duration::from_millis(200)).await;
                    }
                    // `sync_remote` refreshes remote-tracking refs first;
                    // a failed fetch must not hide the local Git status
                    // (offline use is valid).
                    let result =
                        threadlane_ui_state::project_io::inspect(&client, &work_dir, true)
                            .await;
                    if result.is_ok()
                        || client.supports_command_requests()
                        || attempt == 1
                    {
                        let _ = tx.send(GitEvent::Loaded { work_dir, result });
                        return;
                    }
                }
            })
            .detach();
    }

    fn apply_git_event(&mut self, event: GitEvent, cx: &mut Context<Self>) {
        let (work_dir, result) = match event {
            GitEvent::PrLoaded {
                work_dir,
                branch,
                result,
            } => {
                let target = (work_dir.clone(), branch.clone());
                let state = self.model.read(cx);
                let active = state.projects.iter().flat_map(|project| &project.sessions)
                    .any(|session| state.active_session_id.as_ref() == Some(&session.id)
                        && session.work_dir == work_dir
                        && session.git_branch.as_ref() == Some(&branch));
                let open = result.as_ref().ok().and_then(|pr| pr.as_ref())
                    .is_some_and(|pr| pr.state.eq_ignore_ascii_case("open"));
                let refresh_delay = session_pr_refresh_delay(result.is_ok(), active, open);
                if let Ok(pr) = result {
                    self.model.update(cx, |state, cx| {
                        state
                            .git_prs
                            .insert((work_dir.clone(), branch.clone()), pr.clone());
                        if let Some(info) = pr.as_ref() {
                            state.auto_address_pr_reviews(work_dir.clone(), branch.clone(), info);
                        }
                        cx.notify();
                    });
                }
                Self::schedule_session_pr_refresh(target, refresh_delay, cx);
                return;
            }
            GitEvent::Loaded { work_dir, result } => (work_dir, result),
        };
        let Some(active_work_dir) = self.model.read(cx).active_git_work_dir() else {
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

    fn activate_update(
        &mut self,
        _: &threadlane_ui_sidebar::ActivateUpdate,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Read current state at activation, not a stale render snapshot. Publish the
        // busy state synchronously so repeated clicks cannot start duplicate jobs.
        let status = self.model.read(cx).update_status.clone();
        let next = match &status {
            UpdateStatus::Available(info) => UpdateStatus::Downloading {
                version: info.version.clone(),
                progress: 0.0,
            },
            UpdateStatus::ReadyToInstall { .. } => UpdateStatus::Installing,
            UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Error(_) => {
                UpdateStatus::Checking
            }
            _ => return,
        };
        self.model.update(cx, |state, cx| {
            state.update_status = next;
            cx.notify();
        });
        match status {
            UpdateStatus::Available(info) => updater::download(info, self.updater_tx.clone()),
            UpdateStatus::ReadyToInstall { info, bytes } => {
                updater::install(info, bytes, self.updater_tx.clone())
            }
            UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Error(_) => {
                updater::check(self.updater_tx.clone())
            }
            _ => unreachable!(),
        }
    }

    fn render_command_palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let state = model.read(cx);

        let commands = threadlane_ui_kit::workspace_commands();

        let handoff_terminal = self
            .displayed_terminal(cx)
            .map(|(_group, terminal)| terminal);
        let excerpt_block = match &handoff_terminal {
            Some(terminal) => terminal_excerpt_block_reason(terminal.read(cx).selection_status()),
            None => Some("No terminal is visible"),
        };

        // Editor selection availability mirrors the visible host's
        // disabled state; both hosts carry textual reasons.
        let editor_selection_block = self
            .editor_selection_target(cx)
            .err()
            .map(|reason| reason.to_string());

        // Conversation search scopes to the attached project; with none it
        // stays listed but disabled, carrying its reason in the subtitle.
        let no_project = conversation_search_scope(state).is_none();
        let mut commands_group = CommandGroup::new().label("Commands & Actions");
        let disabled_reason = |key: &str| match key {
            "add_terminal_selection" => excerpt_block,
            "add_editor_selection" => editor_selection_block.as_deref(),
            "search_conversations" if no_project => Some("Select a project first"),
            _ => None,
        };
        for command in &commands {
            commands_group = commands_group.item(command.item(disabled_reason(command.key())));
        }
        let mut recent_group = CommandGroup::new().label("Recently used");
        for action_key in &self.recent_palette_actions {
            if let Some(command) = commands.iter().find(|command| command.key() == *action_key) {
                recent_group = recent_group.item(command.item(disabled_reason(command.key())));
            }
        }

        let mut session_entries = Vec::new();
        let settings_query = !self.command_state.read(cx).query(cx).trim().is_empty();
        let settings_entries = if settings_query {
            threadlane_ui_settings::SETTINGS_SEARCH_ITEMS
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut settings_group = CommandGroup::new().label("Settings");
        for item in threadlane_ui_settings::SETTINGS_SEARCH_ITEMS.iter() {
            let title = item.title;
            let subtitle = format!("Settings · {}", item.page);
            let keywords = item.keywords;
            settings_group = settings_group.item(
                threadlane_ui_kit::palette_item(title, subtitle, IconName::Settings)
                    .keywords(keywords.iter().copied()),
            );
        }
        let mut sessions_group = CommandGroup::new().label("Sessions");
        for project in &state.projects {
            for session in &project.sessions {
                session_entries.push((project.work_dir.clone(), session.id.clone()));
                let title = session.title.clone();
                let project_name = project.name.clone();
                // Mirror the sidebar search scope so palette lookup also
                // matches the session branch when one is recorded.
                let mut session_keywords = vec![project.name.clone(), session.id.clone()];
                if let Some(branch) = session.git_branch.as_deref() {
                    session_keywords.push(branch.to_string());
                }
                let item =
                    threadlane_ui_kit::palette_item(title, project_name, IconName::SquareTerminal)
                        .keywords(session_keywords);
                sessions_group = sessions_group.item(item);
            }
        }

        let view = cx.weak_entity();
        let view_cancel = cx.weak_entity();

        let view_backdrop = cx.weak_entity();
        let command = threadlane_ui_kit::workspace_palette_command(&self.command_state)
            .group(recent_group)
            .group(commands_group)
            .when(settings_query, |command| command.group(settings_group))
            .group(sessions_group)
            .on_cancel(move |window, cx| {
                let _ = view_cancel.update(cx, |this, cx| {
                    this.close_command_palette(window, cx);
                    cx.notify();
                });
            })
            .on_confirm(move |index, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.close_command_palette(window, cx);
                    let sessions_section = if settings_query { 3 } else { 2 };
                    if index.section == 2 && settings_query {
                        if let Some(id) = settings_entries.get(index.row) {
                            this.model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::OpenSettings);
                                cx.notify();
                            });
                            this.settings.update(cx, |settings, cx| {
                                settings.open_search_destination(id, cx)
                            });
                            cx.notify();
                            return;
                        }
                    }
                    if index.section == 0 {
                        if let Some(action_key) = this.recent_palette_actions.get(index.row) {
                            this.execute_palette_action(action_key, window, cx);
                        }
                    } else if index.section == 1 {
                        if let Some(command) = commands.get(index.row) {
                            this.execute_palette_action(command.key(), window, cx);
                        }
                    } else if index.section == sessions_section {
                        if let Some((work_dir, session_id)) = session_entries.get(index.row) {
                            let work_dir = work_dir.clone();
                            let session_id = session_id.clone();
                            this.model.update(cx, |state, cx| {
                                controller::dispatch(
                                    state,
                                    AppAction::SelectSession {
                                        work_dir,
                                        session_id,
                                    },
                                );
                                cx.notify();
                            });
                        }
                    }
                    cx.notify();
                });
            });
        threadlane_ui_kit::workspace_palette_frame(
            command,
            move |window, cx| {
                let _ = view_backdrop.update(cx, |this, cx| {
                    this.close_command_palette(window, cx);
                    cx.notify();
                });
            },
            cx,
        )
        .into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let page_summary = (self.model.read(cx).workspace_page == WorkspacePage::GitHub)
            .then(|| self.github.read(cx).status_text(cx));
        let muted = cx.theme().muted_foreground;
        let state = self.model.read(cx);

        let git_status =
            active_project_git_status(state.active_git_work_dir().as_deref(), &state.git_statuses);

        let branch = git_status
            .and_then(|s| s.branch.clone())
            .unwrap_or_else(|| "no branch".to_string());
        let (additions, deletions) = git_status.map_or((0, 0), |s| {
            s.files
                .iter()
                .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions))
        });
        let dirty_count = git_status.map_or(0, |s| s.files.len());

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
        let checkout_display = state
            .active_git_work_dir()
            .map(|dir| dir.display().to_string());
        let git_context = match checkout_display.as_deref() {
            Some(checkout) => format!("{active_project} · {branch}\n{checkout}"),
            None => format!("{active_project} · {branch}"),
        };

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
                .accessibility_label(if failing_checks > 0 {
                    format!("PR #{pr_num} ({failing_checks} failing checks) — Open in browser")
                } else if pending_checks > 0 {
                    format!("PR #{pr_num} (CI in progress) — Open in browser")
                } else {
                    format!("PR #{pr_num} (CI passed) — Open in browser")
                })
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
                            .label("Git")
                            .ghost()
                            .xsmall()
                            .accessibility_label(format!(
                                "{git_context} — Switch or manage branches"
                            ))
                            .tooltip(format!("{git_context} — Switch or manage branches"))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.open_git_branches(cx);
                            })),
                    )
                    .children((dirty_count > 0).then(|| {
                        Button::new("status-git-changes")
                            .label(format!("{dirty_count} changed · +{additions} −{deletions}"))
                            .ghost()
                            .xsmall()
                            .accessibility_label(format!(
                                "{dirty_count} changed files, {additions} additions, \
                                 {deletions} deletions — Review workspace changes"
                            ))
                            .tooltip("Review workspace changes")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.open_git_review(cx);
                            }))
                    }))
                    .children(pr_badge)
                    .children(page_summary.map(|summary| {
                        div().pl_2().text_xs().text_color(muted).child(summary)
                    })),
            )
            .right(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
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
                            .accessibility_label(if self.bottom_panel_visible {
                                "Hide terminal"
                            } else {
                                "Show terminal"
                            })
                            .tooltip(if self.bottom_panel_visible {
                                "Hide terminal"
                            } else {
                                "Show terminal"
                            })
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.bottom_panel_visible = !this.bottom_panel_visible;
                                if this.bottom_panel_visible {
                                    let state = this.model.read(cx);
                                    let project = state.terminal_group_key();
                                    let unavailable = state.active_worktree_unavailable();
                                    if !unavailable {
                                        let terminal = project
                                            .as_ref()
                                            .and_then(|project| this.terminal_groups.get(project))
                                            .and_then(|group| group.tabs.get(group.active_tab))
                                            .cloned()
                                            .unwrap_or_else(|| this.fallback_terminal(cx));
                                        let focus = terminal.read(cx).focus_handle(cx);
                                        focus.focus(window, cx);
                                    }
                                }
                                cx.notify();
                            })),
                    ),
            )
    }

    fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        let inset = if self.sidebar_collapsed {
            window.rem_size() * threadlane_ui_theme::WINDOW_CONTROLS_CONTENT_INSET
        } else {
            window.rem_size() * 0.875
        };
        self.chat_list.update(cx, |chat, cx| {
            chat.header_left_padding = inset;
            cx.notify();
        });
        self.github.update(cx, |github, cx| {
            github.set_window_controls_inset(self.sidebar_collapsed.then_some(inset), cx);
        });
        cx.notify();
    }

    fn toggle_right_panel_action(
        &mut self,
        _: &ToggleRightPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.right_panel_visible = !self.right_panel_visible;
        self.right_panel.update(cx, |panel, cx| {
            panel.set_visible(self.right_panel_visible, cx);
        });
        if !self.right_panel_visible {
            self.chat_list.update(cx, |chat, cx| chat.focus_composer(window, cx));
        }
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
            let state = self.model.read(cx);
            let project = state.terminal_group_key();
            let unavailable = state.active_worktree_unavailable();
            if !unavailable {
                let terminal = project
                    .as_ref()
                    .and_then(|project| self.terminal_groups.get(project))
                    .and_then(|group| group.tabs.get(group.active_tab))
                    .cloned()
                    .unwrap_or_else(|| self.fallback_terminal(cx));
                let focus = terminal.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    fn begin_new_task_action(
        &mut self,
        _: &BeginNewTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model.update(cx, |state, cx| {
            controller::dispatch(state, AppAction::BeginNewTask);
            cx.notify();
        });
        self.chat_list.update(cx, |chat, cx| {
            chat.focus_composer(window, cx);
        });
        cx.notify();
    }

    fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model.update(cx, |state, cx| {
            controller::dispatch(state, AppAction::OpenSettings);
            cx.notify();
        });
        cx.notify();
    }

    fn cancel_active_generation_action(
        &mut self,
        _: &CancelActiveGeneration,
        window: &mut Window,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_list.update(cx, |chat, cx| {
            chat.set_tab(threadlane_ui_chat::CentralTab::Chat, cx);
        });
    }

    fn select_trajectory_tab_action(
        &mut self,
        _: &SelectTrajectoryTab,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_trajectory_panel(cx);
    }

    fn select_editor_tab_action(
        &mut self,
        _: &SelectEditorTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_list.update(cx, |chat, cx| {
            chat.set_tab(threadlane_ui_chat::CentralTab::Editor, cx);
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
        if let Some((work_dir, number)) = self
            .model
            .update(cx, |state, _cx| state.requested_github_issue.take())
        {
            self.github.update(cx, |github, cx| {
                github.open_linked_task(work_dir, number, cx)
            });
        }
        let workspace_page = self.model.read(cx).workspace_page;
        let link_terminal = if self.bottom_panel_visible && workspace_page == WorkspacePage::Chat {
            self.displayed_terminal(cx).map(|(_, terminal)| terminal)
        } else {
            None
        };
        if self.last_link_terminal != link_terminal {
            if let Some(previous) = self.last_link_terminal.take() {
                previous.update(cx, |terminal, cx| terminal.dismiss_links(cx));
            }
            self.last_link_terminal = link_terminal;
        }
        if self.rendered_page != workspace_page {
            let settings_transition = self.rendered_page == WorkspacePage::Settings
                || workspace_page == WorkspacePage::Settings;
            self.rendered_page = workspace_page;
            if settings_transition {
                self.focus_handle.focus(window, cx);
                cx.on_next_frame(window, |this, window, cx| {
                    if this.model.read(cx).workspace_page == WorkspacePage::Chat {
                        this.chat_list.update(cx, |chat, cx| chat.focus_composer(window, cx));
                    } else if window.last_input_was_keyboard() {
                        // Keyboard entry (cmd-,) moves focus into the page. A click
                        // keeps it on the workspace (Tab still enters the page), so
                        // the first nav row does not draw an unrequested focus ring.
                        window.focus_next(cx);
                    }
                });
            }
        }
        let (terminal_key, terminal_cwd, terminal_unavailable, composer_target) = {
            let state = self.model.read(cx);
            let composer_target = state
                .projects
                .iter()
                .flat_map(|project| project.sessions.iter())
                .find(|session| state.active_session_id.as_deref() == Some(&session.id))
                .map(|session| format!("the \"{}\" draft", session.title))
                .unwrap_or_else(|| "the new-task draft".to_string());
            (
                state.terminal_group_key(),
                state.active_git_work_dir(),
                state.active_worktree_unavailable(),
                composer_target,
            )
        };
        let (terminal_tabs, active_terminal_tab, active_terminal) =
            match (&terminal_key, &terminal_cwd) {
                (Some(key), Some(cwd)) => {
                    let group = self.get_or_create_terminal_group_with_cwd(key, cwd, cx);
                    (
                        group.tabs.clone(),
                        group.active_tab,
                        Some(group.tabs[group.active_tab].clone()),
                    )
                }
                _ if terminal_unavailable => (Vec::new(), 0, None),
                _ => {
                    let fallback = self.fallback_terminal(cx);
                    (vec![fallback.clone()], 0, Some(fallback))
                }
            };
        let terminal_project = terminal_key;
        let new_tab_cwd =
            terminal_cwd.or_else(|| terminal_project.clone());
        let theme = cx.theme().colors;
        let rem = window.rem_size();
        let viewport = window.viewport_size();
        let sidebar_width = self
            .sidebar_resizable_state
            .read(cx)
            .sizes()
            .first()
            .copied()
            .unwrap_or(rem * self.preferred_panel_sizes[0]);
        let layout = threadlane_ui_kit::WorkspaceLayout::new(
            viewport.width,
            rem,
            sidebar_width,
            self.sidebar_collapsed,
            self.right_panel_visible,
        );
        let show_sidebar = layout.sidebar_visible;
        let review_focus = layout.right_panel_focus;
        let visible_panels = [
            workspace_page != WorkspacePage::Settings && show_sidebar,
            workspace_page == WorkspacePage::Chat && self.right_panel_visible && !review_focus,
            workspace_page == WorkspacePage::Chat && self.bottom_panel_visible,
        ];
        let panel_layout = (viewport, rem, visible_panels);
        if self.panel_layout != Some(panel_layout) {
            self.panel_layout = Some(panel_layout);
            // A reopened panel must measure its current container before restoring its split.
            cx.on_next_frame(window, move |this, window, cx| {
                if this.panel_layout != Some(panel_layout) {
                    return;
                }
                threadlane_ui_kit::restore_workspace_panel_sizes(
                    [
                        (
                            visible_panels[0],
                            this.sidebar_resizable_state.clone(),
                            0,
                            this.preferred_panel_sizes[0],
                        ),
                        (
                            visible_panels[1],
                            this.right_panel_resizable_state.clone(),
                            1,
                            this.preferred_panel_sizes[1],
                        ),
                        (
                            visible_panels[2],
                            this.bottom_panel_resizable_state.clone(),
                            1,
                            this.preferred_panel_sizes[2],
                        ),
                    ],
                    rem,
                    window,
                    cx,
                );
                cx.notify();
            });
        }
        let header_inset = layout.header_inset;
        if self.chat_list.read(cx).header_left_padding != header_inset {
            self.chat_list.update(cx, |chat, cx| {
                chat.header_left_padding = header_inset;
                cx.notify();
            });
        }
        // GitHub and Automations headers share the window-controls row, so they
        // need the same inset whenever the sidebar is hidden, including the
        // narrow-window auto-hide.
        let page_header_inset = (!layout.sidebar_visible).then_some(header_inset);
        self.github.update(cx, |github, cx| {
            github.set_window_controls_inset(page_header_inset, cx);
        });
        self.automations.update(cx, |automations, cx| {
            automations.set_header_inset(page_header_inset, cx);
        });

        let environment_width = layout.environment_width;
        self.chat_list.update(cx, |chat, cx| {
            chat.set_environment_width(environment_width, rem, cx);
        });

        let chat_page_content = {
        self.right_panel.update(cx, |panel, cx| {
            panel.set_visible(self.right_panel_visible, cx);
        });
            let upper_content = if review_focus {
                threadlane_ui_kit::workspace_right_panel_focus(
                    self.right_panel.clone(),
                    cx.listener(|this, _, window, cx| {
                        this.toggle_right_panel_action(&ToggleRightPanel, window, cx);
                    }),
                    cx,
                ).into_any_element()
            } else if self.right_panel_visible {
                threadlane_ui_kit::workspace_right_panel_split(
                    &self.right_panel_resizable_state, self.chat_list.clone(), self.right_panel.clone(), rem, viewport.width,
                )
                    .on_resize(cx.listener(|this, state: &Entity<ResizableState>, window, cx| {
                        if let Some(size) = state.read(cx).sizes().get(1) {
                            this.preferred_panel_sizes[1] = *size / window.rem_size();
                        }
                    }))
                    .into_any_element()
            } else {
                self.chat_list.clone().into_any_element()
            };

            let main_content = if self.bottom_panel_visible {
                if terminal_unavailable {
                    let owner = cx.entity().clone();
                    let terminal_panel = threadlane_ui_kit::terminal_unavailable(
                        move |action, _, cx| {
                            owner.update(cx, |this, cx| {
                                match action {
                                    threadlane_ui_kit::TerminalAction::Hide => {
                                        this.bottom_panel_visible = false
                                    }
                                    threadlane_ui_kit::TerminalAction::RecreateWorktree => {
                                        this.model.update(cx, |state, cx| {
                                            controller::dispatch(state, AppAction::RecreateActiveWorktree);
                                            cx.notify();
                                        });
                                    }
                                    threadlane_ui_kit::TerminalAction::UseProjectFolder => {
                                        this.model.update(cx, |state, cx| {
                                            if let Some(work_dir) = state.active_work_dir.clone() {
                                                controller::dispatch(
                                                    state,
                                                    AppAction::SelectDraftProject(work_dir),
                                                );
                                            }
                                            cx.notify();
                                        });
                                    }
                                    _ => return,
                                }
                                cx.notify();
                            })
                        },
                        cx,
                    );
                    threadlane_ui_kit::workspace_terminal_split(
                        &self.bottom_panel_resizable_state,
                        upper_content,
                        terminal_panel,
                        rem,
                        viewport.height,
                    )
                    .on_resize(
                        cx.listener(|this, state: &Entity<ResizableState>, window, cx| {
                            if let Some(size) = state.read(cx).sizes().get(1) {
                                this.preferred_panel_sizes[2] = *size / window.rem_size();
                            }
                        }),
                    )
                    .into_any_element()
                } else {
                    let tab_buttons = terminal_tabs
                        .iter()
                        .enumerate()
                        .map(|(tab, terminal)| {
                            let project = terminal_project.clone();
                            let cwd = new_tab_cwd.clone();
                            let terminal = terminal.clone();
                            let owner = cx.entity().clone();
                            let hint = format!(
                                "Shell {} · launched in {}",
                                tab + 1,
                                terminal.read(cx).project().display()
                            );
                            let armed = project.as_ref().is_some_and(|project| {
                                self.pending_terminal_close == Some((project.clone(), terminal.entity_id()))
                            });
                            threadlane_ui_kit::TerminalTab::new(
                                format!("terminal-{:?}", terminal.entity_id()),
                                format!("Shell {}", tab + 1),
                                hint,
                            )
                            .selected(tab == active_terminal_tab)
                            .close_armed(armed)
                            .project_actions(project.is_some())
                            .close_others(terminal_tabs.len() > 1)
                            .render(move |action, window, cx| {
                                owner.update(cx, |this, cx| {
                                    this.request_terminal_tab(
                                        action,
                                        project.as_ref(),
                                        cwd.as_ref(),
                                        &terminal,
                                        window,
                                        cx,
                                    );
                                })
                            })
                            .into_any_element()
                        })
                        .collect();
                    let active_terminal = active_terminal.expect("available terminal");
                    let excerpt_block =
                        terminal_excerpt_block_reason(active_terminal.read(cx).selection_status());
                    let handoff_hint = excerpt_block.map(str::to_owned).unwrap_or_else(|| {
                        format!("Add the selected terminal text to {composer_target} — nothing is sent")
                    });
                    let shell_cwd = active_terminal.read(cx).project().clone();
                    let worktree_shell = path_is_threadlane_worktree(&shell_cwd);
                    let project = terminal_project.clone();
                    let cwd = new_tab_cwd.clone();
                    let terminal = active_terminal.clone();
                    let owner = cx.entity().clone();
                    let toolbar = threadlane_ui_kit::TerminalToolbar::new(shell_cwd_label(&shell_cwd))
                        .path_hint(shell_cwd.display().to_string())
                        .worktree(worktree_shell)
                        .tabs(tab_buttons)
                        .new_tab(project.is_some())
                        .selection(excerpt_block.is_none(), handoff_hint)
                        .shortcuts(
                            if cfg!(target_os = "macos") {
                                "Find in terminal output (Cmd+F)"
                            } else {
                                "Find in terminal output (Ctrl+Shift+F)"
                            },
                            if cfg!(target_os = "macos") {
                                "Hide terminal (Cmd+J)"
                            } else {
                                "Hide terminal (Ctrl+J)"
                            },
                        )
                        .render(
                            move |action, window, cx| {
                                owner.update(cx, |this, cx| {
                                    this.request_terminal_action(
                                        action,
                                        project.clone(),
                                        cwd.clone(),
                                        terminal.clone(),
                                        window,
                                        cx,
                                    );
                                })
                            },
                            cx,
                        );
                    let terminal_panel = threadlane_ui_kit::terminal_surface(cx)
                        .child(toolbar)
                        .child(div().flex_1().min_h_0().child(active_terminal));

                    threadlane_ui_kit::workspace_terminal_split(
                        &self.bottom_panel_resizable_state,
                        upper_content,
                        terminal_panel,
                        rem,
                        viewport.height,
                    )
                    .on_resize(
                        cx.listener(|this, state: &Entity<ResizableState>, window, cx| {
                            if let Some(size) = state.read(cx).sizes().get(1) {
                                this.preferred_panel_sizes[2] = *size / window.rem_size();
                            }
                        }),
                    )
                    .into_any_element()
                }
            } else {
                upper_content
            };

            main_content
        };

        let central_content = match workspace_page {
            WorkspacePage::Chat => chat_page_content.into_any_element(),
            WorkspacePage::GitHub => self.github.clone().into_any_element(),
            WorkspacePage::Automations => self.automations.clone().into_any_element(),
            WorkspacePage::Settings => self.settings.clone().into_any_element(),
        };
        let page_content = if workspace_page != WorkspacePage::Settings && show_sidebar {
            threadlane_ui_kit::workspace_sidebar_split(
                &self.sidebar_resizable_state, self.sidebar.clone(), central_content, rem,
            )
                .on_resize(cx.listener(|this, state: &Entity<ResizableState>, window, cx| {
                    if let Some(size) = state.read(cx).sizes().first() {
                        this.preferred_panel_sizes[0] = *size / window.rem_size();
                    }
                }))
                .into_any_element()
        } else {
            central_content
        };

        // One full-width status bar on every page; GitHub contributes its list
        // summary to it instead of drawing a second, content-width bar.
        let view_with_status_bar =
            threadlane_ui_kit::workspace_with_status(page_content, self.render_status_bar(cx))
                .into_any_element();

        self.right_panel
            .update(cx, |panel, cx| panel.sync_git_dialog(window, cx));

        div()
            .id("workspace-root")
            .tab_group()
            .key_context("ThreadlaneWorkspace")
            .relative()
            .flex()
            .w_full()
            .h_full()
            .track_focus(&self.focus_handle)
            .role(Role::Application)
            .on_action(cx.listener(Self::toggle_command_palette))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::toggle_right_panel_action))
            .on_action(
                cx.listener(|this, _: &threadlane_ui_chat::OpenWorkspaceReview, _, cx| {
                    this.open_git_review(cx);
                }),
            )
            .on_action(cx.listener(
                |this, _: &threadlane_ui_chat::OpenWorkspaceBranches, _, cx| {
                    this.open_git_branches(cx);
                },
            ))
            .on_action(cx.listener(
                |this, _: &threadlane_ui_chat::OpenWorkspaceTrajectory, _, cx| {
                    this.open_trajectory_panel(cx);
                },
            ))
            .on_action(cx.listener(
                |this, _: &threadlane_ui_chat::OpenWorkspaceCommit, window, cx| {
                    this.open_git_review(cx);
                    this.right_panel
                        .update(cx, |panel, cx| panel.open_commit(window, cx));
                },
            ))
            .on_action(cx.listener(
                |this, _: &threadlane_ui_chat::PullWorkspaceBranch, window, cx| {
                    this.open_git_review(cx);
                    this.right_panel.update(cx, |panel, cx| {
                        panel.run_git_action(
                            threadlane_ui_right_panel::GitAction::Pull,
                            window,
                            cx,
                        );
                    });
                },
            ))
            .on_action(cx.listener(
                |this, _: &threadlane_ui_chat::PushWorkspaceBranch, window, cx| {
                    this.open_git_review(cx);
                    this.right_panel.update(cx, |panel, cx| {
                        panel.run_git_action(
                            threadlane_ui_right_panel::GitAction::Push,
                            window,
                            cx,
                        );
                    });
                },
            ))
            .on_action(cx.listener(
                |this, _: &threadlane_ui_chat::CreateWorkspacePullRequest, window, cx| {
                    this.open_git_review(cx);
                    this.right_panel
                        .update(cx, |panel, cx| panel.open_draft_pr_dialog(window, cx));
                },
            ))
            .on_action(cx.listener(
                |this, _: &threadlane_ui_chat::CreateWorkspaceBranch, window, cx| {
                    this.open_git_new_branch(window, cx);
                },
            ))
            .on_action(
                cx.listener(|this, _: &threadlane_ui_chat::OpenWorkspaceFiles, _, cx| {
                    this.right_panel_visible = true;
                    this.right_panel.update(cx, |panel, cx| {
                        panel.open_surface(threadlane_ui_right_panel::Surface::Files, cx);
                    });
                    cx.notify();
                }),
            )
            .on_action(
                cx.listener(|this, _: &threadlane_ui_chat::OpenWorkspaceAgents, _, cx| {
                    this.open_agents_panel(cx);
                }),
            )
            .on_action(cx.listener(Self::switch_session_action))
            .on_action(cx.listener(Self::toggle_terminal_action))
            .on_action(cx.listener(Self::begin_new_task_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::activate_update))
            .on_action(cx.listener(|this, _: &threadlane_ui_settings::ActivateUpdate, window, cx| {
                this.activate_update(&threadlane_ui_sidebar::ActivateUpdate, window, cx);
            }))
            .on_action(cx.listener(Self::cancel_active_generation_action))
            .on_action(cx.listener(Self::select_chat_tab_action))
            .on_action(cx.listener(Self::select_trajectory_tab_action))
            .on_action(cx.listener(Self::select_editor_tab_action))
            .on_action(cx.listener(Self::focus_composer_action))
            .bg(theme.background)
            .child(view_with_status_bar)
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("command-palette-btn")
                    .accessibility_label("Command palette (Cmd+K)")
                    .icon(IconName::SquareTerminal)
                    .tooltip("Command palette (Cmd+K)")
                    .ghost()
                    .selected(self.command_palette_open)
                    .xsmall()
                    .absolute()
                    .top(rems(0.5625))
                    .right_12()
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.toggle_command_palette(&ToggleCommandPalette, window, cx);
                    }))
            }))
            .children((workspace_page == WorkspacePage::Chat).then(|| {
                Button::new("right-panel-toggle")
                        .accessibility_label(if self.right_panel_visible {
                            "Hide right panel"
                        } else {
                            "Show right panel"
                        })
                    .icon(IconName::PanelRight)
                    .tooltip(if self.right_panel_visible {
                        "Hide right panel"
                    } else {
                        "Show right panel"
                    })
                    .ghost()
                    .xsmall()
                    .absolute()
                    .top(rems(0.5625))
                    .right_3()
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.toggle_right_panel_action(&ToggleRightPanel, window, cx);
                    }))
            }))
            .children((workspace_page != WorkspacePage::Settings).then(|| {
                threadlane_ui_kit::workspace_sidebar_toggle(self.sidebar_collapsed, layout.sidebar_available)
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.toggle_sidebar_action(&ToggleSidebar, window, cx);
                    }))
            }))
            .children(self.command_palette_open.then(|| {
                if self.session_picker.is_some() {
                    self.render_session_picker(cx)
                } else if self.conversation_search.is_some() {
                    self.render_conversation_search(cx).into_any_element()
                } else {
                    self.render_command_palette(cx).into_any_element()
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        active_project_git_status, format_terminal_excerpt, git_result_matches_active,
        next_workspace_event, open_github_from_palette, path_is_threadlane_worktree,
        session_pr_refresh_delay, session_pr_target_is_active, shell_cwd_label,
        terminal_excerpt_block_reason, GitEvent, WorkspacePumpEvent, TERMINAL_EXCERPT_LIMIT,
    };
    use threadlane_ui_terminal::SelectionStatus;
    use threadlane_ui_state::updater::UpdaterEvent;
    use threadlane_ui_state::{AppState, SessionInfo, WorkspacePage};
    use std::cell::Cell;
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use threadlane_git::GitStatus;

    #[gpui::test]
    fn quit_uses_close_guard_with_and_without_a_focused_window(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, InteractiveElement as _, StatefulInteractiveElement as _};
        use gpui_component::{Root, WindowExt as _};
        use std::rc::Rc;

        struct QuitHost(gpui::FocusHandle);
        impl gpui::Render for QuitHost {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                _: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                gpui::div()
                    .id("quit-host")
                    .role(gpui::Role::Application)
                    .track_focus(&self.0)
            }
        }
        let fallback = Rc::new(Cell::new(false));
        let tracked = fallback.clone();
        cx.update(move |cx| {
            gpui_component::init(cx);
            cx.bind_keys([gpui::KeyBinding::new("cmd-q", super::QuitThreadlane, None)]);
            cx.on_action(move |_: &super::QuitThreadlane, _| tracked.set(true));
        });
        let allowed = Rc::new(Cell::new(false));
        let calls = Rc::new(Cell::new(0));
        let close_allowed = allowed.clone();
        let close_calls = calls.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            super::install_window_close_handler(window, cx, move |window, cx| {
                close_calls.set(close_calls.get() + 1);
                if close_allowed.get() {
                    return true;
                }
                if !window.has_active_dialog(cx) {
                    window.open_alert_dialog(cx, |dialog, _, _| dialog.title("Active work").confirm());
                }
                false
            });
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Root::new(cx.new(|_| QuitHost(focus)), window, cx)
        });
        cx.simulate_keystrokes("cmd-q");
        cx.run_until_parked();
        cx.update(|window, cx| assert!(window.has_active_dialog(cx)));
        assert_eq!(calls.get(), 1);
        assert!(
            !fallback.get(),
            "the startup handler must never bypass the close guard"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| assert!(!window.has_active_dialog(cx)));

        cx.deactivate_window();
        cx.cx.update(|cx| {
            assert!(cx.active_window().is_none());
            cx.dispatch_action(&super::QuitThreadlane);
        });
        cx.run_until_parked();
        cx.update(|window, cx| assert!(window.has_active_dialog(cx)));
        assert_eq!(calls.get(), 2);
        assert!(
            !fallback.get(),
            "an unfocused window must retain its close guard"
        );

        allowed.set(true);
        cx.cx
            .update(|cx| cx.dispatch_action(&super::QuitThreadlane));
        cx.run_until_parked();
        cx.cx.update(|cx| assert!(cx.windows().is_empty()));
        assert_eq!(calls.get(), 3);
        assert!(!fallback.get());
    }

    #[gpui::test]
    fn palette_dismissal_restores_focus_once(cx: &mut gpui::TestAppContext) {
        use gpui::{InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _};
        struct FocusHost(gpui::FocusHandle, gpui::FocusHandle);
        impl gpui::Render for FocusHost {
            fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
                gpui::div()
                    .child(gpui::div().id("trigger").role(gpui::Role::Button).track_focus(&self.0))
                    .child(gpui::div().id("palette").role(gpui::Role::Button).track_focus(&self.1))
            }
        }
        let (view, cx) = cx.add_window_view(|_, cx| FocusHost(cx.focus_handle(), cx.focus_handle()));
        cx.run_until_parked();
        cx.update(|window, cx| {
            let trigger = view.read(cx).0.clone();
            let palette = view.read(cx).1.clone();
            palette.focus(window, cx);
            let mut open = true;
            let mut previous = Some(trigger.clone());
            super::close_command_palette(&mut open, &mut previous, window, cx);
            assert!(!open);
            assert!(trigger.is_focused(window));
            palette.focus(window, cx);
            super::close_command_palette(&mut open, &mut previous, window, cx);
            assert!(palette.is_focused(window), "a repeated dismissal must not restore stale focus");
        });
    }

    #[gpui::test]
    fn escape_dismisses_dialog_before_cancelling_generation(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, InteractiveElement as _, Styled as _};
        use gpui_component::{Root, WindowExt as _};
        use std::rc::Rc;
        struct DialogHost(Rc<Cell<bool>>);
        impl gpui::Render for DialogHost {
            fn render(&mut self, _window: &mut gpui::Window, _cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
                let cancelled = self.0.clone();
                gpui::div().size_full().key_context("ThreadlaneWorkspace")
                    .on_action(move |_: &super::CancelActiveGeneration, _, _| cancelled.set(true))
            }
        }
        cx.update(|cx| { gpui_component::init(cx); super::init(cx); });
        let cancelled = Rc::new(Cell::new(false));
        let tracked = cancelled.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            Root::new(cx.new(|_| DialogHost(tracked)), window, cx)
        });
        cx.update(|window, cx| window.open_dialog(cx, |dialog, _, _| dialog.title("Test dialog")));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| assert!(!window.has_active_dialog(cx)));
        assert!(!cancelled.get(), "dialog dismissal must not cancel the underlying turn");
    }

    #[gpui::test]
    fn escape_dismisses_menu_before_cancelling_generation(cx: &mut gpui::TestAppContext) {
        use gpui::{Focusable as _, InteractiveElement as _, StatefulInteractiveElement as _, ParentElement as _, Styled as _};
        use gpui_component::menu::PopupMenu;
        use std::rc::Rc;
        struct MenuHost(gpui::Entity<PopupMenu>, Rc<Cell<bool>>, gpui::FocusHandle);
        impl gpui::Render for MenuHost {
            fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
                let stopped = self.1.clone();
                gpui::div().id("menu-test-workspace").size_full().key_context("ThreadlaneWorkspace")
                    .track_focus(&self.2).role(gpui::Role::Application)
                    .on_action(move |_: &super::CancelActiveGeneration, _, _| stopped.set(true))
                    .child(self.0.clone())
            }
        }
        cx.update(|cx| { gpui_component::init(cx); super::init(cx); });
        let stopped = Rc::new(Cell::new(false));
        let tracked = stopped.clone();
        let (host, cx) = cx.add_window_view(move |window, cx| {
            let focus = cx.focus_handle();
            let menu = PopupMenu::build(window, cx, |menu, _, _| menu.label("Test menu").action_context(focus.clone()));
            menu.read(cx).focus_handle(cx).focus(window, cx);
            MenuHost(menu, tracked, focus)
        });
        let dismissed = Rc::new(Cell::new(false));
        let tracked = dismissed.clone();
        let _subscription = cx.update(|_, cx| { let menu = host.read(cx).0.clone(); cx.subscribe(&menu, move |_, _: &gpui::DismissEvent, _| tracked.set(true)) });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(dismissed.get(), "Escape must dismiss the native menu");
        assert!(!stopped.get(), "menu dismissal must not stop the underlying turn");
        cx.simulate_keystrokes("escape");
        assert!(stopped.get(), "Escape still stops a turn after focus returns to the workspace");
    }

    #[gpui::test]
    fn terminal_links_reject_hidden_switched_and_unsafe_sources(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        use threadlane_ui_state::WorkspacePage;
        let first = cx.new(|_| 1u8);
        let second = cx.new(|_| 2u8);
        let accepts = |displayed, visible, page, url| super::terminal_link_is_current(
            displayed, first.entity_id(), visible, page, url,
        );
        assert!(accepts(Some(first.entity_id()), true, WorkspacePage::Chat, "http://0.0.0.0:3000/"));
        assert!(!accepts(Some(second.entity_id()), true, WorkspacePage::Chat, "http://host/"));
        assert!(!accepts(None, true, WorkspacePage::Chat, "http://host/"));
        assert!(!accepts(Some(first.entity_id()), false, WorkspacePage::Chat, "http://host/"));
        assert!(!accepts(Some(first.entity_id()), true, WorkspacePage::Settings, "http://host/"));
        for url in ["file:///tmp/a", "localhost:3000", "http://user@host/", "https://host/\n"] {
            assert!(!accepts(Some(first.entity_id()), true, WorkspacePage::Chat, url));
        }
    }

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
        assert_eq!(session_pr_refresh_delay(true, true, true).as_secs(), 120);
        assert_eq!(session_pr_refresh_delay(true, false, true).as_secs(), 600);
        assert_eq!(session_pr_refresh_delay(true, true, false).as_secs(), 1800);
        assert_eq!(session_pr_refresh_delay(true, false, false).as_secs(), 1800);
        assert_eq!(session_pr_refresh_delay(false, true, true).as_secs(), 600);
    }

    #[test]
    fn github_palette_action_notifies_model_observers() {
        let notification_count = Cell::new(0);
        let mut state = AppState::default();

        open_github_from_palette(&mut state, || {
            notification_count.set(notification_count.get() + 1)
        });

        assert_eq!(state.workspace_page, WorkspacePage::GitHub);
        assert_eq!(notification_count.get(), 1);
    }

    #[test]
    fn terminal_excerpt_block_reason_reports_disabled_states() {
        let status = |has_text, excerpt_len| SelectionStatus {
            has_text,
            excerpt_len,
        };

        assert_eq!(
            terminal_excerpt_block_reason(None),
            Some("Select terminal output first")
        );
        assert_eq!(
            terminal_excerpt_block_reason(Some(status(false, 4))),
            Some("The terminal selection is empty — select output text first")
        );
        assert_eq!(
            terminal_excerpt_block_reason(Some(status(true, TERMINAL_EXCERPT_LIMIT + 1))),
            Some("Select less terminal output (maximum 32 KiB)")
        );
        assert_eq!(
            terminal_excerpt_block_reason(Some(status(true, 17))),
            None
        );
    }

    #[test]
    fn terminal_excerpt_labels_the_shell_and_outfences_the_text() {
        let block = format_terminal_excerpt(2, Path::new("/repo/worktree"), "a `tick` and ```run");
        assert!(block.starts_with("Terminal · Shell 2 · launched in /repo/worktree\n````\n"));
        assert!(block.contains("\na `tick` and ```run\n"));
        assert!(block.ends_with("\n````"));

        // Ordinary text still gets the minimum three-backtick fence.
        let plain = format_terminal_excerpt(1, Path::new("/repo"), "line");
        assert_eq!(plain, "Terminal · Shell 1 · launched in /repo\n```\nline\n```");
    }

    #[test]
    fn shell_cwd_label_marks_threadlane_worktrees() {
        assert!(path_is_threadlane_worktree(Path::new(
            "/repo/.threadlane/worktrees/agent-7"
        )));
        assert!(!path_is_threadlane_worktree(Path::new("/repo")));
        assert!(!path_is_threadlane_worktree(Path::new(
            "/repo/.threadlane/sessions"
        )));

        assert_eq!(
            shell_cwd_label(Path::new("/repo/.threadlane/worktrees/agent-7")),
            ".threadlane/worktrees/agent-7"
        );
        assert_eq!(shell_cwd_label(Path::new("/repo")), "repo");
        assert_eq!(shell_cwd_label(Path::new("/")), "/");
    }

    #[tokio::test]
    async fn workspace_pump_waits_for_a_real_producer_event() {
        let (_git_tx, mut git_rx) = tokio::sync::mpsc::unbounded_channel::<GitEvent>();
        let (_updater_tx, mut updater_rx) = tokio::sync::mpsc::unbounded_channel::<UpdaterEvent>();
        let (_sessions_tx, mut sessions_rx) =
            tokio::sync::mpsc::unbounded_channel::<(u64, PathBuf, Vec<SessionInfo>)>();
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
