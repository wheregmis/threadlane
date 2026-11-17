//! `threadlane-desktop`: the desktop app (Plan.md §11.1).
//!
//! The shell and its screens over [`AppState`]: Home, a project with its
//! tasks, conversation and agents, a task, the agents of each space, and the
//! machines. Decisions are answered from Home, a project, a task, or a
//! system notification. Tokio runs on its own threads beside GPUI's executor
//! (Plan.md §18.4 item 3): every hub call is spawned there and its result
//! handed back to the view. The screens live in `screens/`.

use std::future::Future;
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_client::{ClientCore, ClientDirs, SpaceFeedItem};
use threadlane_core::ids::TaskId;
use threadlane_core::model::Mode;
use threadlane_core::time::{Clock as _, SystemClock, Timestamp};
use threadlane_desktop::{
    Activity, AppState, When, change_memory, decision_of_tag, describe, load_earlier, load_memory,
    load_modes, load_project, load_task, pump, refresh,
};
use tokio::runtime::Runtime;
use tokio::sync::{RwLock, mpsc};

mod screens;

/// How often the app asks every hub again, so reachability, what runs, and
/// the open screen stay current.
const REFRESH_EVERY: Duration = Duration::from_secs(30);

/// How soon the app asks again while a hub is away or refused a part.
const RETRY_EVERY: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("threadlane-desktop: cannot start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let core = match ClientDirs::from_env().and_then(ClientCore::load) {
        Ok(core) => core,
        Err(error) => {
            eprintln!("threadlane-desktop: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The paired hubs are known without asking them: the first frame lists them.
    let state = AppState::paired(core.hubs().iter().map(|hub| hub.name.as_str()));
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
    let start = std::env::var("THREADLANE_DESKTOP_OPEN")
        .ok()
        .and_then(|open| Screen::named(&open));

    application().with_assets(assets::Assets).run(move |cx| {
        cx.set_app_identity("dev.threadlane.desktop", "Threadlane");
        init(cx);
        cx.on_window_closed(|cx, _window| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1180.), px(780.)), cx)),
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
                Desktop::new(state, core, tokio, feed, start, window, cx)
            })
        });
        match opened {
            Ok((_window, view)) => {
                let view = view.downgrade();
                cx.on_system_notification_response(move |response, cx| {
                    let Some((space, id)) = decision_of_tag(&response.tag) else {
                        return;
                    };
                    let (space, id) = (space.to_owned(), id.to_owned());
                    let option = response.action_id.map(|option| option.to_string());
                    let _gone = view.update(cx, |this, cx| match option {
                        Some(option) => this.answer(space, id, option, cx),
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
    /// The agents of every space.
    Agents,
    /// A form for a new project in one space.
    NewProject {
        space: String,
    },
    /// The spaces and their machines.
    Machines,
}

impl Screen {
    /// The screen `THREADLANE_DESKTOP_OPEN` names: `home`, `agents`,
    /// `machines`, `<space>/<project>[/tasks|conversation|agents|memory]`, or
    /// `<space>/T-0001`. For looking at a screen without clicking to it.
    fn named(open: &str) -> Option<Self> {
        let parts: Vec<&str> = open.split('/').collect();
        Some(match parts.as_slice() {
            ["home"] => Self::Home,
            ["agents"] => Self::Agents,
            ["machines"] => Self::Machines,
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
                    ["conversation"] => Tab::Conversation,
                    ["agents"] => Tab::Agents,
                    ["memory"] => Tab::Memory,
                    _ => Tab::Tasks,
                },
            },
            _ => return None,
        })
    }
}

/// The tabs of the project screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Tasks,
    Conversation,
    Agents,
    Memory,
}

/// The feed as the window takes it: what the pumps hand over, and the
/// sender a newly paired space's pump hands its items to.
struct Feed {
    shown: mpsc::UnboundedSender<SpaceFeedItem>,
    received: mpsc::UnboundedReceiver<SpaceFeedItem>,
}

/// The window's one view: the shell and its screens over [`AppState`].
struct Desktop {
    state: AppState,
    core: Arc<RwLock<ClientCore>>,
    tokio: tokio::runtime::Handle,
    screen: Screen,
    composer: Entity<InputState>,
    /// The task screen's composer (wireframe 5).
    task_composer: Entity<InputState>,
    note: Entity<InputState>,
    /// The Memory tab's editor.
    memory_editor: Entity<TextareaState>,
    /// The "+ Add space" form's name and join code.
    space_name: Entity<InputState>,
    join_code: Entity<InputState>,
    /// Where a newly paired space's feed goes.
    shown: mpsc::UnboundedSender<SpaceFeedItem>,
    /// Text to put in the memory editor at the next render.
    fill_editor: Option<String>,
    /// Empty the "+ Add space" form at the next render.
    clear_join: bool,
    /// The new project form's name and description.
    project_name: Entity<InputState>,
    project_description: Entity<InputState>,
    /// Empty the note at the next render, which has the window it needs.
    clear_note: bool,
    /// Empty the new project form at the next render.
    clear_project_form: bool,
    offset_minutes: i64,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Desktop {
    fn new(
        state: AppState,
        core: Arc<RwLock<ClientCore>>,
        tokio: tokio::runtime::Handle,
        feed: Feed,
        start: Option<Screen>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer =
            cx.new(|cx| InputState::new(window, cx).placeholder("Tell your team what you need…"));
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Note (optional)"));
        let task_composer =
            cx.new(|cx| InputState::new(window, cx).placeholder("Add to this task…"));
        let memory_editor = cx.new(|cx| TextareaState::new(window, cx).rows(16));
        let space_name = cx.new(|cx| InputState::new(window, cx).placeholder("Name for the space"));
        let join_code = cx.new(|cx| InputState::new(window, cx).placeholder("Join code"));
        let Feed {
            shown,
            received: mut feed,
        } = feed;
        let project_name = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let project_description =
            cx.new(|cx| InputState::new(window, cx).placeholder("Description (optional)"));
        let subscriptions = vec![
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.send(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &task_composer,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.send_to_task(window, cx);
                    }
                },
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
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    return;
                }
                // By then the refresh has answered: ask again soon if it left gaps.
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
        Self {
            state,
            core,
            tokio,
            screen: start.unwrap_or(Screen::Home),
            composer,
            task_composer,
            note,
            memory_editor,
            space_name,
            join_code,
            shown,
            fill_editor: None,
            clear_join: false,
            project_name,
            project_description,
            clear_note: false,
            clear_project_form: false,
            offset_minutes: local_offset_minutes(),
            _tasks: vec![follow, refresh_loop],
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
        let core = Arc::clone(&self.core);
        // The open screen loads once the refresh is in: the Agents screen
        // needs the projects it lists.
        self.run(
            async move { refresh(&*core.read().await).await },
            |this, refreshed, cx| {
                this.state.apply(refreshed);
                this.load_screen(cx);
            },
            cx,
        );
    }

    /// Loads what the open screen shows beyond what a refresh brings.
    fn load_screen(&mut self, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        match self.screen.clone() {
            Screen::Project { space, project, .. } => {
                // The Conversation tab's card and the Memory tab need the memory.
                let selected = self
                    .state
                    .memory_selected(&space, &project)
                    .map(str::to_owned);
                let (in_space, named) = (space.clone(), project.clone());
                let for_memory = Arc::clone(&core);
                self.run(
                    async move { load_project(&*core.read().await, &space, &project).await },
                    |this, loaded, _| this.state.apply_project(loaded),
                    cx,
                );
                self.run(
                    async move {
                        load_memory(&*for_memory.read().await, &in_space, &named, selected).await
                    },
                    |this, loaded, _| this.state.apply_memory(loaded),
                    cx,
                );
            }
            Screen::Task { space, task } => self.run(
                async move { load_task(&*core.read().await, &space, task).await },
                |this, loaded, _| this.state.apply_task(loaded),
                cx,
            ),
            Screen::Agents => {
                let projects = self
                    .state
                    .targets()
                    .into_iter()
                    .map(|(space, project)| (space.to_owned(), project.to_owned()))
                    .collect();
                self.run(
                    async move { load_modes(&*core.read().await, projects).await },
                    |this, loaded, _| this.state.apply_modes(loaded),
                    cx,
                );
            }
            Screen::Home | Screen::Machines | Screen::NewProject { .. } => {}
        }
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
                if *space == item.space && item.item.project_name.as_deref() == Some(project)
        );
        if let Some(alert) = self.state.receive(item) {
            let decision = matches!(alert, threadlane_desktop::Alert::Decision { .. });
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
            // The card carries no creation time or context: ask its hub.
            if decision && !back {
                self.refresh(cx);
            }
        }
        if back {
            self.refresh(cx);
        }
        // What the lead says often comes with tasks it made or finished.
        if for_open_project {
            self.load_screen(cx);
        }
        cx.notify();
    }

    fn go(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
        self.load_screen(cx);
        cx.notify();
    }

    fn answer(&mut self, space: String, id: String, option: String, cx: &mut Context<Self>) {
        let note = self.note.read(cx).value().trim().to_owned();
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Answering {space} {id}…")));
        self.run(
            async move {
                let note = (!note.is_empty()).then_some(note);
                core.read()
                    .await
                    .answer(Some(&space), &id, &option, note.as_deref())
                    .await
            },
            |this, answered, cx| match answered {
                Ok(answer) => {
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
                    this.clear_note = true;
                    this.load_screen(cx);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Sends the composer's text to the open project's lead, or from Home
    /// to the project the picker names.
    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = match &self.screen {
            Screen::Project { space, project, .. } => Some((space.clone(), project.clone())),
            _ => self
                .state
                .target()
                .map(|(space, project)| (space.to_owned(), project.to_owned())),
        };
        let Some((space, project)) = target else {
            self.state.report(Activity::Failed(
                "No project to send to: pair a hub first.".to_owned(),
            ));
            cx.notify();
            return;
        };
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        let core = Arc::clone(&self.core);
        self.state.report(Activity::Working(format!(
            "Sending to the lead of {space} / {project}…"
        )));
        self.run(
            async move {
                let sent = core
                    .read()
                    .await
                    .send(Some(&space), Some(&project), &text)
                    .await;
                (space, project, text, sent)
            },
            |this, (space, project, text, sent), _| match sent {
                Ok(sent) => {
                    let at = SystemClock.now();
                    this.state
                        .sent(&space, &project, &text, sent.accepted.event_id, at);
                    this.state.report(Activity::Done(format!(
                        "Sent to the lead of {space} / {project}."
                    )));
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
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
        mode: Mode,
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
                    this.state.report(Activity::Done(format!(
                        "{} is now {} in {space} / {project}.",
                        binding.agent, binding.mode
                    )));
                    this.state.mode_set(&space, &project, binding);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
    }

    /// Creates the project the new project form describes, then opens it.
    fn create_project(&mut self, space: String, cx: &mut Context<Self>) {
        let name = self.project_name.read(cx).value().trim().to_owned();
        let description = self.project_description.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.state
                .report(Activity::Failed("Give the project a name.".to_owned()));
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
        let core = Arc::clone(&self.core);
        self.run(
            async move { load_earlier(&*core.read().await, &space, &project, before).await },
            |this, loaded, _| this.state.apply_earlier(loaded),
            cx,
        );
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
    /// Sends the task composer's text to the open task's lane (wireframe 5,
    /// note 5): while a decision is open there, it declines it with the
    /// text as the note.
    fn send_to_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Screen::Task { space, task } = self.screen.clone() else {
            return;
        };
        let text = self.task_composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.task_composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Sending to {space} / {task}…")));
        self.run(
            async move {
                let sent = core
                    .read()
                    .await
                    .send_to_task(Some(&space), &task.to_string(), &text)
                    .await;
                (space, task, text, sent)
            },
            |this, (space, task, text, sent), cx| match sent {
                Ok(sent) => {
                    let at = SystemClock.now();
                    this.state
                        .sent_to_task(&space, task, &text, sent.accepted.event_id, at);
                    this.state
                        .report(Activity::Done(format!("Sent to {space} / {task}.")));
                    // A decision it declined, and the task's new state.
                    this.refresh(cx);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

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
        let core = Arc::clone(&self.core);
        let verb = if save { "Saving" } else { "Deleting" };
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

    /// Pairs this app with the hub of the join code under the name given,
    /// as `threadlane hub join <name> <code>` does; its feed follows.
    fn join_space(&mut self, cx: &mut Context<Self>) {
        let name = self.space_name.read(cx).value().trim().to_owned();
        let code = self.join_code.read(cx).value().trim().to_owned();
        if name.is_empty() || code.is_empty() {
            self.state.report(Activity::Failed(
                "Give the space a name and paste its join code.".to_owned(),
            ));
            cx.notify();
            return;
        }
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
                Ok(()) => {
                    this.state.space_added(&name);
                    this.state.report(Activity::Done(format!("Paired {name}.")));
                    this.clear_join = true;
                    this.refresh(cx);
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
                    this.state.space_removed(&name);
                    this.state
                        .report(Activity::Done(format!("Unpaired {name}.")));
                    let gone = match &this.screen {
                        Screen::Project { space, .. }
                        | Screen::Task { space, .. }
                        | Screen::NewProject { space } => *space == name,
                        _ => false,
                    };
                    if gone {
                        this.go(Screen::Machines, cx);
                    }
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.clear_note) {
            self.note
                .update(cx, |note, cx| note.set_value("", window, cx));
        }
        if let Some(text) = self.fill_editor.take() {
            self.memory_editor
                .update(cx, |editor, cx| editor.set_value(text, window, cx));
        }
        if std::mem::take(&mut self.clear_join) {
            for field in [&self.space_name, &self.join_code] {
                field.update(cx, |field, cx| field.set_value("", window, cx));
            }
        }
        if std::mem::take(&mut self.clear_project_form) {
            for field in [&self.project_name, &self.project_description] {
                field.update(cx, |field, cx| field.set_value("", window, cx));
            }
        }
        let when = self.when();
        let body = match self.screen.clone() {
            Screen::Home => self.home(&when, cx),
            Screen::Project {
                space,
                project,
                tab,
            } => self.project(&space, &project, tab, &when, cx),
            Screen::Task { space, task } => self.task(&space, task, &when, cx),
            Screen::Agents => self.agents(cx),
            Screen::NewProject { space } => self.new_project(&space, cx),
            Screen::Machines => self.machines(&when, cx),
        };
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .p_4()
                    .gap_3()
                    .child(body)
                    .children(self.activity(cx)),
            )
    }
}
