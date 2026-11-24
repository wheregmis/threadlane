//! `threadlane-desktop`: the desktop app (Plan.md §11.1).
//!
//! The shell and its screens over [`AppState`]: Home, a project with its
//! tasks, conversation and agents, a task, the agents of each space, and the
//! machines. Decisions are answered from Home, a project, a task, or a
//! system notification. Tokio runs on its own threads beside GPUI's executor
//! (Plan.md §18.4 item 3): every hub call is spawned there and its result
//! handed back to the view. The screens live in `screens/`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::process::{Command, ExitCode};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, InputState, TextareaState, Undo};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, Theme, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_client::{ClientCore, ClientDirs, SpaceFeedItem};
use threadlane_core::client::{ActivityCursor, TaskActivityEntry};
use threadlane_core::ids::{DecisionId, MachineId, TaskId};
use threadlane_core::model::Mode;
use threadlane_core::time::{Clock as _, SystemClock, Timestamp};
use threadlane_desktop::{
    Activity, AppState, Appearance, DecisionKey, RoutineText, Tab, When, change_memory,
    decision_of_tag, describe, load_earlier, load_memory, load_modes, load_project_tab,
    load_routines, load_task_overview, load_timeline, mode_label, pump, refresh_each,
};
use threadlane_desktop::{DreamingAction, change_dreaming, load_dreaming, load_metrics};
use tokio::runtime::Runtime;
use tokio::sync::{RwLock, mpsc};

mod actions;
mod components;
mod icons;
mod keyboard;
mod local_hub;
mod menus;
mod navigation;
mod palette;
mod refresh_gate;
mod repository;
mod screens;
mod tokens;
mod updates;
mod work;
use work::{WorkPage, WorkView};

use icons::DesktopAssets;
use keyboard::{DecisionScroll, HistoryScroll};
use refresh_gate::RefreshGate;
use tokens::{
    LG, MD, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH, SIDEBAR_WIDTH, TABULAR, TYPE_BODY,
    WINDOW_DEFAULT_HEIGHT, WINDOW_DEFAULT_WIDTH, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH,
};

/// Unique notification slot for transient desktop activity feedback.
struct ActivityToast;

/// How often the app asks every hub again, so reachability, what runs, and
/// the open screen stay current.
const REFRESH_EVERY: Duration = Duration::from_secs(30);

/// How soon the app asks again while a hub is away or refused a part.
const RETRY_EVERY: Duration = Duration::from_secs(5);

/// Repaint visible elapsed times without waiting for a hub refresh.
const CLOCK_EVERY: Duration = Duration::from_secs(1);

const HUB_DIR_HINT: &str = "Enter a space name or choose a folder";

fn main() -> ExitCode {
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("threadlane-desktop: cannot start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let dirs = match ClientDirs::from_env() {
        Ok(dirs) => dirs,
        Err(error) => {
            eprintln!("threadlane-desktop: {error}");
            return ExitCode::FAILURE;
        }
    };
    let client_dir = dirs.root.clone();
    let core = match ClientCore::load(dirs) {
        Ok(core) => core,
        Err(error) => {
            eprintln!("threadlane-desktop: {error}");
            return ExitCode::FAILURE;
        }
    };
    local_hub::start_missing(&client_dir, core.hubs());
    // The paired hubs are known without asking them: the first frame lists them.
    let mut state = AppState::paired(core.hubs().iter().map(|hub| hub.name.as_str()));
    state.set_client_dir(client_dir);
    let appearance = state.appearance();
    let (shown, feed) = mpsc::unbounded_channel();
    // With no hub paired there is no feed; Home says so.
    let stream = {
        let _inside = runtime.enter();
        core.follow_all().ok()
    };
    let core = Arc::new(RwLock::new(core));
    if let Some(stream) = stream {
        runtime.spawn(pump(Arc::clone(&core), stream, shown.clone()));
    }
    let tokio = runtime.handle().clone();
    let open = std::env::var("THREADLANE_DESKTOP_OPEN").ok();
    let start = open.as_deref().and_then(Screen::named);
    // `<space>/<project>/agents/<agent>` also expands that agent's row.
    if let Some([space, project, "agents", agent]) = open
        .as_deref()
        .map(|open| open.split('/').collect::<Vec<_>>())
        .as_deref()
    {
        state.toggle_agent(space, project, agent);
    }

    application().with_assets(DesktopAssets).run(move |cx| {
        cx.set_app_identity("dev.threadlane.desktop", "Threadlane");
        init(cx);
        cx.set_window_appearance(appearance_override(appearance));
        cx.on_window_closed(|cx, _window| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let initial_size = state.window_size().map_or_else(
            || size(WINDOW_DEFAULT_WIDTH, WINDOW_DEFAULT_HEIGHT),
            |(w, h)| size(px(w).max(WINDOW_MIN_WIDTH), px(h).max(WINDOW_MIN_HEIGHT)),
        );
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(initial_size, cx)),
            window_min_size: Some(size(WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)),
            titlebar: Some(TitlebarOptions {
                title: Some("Threadlane".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let opened = open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                let feed = Feed {
                    shown,
                    received: feed,
                };
                let mut desktop = Desktop::new(state, core, tokio, feed, start, window, cx);
                desktop.match_hub_builds(cx);
                desktop
            })
        });
        match opened {
            Ok((_window, view)) => {
                let view = view.downgrade();
                let reload = view.clone();
                let appearance_view = view.clone();
                menus::install(
                    appearance,
                    view.clone(),
                    cx,
                    move |cx| {
                        let _gone = reload.update(cx, |this, cx| this.refresh(cx));
                    },
                    move |appearance, window, cx| {
                        let _gone = appearance_view.update(cx, |this, cx| {
                            if let Err(why) = this.state.set_appearance(appearance) {
                                this.state.report(Activity::Failed(why));
                            }
                            cx.set_window_appearance(appearance_override(appearance));
                            apply_appearance(appearance, window, cx);
                            menus::set(this.full_screen, appearance, cx);
                            cx.notify();
                        });
                    },
                );
                cx.on_system_notification_response(move |response, cx| {
                    let Some((space, id)) = decision_of_tag(&response.tag) else {
                        return;
                    };
                    let (space, id) = (space.to_owned(), id.to_owned());
                    let option = response.action_id.map(|option| option.to_string());
                    let _gone = view.update(cx, |this, cx| match option {
                        Some(option) => this.answer(space, id, option, None, cx),
                        None => this.go(Screen::Home, cx),
                    });
                });
            }
            Err(error) => {
                eprintln!("threadlane-desktop: cannot open the window: {error}");
                cx.quit();
            }
        }
    });
    drop(runtime);
    ExitCode::SUCCESS
}

fn appearance_override(appearance: Appearance) -> Option<WindowAppearance> {
    match appearance {
        Appearance::System => None,
        Appearance::Light => Some(WindowAppearance::Light),
        Appearance::Dark => Some(WindowAppearance::Dark),
    }
}

fn apply_appearance(appearance: Appearance, window: &mut Window, cx: &mut App) {
    if let Some(appearance) = appearance_override(appearance) {
        Theme::change(appearance, Some(window), cx);
    } else {
        Theme::sync_system_appearance(Some(window), cx);
    }
}

/// Local time minus UTC in minutes, as `date +%z` says; UTC if it cannot.
fn local_offset_minutes() -> i64 {
    let Ok(output) = Command::new("date").arg("+%z").output() else {
        return 0;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    let (sign, digits) = match text.split_at_checked(1) {
        Some(("-", digits)) => (-1, digits),
        Some(("+", digits)) => (1, digits),
        _ => return 0,
    };
    let (Some(hours), Some(minutes)) = (
        digits.get(..2).and_then(|h| h.parse::<i64>().ok()),
        digits.get(2..4).and_then(|m| m.parse::<i64>().ok()),
    ) else {
        return 0;
    };
    sign * (hours * 60 + minutes)
}

/// This computer's time zone name, as `threadlane routine add` finds it:
/// `TZ`, else where `/etc/localtime` points.
fn local_zone() -> Option<String> {
    if let Ok(zone) = std::env::var("TZ")
        && !zone.trim().is_empty()
    {
        return Some(zone.trim().trim_start_matches(':').to_owned());
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let (_, zone) = target.to_str()?.split_once("zoneinfo/")?;
    Some(zone.to_owned())
}

/// Which screen the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    Home,
    /// One project of one space, on one of its tabs.
    Project {
        space: String,
        project: String,
        tab: Tab,
    },
    /// One task of one space.
    Task {
        space: String,
        task: TaskId,
    },
    /// Agents in the selected space, or all spaces from the debug route.
    Agents(Option<String>),
    /// Work across the projects in one space.
    SpaceTasks {
        space: String,
    },
    /// A form for a new project in one space.
    NewProject {
        space: String,
    },
    /// The spaces and their machines.
    Machines,
    /// One machine in a space.
    Machine {
        space: String,
        machine: MachineId,
    },
    /// Routines in the selected space, or all spaces from the debug route.
    Routines(Option<String>),
    Work {
        space: String,
        page: WorkPage,
    },
    /// One space's settings, or preferences for this app.
    Settings(Option<String>),
    /// Pair another space with this app.
    AddSpace,
}

impl Screen {
    /// The space this screen belongs to, if it belongs to one.
    fn space(&self) -> Option<&str> {
        match self {
            Self::Work { space, .. }
            | Self::Project { space, .. }
            | Self::Task { space, .. }
            | Self::SpaceTasks { space }
            | Self::NewProject { space }
            | Self::Machine { space, .. }
            | Self::Agents(Some(space))
            | Self::Routines(Some(space))
            | Self::Settings(Some(space)) => Some(space),
            _ => None,
        }
    }

    /// Unscoped views load auxiliary data once the full roster is refreshed.
    fn loads_after_refresh_pass(&self) -> bool {
        matches!(self, Self::Home | Self::Agents(None) | Self::Routines(None))
    }

    /// The screen `THREADLANE_DESKTOP_OPEN` names: `home` or `inbox`, `agents`,
    /// `machines`, `<space>/<project>[/chat|tasks|team|routines|memory|setup]`
    /// or `<space>/T-0001`. For looking at a screen without clicking to it.
    fn named(open: &str) -> Option<Self> {
        let parts: Vec<&str> = open.split('/').collect();
        Some(match parts.as_slice() {
            ["home" | "inbox"] => Self::Home,
            ["agents"] => Self::Agents(None),
            ["machines"] => Self::Machines,
            ["routines"] => Self::Routines(None),
            ["settings"] => Self::Settings(None),
            [space, task] if TaskId::parse(task).is_some() && task.starts_with(['T', 't']) => {
                Self::Task {
                    space: (*space).to_owned(),
                    task: TaskId::parse(task)?,
                }
            }
            [space, project, rest @ ..] => Self::Project {
                space: (*space).to_owned(),
                project: (*project).to_owned(),
                tab: match rest {
                    [name, ..] => Tab::named(name).unwrap_or(Tab::Conversation),
                    _ => Tab::Conversation,
                },
            },
            _ => return None,
        })
    }
}

/// What a click asks of a routine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerChange {
    Pause,
    Resume,
    Cancel,
}

/// The feed as the window takes it: what the pumps hand over, and the
/// sender a newly paired space's pump hands its items to.
struct Feed {
    shown: mpsc::UnboundedSender<SpaceFeedItem>,
    received: mpsc::UnboundedReceiver<SpaceFeedItem>,
}

#[derive(Default)]
struct TaskActivity {
    entries: Vec<TaskActivityEntry>,
    next: Option<ActivityCursor>,
    loading: bool,
    error: Option<(Option<ActivityCursor>, String)>,
    loaded: bool,
    newest: Option<(Vec<TaskActivityEntry>, Option<ActivityCursor>)>,
}

impl TaskActivity {
    fn apply_page(
        &mut self,
        before: Option<ActivityCursor>,
        entries: Vec<TaskActivityEntry>,
        next: Option<ActivityCursor>,
    ) {
        if before.is_some() {
            self.entries.extend(entries);
            self.next = next;
        } else if let Some(overlap) = entries.last().and_then(|last| {
            self.entries
                .iter()
                .position(|entry| last.position == entry.position)
        }) {
            self.entries.splice(..=overlap, entries);
            self.newest = None;
        } else if self.entries.is_empty() && (!entries.is_empty() || !self.loaded) {
            self.entries = entries;
            self.next = next;
            self.newest = None;
        } else if !entries.is_empty() {
            self.newest = Some((entries, next));
        }
        self.loaded = true;
    }

    fn show_newest(&mut self) {
        if let Some((entries, next)) = self.newest.take() {
            self.entries = entries;
            self.next = next;
            self.error = None;
        }
    }

    fn is_empty(&self) -> bool {
        self.loaded
            && self.entries.is_empty()
            && self.next.is_none()
            && self.error.is_none()
            && self.newest.is_none()
    }
}

#[derive(Default)]
struct PendingProjectLoad {
    refreshed: (bool, bool),
    latest: (bool, bool),
}

impl PendingProjectLoad {
    fn queue(&mut self, parts: (bool, bool)) {
        self.refreshed.0 |= parts.0;
        self.refreshed.1 |= parts.1;
        self.latest = parts;
    }
}

/// The window's one view: the shell and its screens over [`AppState`].
struct Desktop {
    state: threadlane_desktop::ConversationState,
    desktop_updatable: bool,
    updates: HashMap<Option<String>, updates::UpdateState>,
    core: Arc<RwLock<ClientCore>>,
    tokio: tokio::runtime::Handle,
    screen: Screen,
    nav_history: navigation::NavHistory,
    work: WorkView,
    inbox: screens::Inbox,
    task_activity: TaskActivity,
    task_activity_request: Option<Task<()>>,
    refresh_gate: RefreshGate,
    project_load: Option<Task<()>>,
    project_load_pending: Option<PendingProjectLoad>,
    keyboard_focus: FocusHandle,
    sidebar_collapsed: bool,
    sidebar_width: Pixels,
    sidebar_resizable: Entity<ResizableState>,
    last_activity_revision: u64,
    join_error: Option<String>,
    routine_error: Option<String>,
    decision_scroll: ScrollHandle,
    board_scroll: ScrollHandle,
    dag_scroll: ScrollHandle,
    topo_scroll: ScrollHandle,
    reveal_linked_decision: Option<DecisionKey>,
    conversation_at_bottom: bool,
    conversation_follow: Rc<Cell<bool>>,
    history_scroll: Rc<RefCell<HistoryScroll>>,
    scroll_to_decision: Rc<Cell<DecisionScroll>>,
    decision_inputs: HashMap<DecisionKey, (Entity<InputState>, Subscription)>,
    composer: Entity<TextareaState>,
    composer_shown: Option<threadlane_desktop::Destination>,
    mention: components::composer::MentionPopover,
    machine_limit_saving: bool,
    machine_limit_error: Option<(String, MachineId, String)>,
    pacing_inputs: [Entity<InputState>; 4],
    /// The task screen's composer (wireframe 5).
    task_composer: Entity<TextareaState>,
    /// The Memory tab's editor.
    memory_editor: Entity<TextareaState>,
    /// The Team -> Skills editor's slug and Markdown content.
    skill_slug: Entity<InputState>,
    skill_editor: Entity<TextareaState>,
    dream_cadence: Entity<InputState>,
    /// The Add space form's name and join code.
    space_name: Entity<InputState>,
    join_code: Entity<InputState>,
    /// Where a newly paired space's feed goes.
    shown: mpsc::UnboundedSender<SpaceFeedItem>,
    /// Text to put in the memory editor at the next render.
    fill_editor: Option<String>,
    /// Text to put in the skills editor at the next render.
    fill_skill_slug: Option<String>,
    fill_skill_editor: Option<String>,
    /// Empty the Add space form at the next render.
    clear_join: bool,
    /// The new project form's name and description.
    project_name: Entity<InputState>,
    project_description: Entity<InputState>,
    /// Empty the note at the next render, which has the window it needs.
    /// Empty the new project form at the next render.
    clear_project_form: bool,
    project_name_missing: bool,
    remote_pairing: bool,
    /// The "+ New routine" form's instruction, N, time, day of the month
    /// and zone.
    routine_instruction: Entity<TextareaState>,
    routine_adding: bool,
    routine_every: Entity<InputState>,
    routine_at: Entity<InputState>,
    routine_day: Entity<InputState>,
    routine_zone: Entity<InputState>,
    /// Empty the "+ New routine" instruction at the next render.
    clear_routine: bool,
    /// The first-run screen's hub data directory.
    hub_dir: Entity<InputState>,
    /// The "+ New agent" form's name, role and instructions.
    agent_name: Entity<InputState>,
    agent_role: Entity<InputState>,
    agent_instructions: Entity<InputState>,
    /// The agent edit view's name, role, instructions, and a model per
    /// backend in `BackendKind::ALL` order.
    agent_edit_name: Entity<InputState>,
    agent_edit_role: Entity<InputState>,
    agent_edit_instructions: Entity<TextareaState>,
    agent_models: Vec<Entity<InputState>>,
    agent_edit_models: Vec<Entity<InputState>>,
    /// The "+ Add repository" form's name, remote and default branch.
    repository_hint: Option<String>,
    repository_picking: Option<Rc<()>>,
    repository_name: Entity<InputState>,
    repository_remote: Entity<InputState>,
    repository_branch: Entity<InputState>,
    /// The "+ New task" form's title and goal.
    task_title: Entity<InputState>,
    task_goal: Entity<InputState>,
    task_form_focus: FocusHandle,
    task_form_return_focus: Option<FocusHandle>,
    /// Empty these fields at the next render, which has the window it needs.
    clear_fields: Vec<Entity<InputState>>,
    /// Whether the window was full screen at the last render, which the
    /// View menu's label follows.
    full_screen: bool,
    offset_minutes: i64,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Desktop {
    fn new(
        mut state: AppState,
        core: Arc<RwLock<ClientCore>>,
        tokio: tokio::runtime::Handle,
        feed: Feed,
        start: Option<Screen>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        apply_appearance(state.appearance(), window, cx);
        cx.observe_window_appearance(window, |this, window, cx| {
            if this.state.appearance() == Appearance::System {
                apply_appearance(Appearance::System, window, cx);
            }
        })
        .detach();
        let composer = components::composer::input("Tell your team what you need…", window, cx);
        let task_composer = components::composer::input("Add to this task…", window, cx);
        let memory_editor = cx.new(|cx| TextareaState::new(window, cx).rows(16));
        let skill_slug =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. rust-pr-review"));
        let skill_editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(16)
                .placeholder("# Skill Title\n\nInstructions and checklist…")
        });
        let agent_edit_instructions = cx.new(|cx| TextareaState::new(window, cx).rows(7));
        let dream_cadence =
            cx.new(|cx| InputState::new(window, cx).placeholder("daily 02:00 or every 6h"));
        let space_name = cx.new(|cx| InputState::new(window, cx).placeholder("Name for the space"));
        let join_code = cx.new(|cx| InputState::new(window, cx).placeholder("Join code"));
        let Feed {
            shown,
            received: mut feed,
        } = feed;
        let project_name = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let project_description =
            cx.new(|cx| InputState::new(window, cx).placeholder("Description (optional)"));
        let routine_instruction = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("What should the lead do each time?")
        });
        let routine_every = cx.new(|cx| InputState::new(window, cx).default_value("60"));
        let routine_at = cx.new(|cx| InputState::new(window, cx).default_value("09:00"));
        let routine_day = cx.new(|cx| InputState::new(window, cx).default_value("1"));
        let zone = local_zone().unwrap_or_default();
        let routine_zone = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Time zone, for example Europe/Berlin")
                .default_value(zone)
        });
        let hub_dir = cx.new(|cx| InputState::new(window, cx).placeholder(HUB_DIR_HINT));
        let mut field = |placeholder: &'static str, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let pacing_inputs =
            threadlane_desktop::PACING_FIELDS.map(|setting| field(setting.placeholder, cx));
        let agent_name = field("Name", cx);
        let agent_role = field("Role, for example reviewer", cx);
        let agent_instructions = field("Instructions (optional)", cx);
        let agent_edit_name = field("Name", cx);
        let agent_edit_role = field("Role, for example reviewer", cx);
        let mut model_fields = || {
            threadlane_core::model::BackendKind::ALL
                .iter()
                .map(|_| field("Default model", cx))
                .collect()
        };
        let agent_models = model_fields();
        let agent_edit_models = model_fields();
        let repository_name = field("Name in the project", cx);
        let repository_remote = field("Clone URL or path on the hub", cx);
        let repository_branch = field("Default branch (main)", cx);
        let task_title = field("Title", cx);
        let task_goal = field("What should the agent do?", cx);
        let subscriptions = vec![
            cx.subscribe_in(&space_name, window, |this, input, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.join_error = None;
                    let suggested = this
                        .state
                        .client_dir()
                        .and_then(|root| {
                            ClientDirs::new(root)
                                .space_dir(input.read(cx).value().trim())
                                .ok()
                        })
                        .map_or_else(|| HUB_DIR_HINT.to_owned(), |dir| dir.display().to_string());
                    this.hub_dir
                        .update(cx, |field, cx| field.set_placeholder(suggested, window, cx));
                    cx.notify();
                }
            }),
            cx.subscribe_in(&join_code, window, |this, _input, event, _window, cx| {
                if matches!(event, InputEvent::Change) && this.join_error.is_some() {
                    this.join_error = None;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&hub_dir, window, |this, _input, event, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.join_error = None;
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &routine_instruction,
                window,
                |this, _input, event, _window, cx| {
                    if matches!(event, InputEvent::Change) && this.routine_error.is_some() {
                        this.routine_error = None;
                        cx.notify();
                    }
                },
            ),
            cx.observe(&project_name, |this, name, cx| {
                if this.project_name_missing && !name.read(cx).value().trim().is_empty() {
                    this.project_name_missing = false;
                    cx.notify();
                }
            }),
            components::composer::subscribe(threadlane_desktop::Draft::Lead, &composer, window, cx),
            components::composer::subscribe(
                threadlane_desktop::Draft::Task,
                &task_composer,
                window,
                cx,
            ),
        ];
        let follow = cx.spawn(async move |this, cx| {
            while let Some(item) = feed.recv().await {
                if this.update(cx, |this, cx| this.receive(item, cx)).is_err() {
                    return;
                }
            }
        });
        let refresh_loop = cx.spawn(async move |this, cx| {
            loop {
                if this
                    .update(cx, |this, cx| this.start_refresh(false, cx))
                    .is_err()
                {
                    return;
                }
                // Poll more often while the last result left gaps. A tick never overlaps a request.
                cx.background_executor().timer(RETRY_EVERY).await;
                match this.update(cx, |this, _| this.state.retry_soon()) {
                    Ok(true) => {}
                    Ok(false) => {
                        cx.background_executor()
                            .timer(REFRESH_EVERY.saturating_sub(RETRY_EVERY))
                            .await;
                    }
                    Err(_) => return,
                }
            }
        });
        let clock = cx.spawn(async move |this, cx| {
            let mut last_minute = None;
            loop {
                cx.background_executor().timer(CLOCK_EVERY).await;
                if this
                    .update(cx, |this, cx| {
                        let minute = SystemClock.now().as_millis().div_euclid(60_000);
                        let minute_changed = last_minute.replace(minute) != Some(minute);
                        // Chat history uses calendar labels. Its elapsed labels tick independently.
                        if matches!(
                            this.screen,
                            Screen::Project {
                                tab: Tab::Conversation,
                                ..
                            }
                        ) {
                            if minute_changed {
                                cx.notify();
                            }
                        } else if this.state.has_ticking_work()
                            || this.state.visible_decisions().next().is_some()
                        {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let screen = start.unwrap_or(Screen::Home);
        screens::shell::follow_screen(&mut state, &screen);
        state.set_decision_scope(screen.decision_scope());
        if screen == Screen::Home {
            state.clear_current_decision();
        }
        let conversation = if let Screen::Project {
            space,
            project,
            tab: Tab::Conversation,
        } = &screen
        {
            state.visit_conversation(space, project);
            true
        } else {
            false
        };
        let keyboard_focus = cx.focus_handle();
        keyboard_focus.focus(window, cx);
        cx.on_focus_lost(window, |this, window, cx| {
            this.keyboard_focus.focus(window, cx);
        })
        .detach();
        let sidebar_collapsed = state.sidebar_collapsed();
        let sidebar_width = state.sidebar_width().map_or(SIDEBAR_WIDTH, |w| {
            px(w).clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)
        });
        let sidebar_resizable = cx.new(|_| ResizableState::default());
        Self {
            work: WorkView::new(window, cx),
            inbox: screens::Inbox::default(),
            state: state.into(),
            core,
            tokio,
            screen,
            nav_history: navigation::NavHistory::default(),
            task_activity: TaskActivity::default(),
            task_activity_request: None,
            refresh_gate: RefreshGate::default(),
            project_load: None,
            project_load_pending: None,
            keyboard_focus,
            sidebar_collapsed,
            sidebar_width,
            sidebar_resizable,
            last_activity_revision: 0,
            join_error: None,
            routine_error: None,
            decision_scroll: ScrollHandle::default(),
            board_scroll: ScrollHandle::default(),
            dag_scroll: ScrollHandle::default(),
            topo_scroll: ScrollHandle::default(),
            reveal_linked_decision: None,
            conversation_at_bottom: true,
            conversation_follow: Rc::new(Cell::new(conversation)),
            history_scroll: Rc::default(),
            scroll_to_decision: Rc::new(Cell::new(if conversation {
                DecisionScroll::None
            } else {
                DecisionScroll::Card
            })),
            decision_inputs: HashMap::new(),
            composer,
            composer_shown: None,
            mention: Default::default(),
            machine_limit_saving: false,
            machine_limit_error: None,
            pacing_inputs,
            task_composer,
            memory_editor,
            skill_slug,
            skill_editor,
            dream_cadence,
            space_name,
            join_code,
            shown,
            fill_editor: None,
            fill_skill_slug: None,
            fill_skill_editor: None,
            clear_join: false,
            project_name,
            project_description,
            clear_project_form: false,
            project_name_missing: false,
            remote_pairing: false,
            routine_instruction,
            routine_adding: false,
            routine_every,
            routine_at,
            routine_day,
            routine_zone,
            clear_routine: false,
            hub_dir,
            agent_name,
            agent_role,
            agent_instructions,
            agent_edit_name,
            agent_edit_role,
            agent_edit_instructions,
            agent_models,
            agent_edit_models,
            repository_hint: None,
            repository_picking: None,
            repository_name,
            repository_remote,
            repository_branch,
            task_title,
            task_goal,
            task_form_focus: cx.focus_handle(),
            task_form_return_focus: None,
            clear_fields: Vec::new(),
            full_screen: false,
            offset_minutes: local_offset_minutes(),
            desktop_updatable: threadlane_updater::Component::Desktop.available(),
            updates: HashMap::new(),
            _tasks: vec![follow, refresh_loop, clock],
            _subscriptions: subscriptions,
        }
    }

    /// Now, in local time.
    fn when(&self) -> When {
        When {
            now: SystemClock.now(),
            offset_minutes: self.offset_minutes,
        }
    }

    /// Runs `job` on the Tokio threads and hands its result to `then`.
    fn run<T: Send + 'static>(
        &self,
        job: impl Future<Output = T> + Send + 'static,
        then: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let job = self.tokio.spawn(job);
        cx.spawn(async move |this, cx| {
            if let Ok(result) = job.await {
                let _gone = this.update(cx, |this, cx| {
                    then(this, result, cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.start_refresh(true, cx);
    }

    /// Brings each hub this app started onto the app's build, and says
    /// which hubs run another build it leaves alone.
    fn match_hub_builds(&mut self, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let root = self.state.client_dir().map(std::path::Path::to_path_buf);
        self.run(
            async move {
                let app = local_hub::app_build();
                let statuses = core.read().await.status().await;
                let mut said = Vec::new();
                for status in statuses {
                    let Ok(info) = status.info else { continue };
                    let socket = match status.reach {
                        threadlane_client::Reach::Socket { socket } => Some(socket),
                        threadlane_client::Reach::Iroh { .. } => None,
                    };
                    let ours = match (&root, &socket) {
                        (Some(root), Some(socket)) => local_hub::ours(root, socket, info.pid),
                        _ => None,
                    };
                    let name = status.name;
                    let was = info.build.as_ref().map_or_else(
                        || "an older build".to_owned(),
                        |build| format!("build {build}"),
                    );
                    match local_hub::plan(&name, &app, info.build.as_ref(), ours.is_some()) {
                        local_hub::Plan::Keep => {}
                        local_hub::Plan::Notice(notice) => said.push((false, notice)),
                        local_hub::Plan::Restart => {
                            let (Some(root), Some(socket), Some(pid)) =
                                (root.clone(), socket, ours)
                            else {
                                continue;
                            };
                            let space = name.clone();
                            let restarted = tokio::task::spawn_blocking(move || {
                                local_hub::restart(&root, &socket, &space, pid)
                            })
                            .await
                            .map_err(|error| error.to_string())
                            .flatten();
                            said.push(match restarted {
                                Ok(()) => (
                                    true,
                                    format!(
                                        "Restarted the hub of {name} on build {app}; it was on {was}."
                                    ),
                                ),
                                Err(why) => (
                                    false,
                                    format!(
                                        "The hub of {name} is on {was}; this app is on build {app}. Restarting it failed: {why}."
                                    ),
                                ),
                            });
                        }
                    }
                }
                said
            },
            |this, said, cx| {
                if said.is_empty() {
                    return;
                }
                let ok = said.iter().all(|(ok, _)| *ok);
                let text = said
                    .into_iter()
                    .map(|(_, text)| text)
                    .collect::<Vec<_>>()
                    .join(" ");
                this.state.report(if ok {
                    Activity::Done(text)
                } else {
                    Activity::Failed(text)
                });
                this.refresh(cx);
            },
            cx,
        );
    }

    fn start_refresh(&mut self, explicit: bool, cx: &mut Context<Self>) {
        let Some(pairing_revision) = self.refresh_gate.begin(explicit) else {
            return;
        };
        self.state.checking_connections();
        cx.notify();
        let core = Arc::clone(&self.core);
        let (send, mut receive) = mpsc::unbounded_channel();
        let job = self.tokio.spawn(async move {
            let core = core.read().await;
            refresh_each(&core, |_, part| send.send(part).is_ok()).await;
            core.hubs()
                .iter()
                .map(|hub| hub.name.clone())
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            while let Some(part) = receive.recv().await {
                if this
                    .update(cx, |this, cx| {
                        if this.refresh_gate.is_current(pairing_revision) {
                            this.apply_refresh(part, cx);
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    job.abort();
                    return;
                }
            }
            let roster = job.await;
            let _ = this.update(cx, |this, cx| {
                let (current, again) = this.refresh_gate.finish(pairing_revision);
                this.state.finish_checking_connections();
                if current && let Ok(roster) = roster {
                    this.state.retain_paired_spaces(&roster);
                    // These unscoped screens fetch auxiliary data across all hubs.
                    // Load once, rather than re-querying every hub on each result.
                    if this.screen.loads_after_refresh_pass() {
                        this.load_screen(cx);
                    }
                }
                if again {
                    this.start_refresh(true, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_refresh(&mut self, refreshed: threadlane_desktop::Refresh, cx: &mut Context<Self>) {
        let selected_hub = refreshed
            .refreshed_space()
            .is_some_and(|space| self.screen.space() == Some(space));
        let open_project = match &self.screen {
            Screen::Project { space, project, .. } => Some((space.clone(), project.clone())),
            Screen::Task { space, task } => self
                .state
                .task(space, *task)
                .map(|detail| (space.clone(), detail.summary.project.clone())),
            _ => None,
        };
        let parts = match &self.screen {
            Screen::Project { space, project, .. } => refreshed.project_parts(space, project),
            _ => (false, false),
        };
        self.state.apply_partial(refreshed);
        // Refreshes run every REFRESH_EVERY, so this re-checks well within a minute.
        for alert in self.state.reminders(SystemClock.now()) {
            if self.state.notifies(&alert) {
                self.remind(alert, cx);
            }
        }
        if !selected_hub {
            return;
        }
        if let Some((space, project)) = open_project
            && self.state.project_absent(&space, &project)
        {
            self.state.report(Activity::Failed(format!(
                "No project {project:?} in {space}."
            )));
            self.go(Screen::Home, cx);
        } else {
            self.load_screen_with_parts(parts, cx);
        }
    }

    /// Loads what the open screen shows beyond what a refresh brings.
    fn load_screen(&mut self, cx: &mut Context<Self>) {
        self.load_screen_with_parts((false, false), cx);
    }

    fn load_screen_with_parts(&mut self, parts: (bool, bool), cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        match self.screen.clone() {
            Screen::Work { space, page } => self.load_work(space, page, cx),
            Screen::Routines(scope) => {
                let projects = self
                    .state
                    .targets()
                    .into_iter()
                    .filter(|(space, _)| scope.as_deref().is_none_or(|scope| *space == scope))
                    .map(|(space, project)| (space.to_owned(), project.to_owned()))
                    .collect();
                self.load_routines(projects, cx);
            }
            Screen::Project {
                space,
                project,
                tab,
            } => {
                if self.project_load.is_some() {
                    self.project_load_pending
                        .get_or_insert_with(PendingProjectLoad::default)
                        .queue(parts);
                    return;
                }
                if matches!(tab, Tab::Conversation | Tab::Tasks) {
                    self.load_space_skills(space.clone(), cx);
                }
                let (fresh_tasks, fresh_lead) = parts;
                if tab == Tab::Routines {
                    self.load_routines(vec![(space.clone(), project.clone())], cx);
                }
                if tab == Tab::Memory {
                    let (in_space, named) = (space.clone(), project.clone());
                    let for_dreaming = Arc::clone(&core);
                    let revision = self.state.dreaming_revision(&space, &project);
                    self.run(
                        async move {
                            load_dreaming(&*for_dreaming.read().await, &in_space, &named, revision)
                                .await
                        },
                        |this, loaded, _| this.state.apply_dreaming(loaded),
                        cx,
                    );
                }
                if matches!(tab, Tab::Conversation | Tab::Memory) {
                    let selected = (tab == Tab::Memory)
                        .then(|| {
                            self.state
                                .memory_selected(&space, &project)
                                .map(str::to_owned)
                        })
                        .flatten();
                    let (in_space, named) = (space.clone(), project.clone());
                    let for_memory = Arc::clone(&core);
                    self.run(
                        async move {
                            load_memory(&*for_memory.read().await, &in_space, &named, selected)
                                .await
                        },
                        |this, loaded, _| this.state.apply_memory(loaded),
                        cx,
                    );
                }
                let pulls_for = (tab == Tab::Tasks).then(|| (space.clone(), project.clone()));
                let job = self.tokio.spawn(async move {
                    load_project_tab(
                        &*core.read().await,
                        &space,
                        &project,
                        Some(tab),
                        fresh_tasks,
                        fresh_lead,
                    )
                    .await
                });
                self.project_load = Some(cx.spawn(async move |this, cx| {
                    let loaded = job.await;
                    let _ = this.update(cx, |this, cx| {
                        this.project_load = None;
                        let pending = this.project_load_pending.take();
                        if let Ok(mut loaded) = loaded {
                            if let Some(pending) = &pending {
                                loaded.keep_refreshed_parts(pending.refreshed);
                            }
                            this.state.apply_project(loaded);
                            if let Some((space, project)) = pulls_for {
                                this.load_task_pulls(space, &project, cx);
                            }
                            cx.notify();
                        }
                        if let Some(pending) = pending {
                            this.load_screen_with_parts(pending.latest, cx);
                        }
                    });
                }));
            }
            Screen::Task { space, task } => {
                self.load_space_skills(space.clone(), cx);
                // The feed runs live and summarizes a finished task, so its newest page always loads.
                self.load_task_activity(space.clone(), task, None, cx);
                let for_repos = Arc::clone(&core);
                self.run(
                    async move { load_task_overview(&*core.read().await, &space, task).await },
                    move |this, loaded, cx| {
                        this.state.apply_task(loaded);
                        if let Screen::Task { space, task } = &this.screen
                            && this.state.review_files_stale(space, *task)
                        {
                            this.load_review_files(space.clone(), *task, cx);
                        }
                        if let Screen::Task { space, task } = &this.screen
                            && let Some(project) = this
                                .state
                                .task(space, *task)
                                .map(|detail| detail.summary.project.clone())
                            && this.state.project_gap(
                                space,
                                &project,
                                threadlane_desktop::Part::Repositories,
                            ) == Some(threadlane_desktop::Gap::Loading)
                        {
                            let space = space.clone();
                            let core = Arc::clone(&for_repos);
                            this.run(
                                async move {
                                    let repositories = core
                                        .read()
                                        .await
                                        .repositories(Some(&space), Some(&project))
                                        .await;
                                    (space, project, repositories)
                                },
                                |this, (space, project, repositories), _| {
                                    this.state
                                        .apply_repositories(&space, &project, repositories);
                                },
                                cx,
                            );
                        }
                    },
                    cx,
                );
            }
            Screen::Agents(scope) => {
                let skill_spaces: Vec<String> = self
                    .state
                    .spaces()
                    .iter()
                    .filter(|space| scope.as_deref().is_none_or(|s| s == space.name))
                    .map(|space| space.name.clone())
                    .collect();
                for space in skill_spaces {
                    self.load_space_skills(space, cx);
                }
                let projects = self
                    .state
                    .targets()
                    .into_iter()
                    .filter(|(space, _)| scope.as_deref().is_none_or(|scope| *space == scope))
                    .map(|(space, project)| (space.to_owned(), project.to_owned()))
                    .collect();
                self.run(
                    async move { load_modes(&*core.read().await, projects).await },
                    |this, loaded, _| this.state.apply_modes(loaded),
                    cx,
                );
            }
            Screen::Settings(Some(scope)) => {
                let projects = self
                    .state
                    .targets()
                    .into_iter()
                    .filter(|(space, _)| *space == scope)
                    .map(|(space, project)| (space.to_owned(), project.to_owned()))
                    .collect();
                let for_modes = Arc::clone(&core);
                self.run(
                    async move { load_modes(&*for_modes.read().await, projects).await },
                    |this, modes, _| this.state.apply_modes(modes),
                    cx,
                );
                self.run(
                    async move {
                        let core = core.read().await;
                        let (pacing, metrics) =
                            tokio::join!(core.pacing(Some(&scope)), load_metrics(&core, &scope));
                        (scope, pacing, metrics)
                    },
                    |this, (space, pacing, metrics), _| {
                        this.state.apply_pacing(space.clone(), pacing);
                        this.state.apply_metrics(space, metrics);
                    },
                    cx,
                );
            }
            Screen::Machine { space, .. } => {
                let when = self.when();
                self.run(
                    async move {
                        threadlane_desktop::load_machine_history(&*core.read().await, &space, &when)
                            .await
                    },
                    |this, loaded, _| this.state.apply_machine_history(loaded),
                    cx,
                );
            }
            Screen::Home => {
                let skill_spaces: Vec<String> = self
                    .state
                    .spaces()
                    .iter()
                    .map(|space| space.name.clone())
                    .collect();
                for space in skill_spaces {
                    self.load_space_skills(space, cx);
                }
                let projects = self.state.timeline_projects();
                self.run(
                    async move { load_timeline(&*core.read().await, projects).await },
                    |this, loaded, _| this.state.apply_timeline(loaded),
                    cx,
                );
            }
            Screen::Machines
            | Screen::NewProject { .. }
            | Screen::SpaceTasks { .. }
            | Screen::Settings(None)
            | Screen::AddSpace => {}
        }
    }

    /// Asks for the routines of each `(space, project)`.
    fn load_routines(&mut self, projects: Vec<(String, String)>, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.run(
            async move { load_routines(&*core.read().await, projects).await },
            |this, loaded, _| this.state.apply_routines(loaded),
            cx,
        );
    }

    /// Reads the pull request strip of a project's board and list cards.
    fn load_task_pulls(&mut self, space: String, project: &str, cx: &mut Context<Self>) {
        let tasks = self.state.pull_candidates(&space, project);
        if tasks.is_empty() {
            return;
        }
        let core = Arc::clone(&self.core);
        self.run(
            async move { threadlane_desktop::load_pulls(&*core.read().await, &space, tasks).await },
            |this, loaded, _| this.state.apply_pulls(loaded),
            cx,
        );
    }

    fn load_task_activity(
        &mut self,
        space: String,
        task: TaskId,
        before: Option<ActivityCursor>,
        cx: &mut Context<Self>,
    ) {
        if self.task_activity.loading {
            return;
        }
        self.task_activity.loading = true;
        self.task_activity.error = None;
        let core = Arc::clone(&self.core);
        let job = self
            .tokio
            .spawn(async move { core.read().await.task_activity(&space, task, before).await });
        self.task_activity_request = Some(cx.spawn(async move |this, cx| {
            let Ok(result) = job.await else { return };
            let _ = this.update(cx, |this, cx| {
                let activity = &mut this.task_activity;
                activity.loading = false;
                match result {
                    Ok((entries, next)) => activity.apply_page(before, entries, next),
                    Err(error) => activity.error = Some((before, describe(&error))),
                }
                cx.notify();
            });
        }));
    }

    fn receive(&mut self, item: SpaceFeedItem, cx: &mut Context<Self>) {
        // An item from a hub that was away or refused a part: it is back.
        let back = self
            .state
            .space(&item.space)
            .is_some_and(threadlane_desktop::Space::stale);
        let for_open_project = matches!(
            &self.screen,
            Screen::Project { space, project, .. }
                if threadlane_desktop::belongs_in_conversation(&item, space, project)
        );
        let conversation_arrival = match &self.screen {
            Screen::Project {
                space,
                project,
                tab: Tab::Conversation,
            } if for_open_project => Some((
                space.clone(),
                project.clone(),
                self.state.conversation_latest(space, project),
            )),
            _ => None,
        };
        // The card carries no creation time or context: ask its hub, also
        // for a decision that waits under Later without an alert.
        let decision = self.state.is_new_decision(&item);
        if let Some(alert) = self.state.receive(item)
            && self.state.notifies(&alert)
        {
            self.notify(&alert, cx);
        }
        if decision && !back {
            self.refresh(cx);
        }
        if back {
            self.refresh(cx);
        }
        if let Some((space, project, previous)) = conversation_arrival
            && self.state.conversation_latest(&space, &project) > previous
        {
            self.state.conversation_arrived(&space, &project, previous);
        }
        // What the lead says often comes with tasks it made or finished.
        if for_open_project {
            self.load_screen(cx);
        }
        cx.notify();
    }

    /// Raises the system notification for `alert`.
    /// Raises a reminder unless the hub says it already went out, before a
    /// restart of this app or the hub.
    fn remind(&self, alert: threadlane_desktop::Alert, cx: &mut Context<Self>) {
        let claim = match &alert {
            threadlane_desktop::Alert::Decision { space, card, .. } => {
                DecisionId::parse(&card.id).map(|id| (space.clone(), id))
            }
            threadlane_desktop::Alert::Notice { .. } => None,
        };
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let first = match claim {
                    // A hub from before reminders were kept, or out of
                    // reach, still gets this run's one reminder.
                    Some((space, id)) => core
                        .read()
                        .await
                        .claim_reminder(Some(&space), id)
                        .await
                        .unwrap_or(true),
                    None => true,
                };
                (alert, first)
            },
            |this, (alert, first), cx| {
                if first {
                    this.notify(&alert, cx);
                }
            },
            cx,
        );
    }

    fn notify(&self, alert: &threadlane_desktop::Alert, cx: &mut Context<Self>) {
        cx.show_system_notification(SystemNotification {
            tag: alert.tag().into(),
            title: alert.title().into(),
            body: alert.body().into(),
            actions: alert
                .options()
                .iter()
                .map(|label| SystemNotificationAction {
                    id: label.clone().into(),
                    label: label.clone().into(),
                })
                .collect(),
        });
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.state.set_sidebar_collapsed(self.sidebar_collapsed);
        cx.notify();
    }

    fn go(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.nav_history.record(self.screen.clone(), &screen);
        self.navigate_to(screen, cx);
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.nav_history.go_back(self.screen.clone()) {
            self.navigate_to(target, cx);
        }
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.nav_history.go_forward(self.screen.clone()) {
            self.navigate_to(target, cx);
        }
    }

    fn navigate_to(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.join_error = None;
        self.routine_error = None;
        if matches!(
            self.screen,
            Screen::Project {
                tab: Tab::Memory,
                ..
            }
        ) && self.screen != screen
        {
            self.clear_fields.push(self.dream_cadence.clone());
        }
        if self.screen != screen {
            self.task_form_return_focus = None;
            self.state.clear_memory_inspection();
            self.state.close_review();
            self.work.leave();
            self.inbox = screens::Inbox::default();
            self.project_load = None;
            self.project_load_pending = None;
            self.task_activity_request = None;
            self.task_activity = TaskActivity::default();
            self.state.reset_chat_target();
            self.project_name_missing = false;
            self.reveal_linked_decision = None;
            self.conversation_at_bottom = true;
            self.conversation_follow = Rc::new(Cell::new(false));
            self.history_scroll = Rc::default();
            if let Screen::Project {
                space,
                project,
                tab: Tab::Conversation,
            } = &screen
            {
                self.state.visit_conversation(space, project);
                self.conversation_follow.set(true);
            }
            self.state.set_decision_scope(screen.decision_scope());
            if screen == Screen::Home {
                self.state.clear_current_decision();
            }
            self.scroll_to_decision = Rc::new(Cell::new(if self.conversation_follow.get() {
                DecisionScroll::None
            } else {
                DecisionScroll::Card
            }));
            self.decision_scroll = ScrollHandle::default();
            self.board_scroll = ScrollHandle::default();
            self.dag_scroll = ScrollHandle::default();
            self.topo_scroll = ScrollHandle::default();
        }
        screens::shell::follow_screen(&mut self.state, &screen);
        self.screen = screen;
        self.load_screen(cx);
        cx.notify();
    }

    fn answer(
        &mut self,
        space: String,
        id: String,
        option: String,
        note: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(decision_id) = DecisionId::parse(&id) else {
            return;
        };
        let key = DecisionKey {
            space: space.clone(),
            id: decision_id,
        };
        if !self.state.begin_decision_answer(key.clone()) {
            return;
        }
        let always = self.state.answers_always(&space, &id, &option);
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Answering {space} {id}…")));
        self.run(
            async move {
                let core = core.read().await;
                if always {
                    core.answer_always(Some(&space), &id, &option, note.as_deref())
                        .await
                } else {
                    core.answer(Some(&space), &id, &option, note.as_deref())
                        .await
                }
            },
            move |this, answered, cx| {
                this.state.finish_decision_answer(&key);
                match answered {
                    Ok(answer) => {
                        if this
                            .state
                            .current_decision()
                            .is_some_and(|current| DecisionKey::from(current) == key)
                        {
                            this.scroll_to_decision.set(DecisionScroll::Card);
                        }
                        this.state.answered(&answer);
                        cx.dismiss_system_notification(&format!(
                            "{}/{}",
                            answer.space, answer.decision.id
                        ));
                        let chosen = answer
                            .decision
                            .chosen()
                            .map_or_else(String::new, |option| format!(": {}", option.label));
                        this.state.report(Activity::Done(format!(
                            "Answered {} / {}{chosen}.",
                            answer.space, answer.decision.id
                        )));
                        this.load_screen(cx);
                    }
                    Err(error) => this.state.report(Activity::Failed(describe(&error))),
                }
            },
            cx,
        );
        cx.notify();
    }

    fn set_mode(
        &mut self,
        space: String,
        project: String,
        agent: String,
        mode: Option<Mode>,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let set = core
                    .read()
                    .await
                    .set_mode(Some(&space), Some(&project), &agent, mode)
                    .await;
                (space, project, set)
            },
            |this, (space, project, set), _| match set {
                Ok(binding) => {
                    this.state.report(Activity::Done(match binding.mode {
                        Some(mode) => format!(
                            "{} is now {} in {space} / {project}.",
                            binding.agent,
                            mode_label(mode)
                        ),
                        None => format!(
                            "{} now follows the mode of {space} / {project}.",
                            binding.agent
                        ),
                    }));
                    this.state.mode_set(&space, &project, binding);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
    }

    /// Creates the project the new project form describes, then opens it.
    fn create_project(&mut self, space: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.project_name.read(cx).value().trim().to_owned();
        let description = self.project_description.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.project_name_missing = true;
            self.project_name.focus_handle(cx).focus(window, cx);
            cx.notify();
            return;
        }
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Creating {name} in {space}…")));
        self.run(
            async move {
                let created = core
                    .read()
                    .await
                    .create_project(Some(&space), &name, &description)
                    .await;
                (space, created)
            },
            |this, (space, created), cx| match created {
                Ok(project) => {
                    let screen = Screen::Project {
                        space: space.clone(),
                        project: project.name.clone(),
                        tab: Tab::Conversation,
                    };
                    this.state.project_created(&space, project);
                    this.clear_project_form = true;
                    this.go(screen, cx);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    fn earlier(
        &mut self,
        space: String,
        project: String,
        before: Timestamp,
        cx: &mut Context<Self>,
    ) {
        if !self.state.begin_earlier(&space, &project, before) {
            return;
        }
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let loaded = load_earlier(&*core.read().await, &space, &project, before).await;
                (space, project, loaded)
            },
            |this, (space, project, loaded), _| {
                if this.state.apply_earlier(loaded)
                    && !this.conversation_follow.get()
                    && this.reveal_linked_decision.is_none()
                    && matches!(
                        &this.screen,
                        Screen::Project { space: shown_space, project: shown_project, tab: Tab::Conversation }
                            if shown_space == &space && shown_project == &project
                    )
                {
                    this.history_scroll.borrow_mut().preserve();
                }
            },
            cx,
        );
        cx.notify();
    }

    fn issue_code(&mut self, space: String, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.run(
            async move { core.read().await.pair_code(Some(&space)).await },
            |this, issued, _| match issued {
                Ok(code) => this.state.issued(code),
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
    }
}

impl Desktop {
    /// Shows a memory file on the right, with its versions.
    fn select_memory(&mut self, space: &str, project: &str, path: &str, cx: &mut Context<Self>) {
        self.state.select_memory(space, project, path);
        self.load_screen(cx);
        cx.notify();
    }

    /// Opens the selected memory file for editing, or closes it.
    fn edit_memory(&mut self, space: &str, project: &str, open: bool, cx: &mut Context<Self>) {
        self.fill_editor = self.state.edit_memory(space, project, open);
        cx.notify();
    }

    /// Saves the editor's text as the selected file, or deletes the file
    /// with `save` false, as you.
    fn change_memory(
        &mut self,
        space: String,
        project: String,
        save: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self
            .state
            .memory_selected(&space, &project)
            .map(str::to_owned)
        else {
            return;
        };
        let content = save.then(|| self.memory_editor.read(cx).value().to_string());
        self.write_memory(space, project, path, content, cx);
    }

    fn write_memory(
        &mut self,
        space: String,
        project: String,
        path: String,
        content: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        let verb = if content.is_some() {
            "Saving"
        } else {
            "Deleting"
        };
        self.state.report(Activity::Working(format!(
            "{verb} {path} in {space} / {project}…"
        )));
        self.run(
            async move {
                change_memory(&*core.read().await, &space, &project, &path, content).await
            },
            |this, changed, cx| {
                let report = match this.state.apply_change(changed) {
                    Ok(done) => Activity::Done(done),
                    Err(why) => Activity::Failed(why),
                };
                this.state.report(report);
                this.load_screen(cx);
            },
            cx,
        );
        cx.notify();
    }

    fn toggle_dreaming(
        &mut self,
        space: String,
        project: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.set_dreaming(space, project, DreamingAction::Toggle(enabled), cx);
    }

    fn save_dream_cadence(&mut self, space: String, project: String, cx: &mut Context<Self>) {
        let text = self.dream_cadence.read(cx).value();
        self.set_dreaming(space, project, DreamingAction::Cadence(&text), cx);
    }

    fn set_dreaming(
        &mut self,
        space: String,
        project: String,
        action: DreamingAction<'_>,
        cx: &mut Context<Self>,
    ) {
        let zone = local_zone().unwrap_or_else(|| "UTC".to_owned());
        let request = match self.state.dreaming_request(&space, &project, action, &zone) {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(error) => {
                self.state.report(Activity::Failed(error));
                cx.notify();
                return;
            }
        };
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let result = change_dreaming(&*core.read().await, &space, request).await;
                (space, project, result)
            },
            |this, (space, project, result), cx| {
                match &result {
                    Ok(_) => {
                        this.clear_fields.push(this.dream_cadence.clone());
                        this.state
                            .report(Activity::Done("Dreaming settings saved.".to_owned()));
                    }
                    Err(error) => this.state.report(Activity::Failed(error.clone())),
                }
                this.state.dreaming_saved(&space, &project, result);
                this.load_screen(cx);
            },
            cx,
        );
        cx.notify();
    }

    /// Pairs this app with the hub of the join code under the name given,
    /// as `threadlane hub join <name> <code>` does; its feed follows.
    fn join_space(&mut self, cx: &mut Context<Self>) {
        let name = self.space_name.read(cx).value().trim().to_owned();
        let code = self.join_code.read(cx).value().trim().to_owned();
        if name.is_empty() || code.is_empty() {
            self.join_error = Some("Give the space a name and paste its join code.".to_owned());
            cx.notify();
            return;
        }
        self.join_error = None;
        let (core, shown) = (Arc::clone(&self.core), self.shown.clone());
        self.state
            .report(Activity::Working(format!("Pairing {name}…")));
        self.run(
            async move {
                let joined = {
                    let mut client = core.write().await;
                    client
                        .join(&name, &code)
                        .await
                        .and_then(|()| client.follow(Some(&name)))
                };
                let joined = joined.map(|stream| {
                    tokio::spawn(pump(Arc::clone(&core), stream, shown));
                });
                (name, joined)
            },
            |this, (name, joined), cx| match joined {
                Ok(()) => this.space_paired(&name, format!("Paired {name}."), cx),
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Pairs this app with the hub whose data directory the first-run
    /// screen names, as `threadlane hub add <name> <dir>` does; its feed
    /// follows.
    fn pair_directory(&mut self, cx: &mut Context<Self>) {
        let name = self.space_name.read(cx).value().trim().to_owned();
        let dir = self.hub_dir.read(cx).value().to_string();
        if name.is_empty() {
            self.join_error = Some("Give the space a name.".to_owned());
            cx.notify();
            return;
        }
        self.join_error = None;
        let dir = screens::machines::parse_hub_dir(&dir, std::env::var_os("HOME").as_deref());
        let (core, shown) = (Arc::clone(&self.core), self.shown.clone());
        let client_dir = self.state.client_dir().map(std::path::Path::to_path_buf);
        self.state
            .report(Activity::Working(format!("Pairing {name}…")));
        self.run(
            async move {
                let paired = {
                    let mut client = core.write().await;
                    client.pair_directory(&name, dir).and_then(|()| {
                        let socket = client
                            .hubs()
                            .iter()
                            .find_map(|hub| {
                                (hub.name == name).then(|| match &hub.reach {
                                    threadlane_client::Reach::Socket { socket } => {
                                        Some(socket.clone())
                                    }
                                    threadlane_client::Reach::Iroh { .. } => None,
                                })
                            })
                            .flatten();
                        client.follow(Some(&name)).map(|stream| (stream, socket))
                    })
                };
                let paired = match paired {
                    Ok((stream, socket)) => {
                        tokio::spawn(pump(Arc::clone(&core), stream, shown));
                        let (data_dir, running) = match socket {
                            Some(socket) => {
                                let running =
                                    tokio::time::timeout(Duration::from_millis(500), async {
                                        tokio::net::UnixStream::connect(&socket).await.is_ok()
                                    })
                                    .await
                                    .unwrap_or(false);
                                let data_dir = socket.parent().map(std::path::Path::to_path_buf);
                                // A space made here gets its hub started here.
                                let running = match (&client_dir, &data_dir) {
                                    (Some(root), Some(dir))
                                        if !running && local_hub::owned(root, dir) =>
                                    {
                                        local_hub::start(root, dir, &name).is_ok()
                                            && listening(&socket).await
                                    }
                                    _ => running,
                                };
                                (data_dir, running)
                            }
                            None => (None, true),
                        };
                        Ok(screens::machines::paired_directory_message(
                            &name,
                            data_dir.as_deref(),
                            running,
                        ))
                    }
                    Err(error) => Err(error),
                };
                (name, paired)
            },
            |this, (name, paired), cx| match paired {
                Ok(message) => this.space_paired(&name, message, cx),
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    fn space_paired(&mut self, name: &str, message: String, cx: &mut Context<Self>) {
        self.refresh_gate.pairing_changed();
        self.state.open_paired_space(name);
        self.state.select_space(name);
        self.state.report(Activity::Done(message));
        self.clear_join = true;
        self.go(Screen::Home, cx);
        self.refresh(cx);
    }

    /// Puts `text` on the clipboard; the status area says so.
    fn copy(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.state.copied(&text);
        cx.notify();
    }

    /// Creates the routine the "+ New routine" form describes in `project`
    /// of `space`, as yours.
    fn add_routine(&mut self, space: String, project: String, cx: &mut Context<Self>) {
        if self.routine_adding || !self.state.routine_form().is_open_for(&space, &project) {
            return;
        }
        let instruction = self.routine_instruction.read(cx).value().trim().to_owned();
        let (every, at, day, zone) = (
            self.routine_every.read(cx).value(),
            self.routine_at.read(cx).value(),
            self.routine_day.read(cx).value(),
            self.routine_zone.read(cx).value(),
        );
        let grace = self.state.routine_form().missed_grace_minutes;
        let recurrence = self.state.routine_form().recurrence(RoutineText {
            every: &every,
            at: &at,
            day: &day,
            zone: &zone,
        });
        let recurrence = match (instruction.is_empty(), recurrence) {
            (true, _) => Err("Say what the routine should do.".to_owned()),
            (false, recurrence) => recurrence,
        };
        let recurrence = match recurrence {
            Ok(recurrence) => {
                self.routine_error = None;
                recurrence
            }
            Err(why) => {
                self.routine_error = Some(why);
                cx.notify();
                return;
            }
        };
        let core = Arc::clone(&self.core);
        self.routine_adding = true;
        self.state.report(Activity::Working(format!(
            "Adding a routine to {space} / {project}…"
        )));
        self.run(
            async move {
                let added = core
                    .read()
                    .await
                    .add_routine_with_grace(
                        Some(&space),
                        Some(&project),
                        &instruction,
                        recurrence,
                        grace,
                    )
                    .await;
                (space, project, added)
            },
            |this, (space, project, added), _| {
                this.routine_adding = false;
                match added {
                    Ok(timer) => {
                        this.state.report(Activity::Done(format!(
                            "Added routine {} to {space} / {project}.",
                            timer.id
                        )));
                        this.state.timer_changed(&space, &project, timer);
                        if this.state.routine_form().is_open_for(&space, &project) {
                            this.state.routine_form_mut().open_for = None;
                            this.clear_routine = true;
                        }
                    }
                    Err(error) => this.state.report(Activity::Failed(describe(&error))),
                }
            },
            cx,
        );
        cx.notify();
    }

    /// Pauses, resumes or cancels timer `id` of `project` in `space`.
    fn change_timer(
        &mut self,
        space: String,
        project: String,
        id: threadlane_core::ids::TimerId,
        change: TimerChange,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        let (doing, done) = match change {
            TimerChange::Pause => ("Pausing", "Paused"),
            TimerChange::Resume => ("Resuming", "Resumed"),
            TimerChange::Cancel => ("Cancelling", "Cancelled"),
        };
        self.state.report(Activity::Working(format!(
            "{doing} routine {id} in {space} / {project}…"
        )));
        self.run(
            async move {
                let core = core.read().await;
                let (in_space, number) = (Some(space.as_str()), id.to_string());
                let changed = match change {
                    TimerChange::Pause => core.pause_timer(in_space, &number).await,
                    TimerChange::Resume => core.resume_timer(in_space, &number).await,
                    TimerChange::Cancel => core.cancel_timer(in_space, &number).await,
                };
                (space, project, changed)
            },
            move |this, (space, project, changed), _| match changed {
                Ok(timer) => {
                    this.state.report(Activity::Done(format!(
                        "{done} routine {} in {space} / {project}.",
                        timer.id
                    )));
                    this.state.timer_changed(&space, &project, timer);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Unpairs this app from a space once you confirmed, as `threadlane hub
    /// remove` does: the hub keeps its data and this app forgets its key.
    fn unpair_space(&mut self, name: String, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let unpaired = core.write().await.unpair(&name);
                (name, unpaired)
            },
            |this, (name, unpaired), cx| match unpaired {
                Ok(()) => {
                    this.refresh_gate.pairing_changed();
                    this.nav_history.prune_space(&name);
                    this.state.space_removed(&name);
                    this.state
                        .report(Activity::Done(format!("Unpaired {name}.")));
                    let gone = match &this.screen {
                        Screen::Project { space, .. }
                        | Screen::Task { space, .. }
                        | Screen::Work { space, .. }
                        | Screen::SpaceTasks { space }
                        | Screen::Agents(Some(space))
                        | Screen::Routines(Some(space))
                        | Screen::Settings(Some(space))
                        | Screen::NewProject { space } => *space == name,
                        _ => false,
                    };
                    if gone {
                        this.go(Screen::Machines, cx);
                    }
                    this.refresh(cx);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
    }
    fn sync_activity_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rev = self.state.activity_revision();
        if rev == self.last_activity_revision {
            return;
        }
        self.last_activity_revision = rev;
        let Some(activity) = self.state.activity().cloned() else {
            return;
        };
        match activity {
            Activity::Working(_) => {}
            Activity::Done(text) => {
                window.push_notification(Notification::success(text).id::<ActivityToast>(), cx);
            }
            Activity::Failed(why) => {
                window.push_notification(
                    Notification::error(why)
                        .title("Action failed")
                        .id::<ActivityToast>()
                        .autohide(false),
                    cx,
                );
            }
        }
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_activity_notifications(window, cx);
        let viewport = window.viewport_size();
        if viewport.width >= WINDOW_MIN_WIDTH && viewport.height >= WINDOW_MIN_HEIGHT {
            self.state
                .set_window_size(f32::from(viewport.width), f32::from(viewport.height));
        }
        if let Some(text) = self.fill_editor.take() {
            self.memory_editor
                .update(cx, |editor, cx| editor.set_value(text, window, cx));
        }
        if let Some(slug) = self.fill_skill_slug.take() {
            self.skill_slug
                .update(cx, |field, cx| field.set_value(slug, window, cx));
        }
        if let Some(text) = self.fill_skill_editor.take() {
            self.skill_editor
                .update(cx, |editor, cx| editor.set_value(text, window, cx));
        }
        if std::mem::take(&mut self.clear_join) {
            self.hub_dir.update(cx, |field, cx| {
                field.set_placeholder(HUB_DIR_HINT, window, cx)
            });
            for field in [&self.space_name, &self.join_code, &self.hub_dir] {
                field.update(cx, |field, cx| field.set_value("", window, cx));
            }
        }
        for field in std::mem::take(&mut self.clear_fields) {
            field.update(cx, |field, cx| field.set_value("", window, cx));
        }
        if std::mem::take(&mut self.clear_routine) {
            self.routine_instruction
                .update(cx, |field, cx| field.set_value("", window, cx));
        }
        if window.is_fullscreen() != self.full_screen {
            self.full_screen = window.is_fullscreen();
            menus::set(self.full_screen, self.state.appearance(), cx);
        }
        if std::mem::take(&mut self.clear_project_form) {
            for field in [&self.project_name, &self.project_description] {
                field.update(cx, |field, cx| field.set_value("", window, cx));
            }
        }
        self.sync_drafts(window, cx);
        self.prepare_decision_inputs(window, cx);
        let when = self.when();
        let body = match self.screen.clone() {
            Screen::Work { space, page } => self.work_screen(&space, &page, &when, window, cx),
            Screen::Home => self.home(&when, window, cx),
            Screen::Project {
                space,
                project,
                tab,
            } => self.project(&space, &project, tab, &when, window, cx),
            Screen::Task { space, task } => self.task(&space, task, &when, cx),
            Screen::Agents(scope) => self.agents(scope.as_deref(), &when, cx),
            Screen::SpaceTasks { space } => self.space_tasks(&space, &when, cx),
            Screen::NewProject { space } => self.new_project(&space, cx),
            Screen::Machines => self.machines(&when, cx),
            Screen::Machine { space, machine } => self.machine(&space, machine, &when, cx),
            Screen::Routines(scope) => self.routines_screen(scope.as_deref(), &when, cx),
            Screen::Settings(scope) => self.settings(scope.as_deref(), &when, cx),
            Screen::AddSpace => self.add_space(cx),
        };
        // With no hub paired, every screen but Machines explains pairing.
        let body = if self.state.first_run()
            && !matches!(self.screen, Screen::Machines | Screen::AddSpace)
        {
            self.first_run(cx)
        } else {
            body
        };
        let main_pane = v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .p(LG)
            .gap(MD)
            .children(self.sidebar_collapsed.then(|| self.collapsed_nav_strip(cx)))
            .children(
                self.state
                    .connection_banner()
                    .map(|text| component::alert::Alert::warning("unreachable-spaces", text)),
            )
            .children(self.page_connections(&when, cx))
            // The body takes the spare height so the activity line stays at the bottom.
            .child(v_flex().flex_1().min_h_0().min_w_0().child(body))
            .children(self.activity(cx));

        let shell: AnyElement = if self.sidebar_collapsed {
            main_pane.into_any_element()
        } else {
            let desktop = cx.entity().downgrade();
            h_resizable("desktop-shell")
                .with_state(&self.sidebar_resizable)
                .on_resize(move |state, _, cx| {
                    if let Some(&width) = state.read(cx).sizes().first() {
                        let clamped = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                        let _ = desktop.update(cx, |this, _| {
                            this.sidebar_width = clamped;
                            this.state.set_sidebar_width(f32::from(clamped));
                        });
                    }
                })
                .child(
                    resizable_panel()
                        .size(self.sidebar_width)
                        .size_range(SIDEBAR_MIN_WIDTH..SIDEBAR_MAX_WIDTH)
                        .child(self.sidebar(&when, cx)),
                )
                .child(resizable_panel().child(main_pane))
                .into_any_element()
        };

        h_flex()
            .id("desktop")
            .track_focus(&self.keyboard_focus)
            .key_context(menus::KEY_CONTEXT)
            .capture_action(cx.listener(|this, _: &Undo, window, cx| {
                if !window.has_active_dialog(cx) && this.undo_draft(window, cx) {
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .items_stretch()
            .font_features(TABULAR.clone())
            .text_size(TYPE_BODY)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(shell)
    }
}

#[cfg(test)]
mod activity_tests {
    use super::TaskActivity;
    use threadlane_core::{
        client::{ActivityCursor, RunStep, TaskActivityEntry},
        ids::RunId,
        time::Timestamp,
    };

    fn position(seq: i64) -> ActivityCursor {
        ActivityCursor { run: RunId(1), seq }
    }

    fn steps(from: i64, to: i64) -> Vec<TaskActivityEntry> {
        (from..=to)
            .rev()
            .map(|seq| TaskActivityEntry {
                position: position(seq),
                step: RunStep {
                    kind: "text".into(),
                    description: seq.to_string(),
                    at: Timestamp::from_millis(0),
                },
            })
            .collect()
    }

    #[test]
    fn non_overlapping_refresh_keeps_history_and_its_cursor_until_requested() {
        let mut activity = TaskActivity::default();
        activity.apply_page(None, steps(21, 40), Some(position(21)));
        activity.apply_page(Some(position(21)), steps(1, 20), None);
        activity.apply_page(None, steps(42, 61), Some(position(42)));

        assert_eq!(activity.entries, steps(1, 40));
        assert_eq!(activity.next, None);
        assert!(activity.newest.is_some());

        activity.show_newest();
        assert_eq!(activity.entries, steps(42, 61));
        assert_eq!(activity.next, Some(position(42)));
    }

    #[test]
    fn overlapping_refresh_extends_history_and_empty_pages_can_continue() {
        let mut activity = TaskActivity::default();
        activity.apply_page(None, Vec::new(), Some(position(5)));
        assert!(!activity.is_empty());
        activity.apply_page(Some(position(5)), steps(1, 4), None);
        activity.apply_page(None, steps(3, 6), Some(position(3)));
        assert_eq!(activity.entries, steps(1, 6));
        assert_eq!(activity.next, None);
        assert!(activity.newest.is_none());

        let mut empty = TaskActivity::default();
        empty.apply_page(None, Vec::new(), None);
        assert!(empty.is_empty());
        empty.error = Some((None, "request failed".into()));
        assert!(!empty.is_empty());
    }
}

#[cfg(test)]
mod refresh_screen_tests {
    use super::Screen;

    #[test]
    fn refresh_end_loads_unscoped_auxiliary_screens_including_home() {
        assert!(Screen::Home.loads_after_refresh_pass());
        assert!(Screen::Agents(None).loads_after_refresh_pass());
        assert!(Screen::Routines(None).loads_after_refresh_pass());
        assert!(!Screen::Agents(Some("Work".into())).loads_after_refresh_pass());
        assert!(!Screen::Routines(Some("Work".into())).loads_after_refresh_pass());
        assert!(!Screen::Machines.loads_after_refresh_pass());
    }
}

#[cfg(test)]
mod project_load_tests {
    use super::PendingProjectLoad;

    #[test]
    fn queued_load_preserves_each_refreshed_part_and_uses_latest_follow_up() {
        let mut pending = PendingProjectLoad::default();
        pending.queue((true, false));
        pending.queue((false, true));
        assert_eq!(pending.refreshed, (true, true));
        assert_eq!(pending.latest, (false, true));

        // A feed event requests both parts without invalidating the refresh.
        pending.queue((false, false));
        assert_eq!(pending.refreshed, (true, true));
        assert_eq!(pending.latest, (false, false));
    }
}

/// Whether a hub that was just started answers on `socket` within ten seconds.
async fn listening(socket: &std::path::Path) -> bool {
    for _ in 0..50 {
        if tokio::net::UnixStream::connect(socket).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}
