//! `threadlane-desktop`: the thin desktop app (Plan.md §15 milestone 4).
//!
//! Three screens over [`AppState`]: Home, the conversation with a project's
//! lead, and answering a decision, from the queue, a card, or a system
//! notification. Tokio runs on its own threads beside GPUI's executor
//! (Plan.md §18.4 item 3): every hub call is spawned there and its result
//! handed back to the view.

use std::future::Future;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_client::{ClientCore, ClientDirs, SpaceFeedItem};
use threadlane_core::model::OutboxKind;
use threadlane_desktop::{AppState, Card, decision_of_tag, pump, refresh};
use tokio::runtime::Runtime;
use tokio::sync::{RwLock, mpsc};

/// How often Home asks every hub again, so reachability stays current.
const REFRESH_EVERY: Duration = Duration::from_secs(30);

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
    let (shown, feed) = mpsc::unbounded_channel();
    // With no hub paired there is no feed; Home says so.
    let stream = {
        let _inside = runtime.enter();
        core.follow_all().ok()
    };
    let core = Arc::new(RwLock::new(core));
    if let Some(stream) = stream {
        runtime.spawn(pump(Arc::clone(&core), stream, shown));
    }
    let tokio = runtime.handle().clone();

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
            window_bounds: Some(WindowBounds::centered(size(px(960.), px(720.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some("Threadlane".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let opened = open_window(options, cx, |window, cx| {
            cx.new(|cx| Desktop::new(core, tokio, feed, window, cx))
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

/// Which screen the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    Home,
    /// The conversation with the lead of `project` in `space`.
    Conversation {
        space: String,
        project: String,
    },
}

/// The window's one view: the screens over [`AppState`].
struct Desktop {
    state: AppState,
    core: Arc<RwLock<ClientCore>>,
    tokio: tokio::runtime::Handle,
    screen: Screen,
    composer: Entity<InputState>,
    note: Entity<InputState>,
    /// The outcome of the last action, or why it failed.
    status: Option<SharedString>,
    /// Empty the note at the next render, which has the window it needs.
    clear_note: bool,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Desktop {
    fn new(
        core: Arc<RwLock<ClientCore>>,
        tokio: tokio::runtime::Handle,
        mut feed: mpsc::UnboundedReceiver<SpaceFeedItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer =
            cx.new(|cx| InputState::new(window, cx).placeholder("Message the project's lead"));
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Note (optional)"));
        let subscriptions = vec![cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.send(window, cx);
                }
            },
        )];
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
                cx.background_executor().timer(REFRESH_EVERY).await;
            }
        });
        Self {
            state: AppState::default(),
            core,
            tokio,
            screen: Screen::Home,
            composer,
            note,
            status: None,
            clear_note: false,
            _tasks: vec![follow, refresh_loop],
            _subscriptions: subscriptions,
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
        self.run(
            async move { refresh(&*core.read().await).await },
            |this, refreshed, _| this.state.apply(refreshed),
            cx,
        );
    }

    fn receive(&mut self, item: SpaceFeedItem, cx: &mut Context<Self>) {
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
            if decision {
                self.refresh(cx);
            }
        }
        cx.notify();
    }

    fn go(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
        self.status = None;
        cx.notify();
    }

    fn answer(&mut self, space: String, id: String, option: String, cx: &mut Context<Self>) {
        let note = self.note.read(cx).value().trim().to_owned();
        let core = Arc::clone(&self.core);
        self.status = Some(format!("Answering {space} {id}…").into());
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
                    this.status = Some(format!("Answered {}.", answer.decision.id).into());
                    this.clear_note = true;
                }
                Err(error) => this.status = Some(error.to_string().into()),
            },
            cx,
        );
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Screen::Conversation { space, project } = self.screen.clone() else {
            return;
        };
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        let core = Arc::clone(&self.core);
        self.status = Some("Sending…".into());
        self.run(
            async move {
                core.read()
                    .await
                    .send(Some(&space), Some(&project), &text)
                    .await
            },
            |this, sent, _| {
                this.status = Some(match sent {
                    Ok(_) => "Sent to the lead.".into(),
                    Err(error) => error.to_string().into(),
                });
            },
            cx,
        );
        cx.notify();
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.clear_note) {
            self.note
                .update(cx, |note, cx| note.set_value("", window, cx));
        }
        let muted = cx.theme().muted_foreground;
        let body = match self.screen.clone() {
            Screen::Home => self.home(cx),
            Screen::Conversation { space, project } => self.conversation(&space, &project, cx),
        };
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(body)
            .children(
                self.status
                    .clone()
                    .map(|status| div().text_sm().text_color(muted).child(status)),
            )
    }
}

impl Desktop {
    /// Home: the spaces and their projects, and the merged queue.
    fn home(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let mut spaces = v_flex().gap_2().child(heading("Spaces"));
        if self.state.spaces().is_empty() {
            spaces = spaces.child(div().text_color(muted).child(
                "No hub is paired. Pair one with `threadlane hub join <name> <code>`, then reopen the app.",
            ));
        }
        for space in self.state.spaces() {
            let reach = match &space.unreachable {
                None => div().text_sm().text_color(muted).child("reachable"),
                Some(why) => div()
                    .text_sm()
                    .text_color(danger)
                    .child(format!("unreachable: {why}")),
            };
            let projects = h_flex()
                .gap_2()
                .flex_wrap()
                .children(space.projects.iter().map(|project| {
                    let screen = Screen::Conversation {
                        space: space.name.clone(),
                        project: project.name.clone(),
                    };
                    Button::new(SharedString::from(format!(
                        "open-{}-{}",
                        space.name, project.id
                    )))
                    .label(project.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
                }));
            spaces = spaces.child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(space.name.clone()),
                            )
                            .child(reach),
                    )
                    .child(projects),
            );
        }

        let mut queue = v_flex()
            .gap_2()
            .child(heading("Waiting on you"))
            .child(Input::new(&self.note));
        if self.state.queue().is_empty() {
            queue = queue.child(div().text_color(muted).child("Nothing waits on you."));
        }
        for queued in self.state.queue() {
            let label = match self
                .state
                .project_name(&queued.space, queued.decision.project_id)
            {
                Some(project) => format!("{}/{project}", queued.space),
                None => queued.space.clone(),
            };
            let id = queued.decision.id.to_string();
            let options = queued
                .decision
                .options
                .iter()
                .map(|option| option.label.clone())
                .collect::<Vec<_>>();
            queue = queue.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(format!("{label} · {id}")),
                    )
                    .child(queued.decision.question.clone())
                    .child(self.option_buttons(&queued.space, &id, &options, cx)),
            );
        }

        v_flex()
            .id("home")
            .flex_1()
            .overflow_y_scroll()
            .gap_6()
            .child(spaces)
            .child(queue)
            .into_any_element()
    }

    /// The conversation with one project's lead: its feed and a composer.
    fn conversation(&self, space: &str, project: &str, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut feed = v_flex().id("feed").flex_1().overflow_y_scroll().gap_3();
        let mut empty = true;
        for item in self.state.conversation(space, project) {
            empty = false;
            let kind = match item.kind() {
                OutboxKind::Message => "lead",
                OutboxKind::SystemNotice => "notice",
                OutboxKind::Decision => "decision",
            };
            let mut entry = v_flex()
                .gap_1()
                .child(div().text_xs().text_color(muted).child(kind));
            match Card::of(item) {
                Some(card) => {
                    entry = entry.child(format!("{}: {}", card.id, card.question));
                    entry = if self.state.is_pending(space, &card.id) {
                        entry.child(self.option_buttons(space, &card.id, &card.options, cx))
                    } else {
                        entry.child(div().text_sm().text_color(muted).child("answered"))
                    };
                }
                None => entry = entry.child(item.text()),
            }
            feed = feed.child(entry);
        }
        if empty {
            feed =
                feed.child(div().text_color(muted).child(
                    "Nothing new from this lead since you last looked. Send it a goal below.",
                ));
        }
        v_flex()
            .flex_1()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("back")
                            .label("Home")
                            .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Home, cx))),
                    )
                    .child(heading(format!("{space}/{project}"))),
            )
            .child(feed)
            .child(Input::new(&self.note))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.composer)))
                    .child(
                        Button::new("send")
                            .primary()
                            .label("Send")
                            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                    ),
            )
            .into_any_element()
    }

    /// One button per option of decision `id` in `space`; a click answers
    /// with that option and the note, if any.
    fn option_buttons(
        &self,
        space: &str,
        id: &str,
        options: &[String],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex().gap_2().children(options.iter().map(|option| {
            let (space, id, option) = (space.to_owned(), id.to_owned(), option.clone());
            Button::new(SharedString::from(format!("answer-{space}-{id}-{option}")))
                .label(option.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.answer(space.clone(), id.clone(), option.clone(), cx);
                }))
        }))
    }
}

fn heading(text: impl Into<SharedString>) -> Div {
    div()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}
