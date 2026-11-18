//! A project (wireframes 2, 3, 4): its tasks by status, the conversation
//! with its lead, the mode of each agent bound in it, and its memory; on
//! its header "Pause project" and "+ New task", which skips the lead.

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab as KitTab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::model::Mode;
use threadlane_desktop::{Line, ModeChoice, Part, When, message_limit, newest_message};

use super::{gap_line, link};
use crate::components::{
    button, card, count, count_badge, decision_scroller, empty_state, field, icon_button, meta,
    page_header, section, status_badge,
};
use crate::icons::AppIcon;
use crate::tokens::{
    BOARD_COLUMN_MIN, FIELD_RADIUS, LG, MD, MEMORY_LIST_WIDTH, MODE_WIDTH, READING_WIDTH,
    ROW_RADIUS, RULES_WIDTH, SHORT_LABEL_WIDTH, SIDEBAR_WIDTH, SM, STEPPER_WIDTH, Status,
    TIMESTAMP_WIDTH, TYPE_BODY, TYPE_META, XS,
};
use crate::{Desktop, Screen, Tab};

/// The four positions of the mode control (Plan.md §2.3).
pub(super) const MODES: [(Mode, &str); 4] = [
    (Mode::Off, "Off"),
    (Mode::OnRequest, "On request"),
    (Mode::Assisted, "Assisted"),
    (Mode::Autonomous, "Autonomous"),
];

impl Desktop {
    pub(crate) fn project(
        &self,
        space: &str,
        project: &str,
        tab: Tab,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (in_space, named) = (space.to_owned(), project.to_owned());
        // Keyed by the tab shown, so the bar's sliding indicator starts fresh
        // under it instead of carrying a half-finished slide from another tab.
        let bar = TabBar::new(SharedString::from(format!("project-tabs-{}", tab.label())))
            .underline()
            .selected_index(tab.index())
            .on_click(cx.listener(move |this, index: &usize, _, cx| {
                let tab = Tab::at(*index).unwrap_or(Tab::Tasks);
                this.go(
                    Screen::Project {
                        space: in_space.clone(),
                        project: named.clone(),
                        tab,
                    },
                    cx,
                );
            }))
            .children(Tab::ALL.map(|tab| {
                let icon = match tab {
                    Tab::Tasks => AppIcon::Columns3,
                    Tab::Conversation => AppIcon::MessagesSquare,
                    Tab::Agents => AppIcon::Bot,
                    Tab::Routines => AppIcon::CalendarClock,
                    Tab::Memory => AppIcon::BookOpen,
                };
                KitTab::new().aria_label(tab.label()).child(
                    h_flex()
                        .gap(XS)
                        .flex_none()
                        .child(Icon::new(icon).size(TYPE_BODY))
                        .child(tab.label()),
                )
            }));
        let paused = self.state.is_paused(space, project);
        let lead = self.state.lead_line(space, project, when);
        let stop = (tab == Tab::Conversation)
            .then(|| self.state.lead_stop(space, project))
            .flatten()
            .map(|target| {
                self.stop_button(space, target, format!("Stop the lead of {project}"), cx)
            });
        let ceiling = self.state.project_ceiling(space, project);
        let lead_mode = self.state.lead_mode(space, project);
        let muted = cx.theme().muted_foreground;
        let body = match tab {
            Tab::Tasks => self.tasks_tab(space, project, when, cx),
            Tab::Conversation => self.conversation_tab(space, project, when, cx),
            Tab::Agents => self.agents_tab(space, project, cx),
            Tab::Routines => self.routines_tab(space, project, when, cx),
            Tab::Memory => self.memory_tab(space, project, when, cx),
        };
        let (pause_space, pause_project) = (space.to_owned(), project.to_owned());
        let (task_space, task_project) = (space.to_owned(), project.to_owned());
        let form_open = self.state.task_form(space, project).is_some();
        let actions = h_flex()
            .gap(SM)
            .child(
                button("pause-project")
                    .small()
                    .icon(if paused {
                        AppIcon::Play
                    } else {
                        AppIcon::Pause
                    })
                    .label(if paused {
                        "Resume project"
                    } else {
                        "Pause project"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_paused(pause_space.clone(), pause_project.clone(), !paused, cx);
                    })),
            )
            .child(
                button("new-task")
                    .small()
                    .primary()
                    .icon(AppIcon::ListPlus)
                    .label("+ New task")
                    .accessibility_label("New task, skipping the lead")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state
                            .open_task_form(&task_space, &task_project, !form_open);
                        cx.notify();
                    })),
            );
        let form = form_open.then(|| self.task_form(space, project, cx));
        let context = h_flex()
            .gap(SM)
            .flex_wrap()
            .child(meta(space.to_owned(), cx))
            .child(meta(format!("Ceiling · {}", mode_name(ceiling)), cx))
            .children(paused.then(|| status_badge("Paused", Status::Waiting, cx)));
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .when(tab == Tab::Conversation, |page| {
                page.max_w(READING_WIDTH).self_center()
            })
            .gap(MD)
            .child(page_header(
                project.to_owned(),
                Some(context),
                Some(actions),
            ))
            .children(lead_mode.filter(|_| tab == Tab::Conversation).map(|mode| {
                div()
                    .text_size(TYPE_BODY)
                    .text_color(muted)
                    .child(format!("Lead mode · {}", mode_name(mode)))
            }))
            .children(lead.map(|line| {
                h_flex()
                    .gap(SM)
                    .flex_wrap()
                    .child(div().text_size(TYPE_BODY).text_color(muted).child(line))
                    .children(stop)
            }))
            .children(form)
            .child(bar)
            .child(body)
            .into_any_element()
    }

    /// The board, then the repositories, agents and machines at a glance.
    fn tasks_tab(
        &self,
        space: &str,
        project: &str,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let view = cx.entity().downgrade();
        let columns = self
            .state
            .board(space, project, when)
            .into_iter()
            .map(|column| {
                let status = Status::task(column.title);
                let proposals = column.proposals.into_iter().map(|proposal| {
                    let id = proposal.id;
                    let button = |label, option: Option<String>| {
                        option.map(|option| {
                            let space = space.to_owned();
                            button(SharedString::from(format!("proposal-{space}-{id}-{label}")))
                                .small()
                                .tab_stop(true)
                                .label(label)
                                .accessibility_label(format!("{label} proposal {id}"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.answer(
                                        space.clone(),
                                        id.to_string(),
                                        option.clone(),
                                        None,
                                        cx,
                                    );
                                }))
                        })
                    };
                    card(cx)
                        .p(SM)
                        .border_dashed()
                        .child(div().whitespace_normal().child(proposal.title))
                        .child(meta(proposal.meta, cx).text_right())
                        .child(
                            h_flex()
                                .gap(XS)
                                .flex_wrap()
                                .children(button("Start", proposal.start))
                                .children(button("Decline", proposal.decline).map(Button::ghost)),
                        )
                });
                let cards = column.cards.into_iter().map(|task_card| {
                    let screen = Screen::Task {
                        space: space.to_owned(),
                        task: task_card.id,
                    };
                    let answer = task_card.decision.map(|decision| {
                        let (space, id, view) =
                            (space.to_owned(), decision.to_string(), view.clone());
                        let options = task_card.options.clone();
                        button(SharedString::from(format!("answer-task-{}", task_card.id)))
                            .small()
                            .primary()
                            .label("Answer")
                            .dropdown_menu(move |mut menu, _, _| {
                                for option in &options {
                                    let (space, id, option, view) =
                                        (space.clone(), id.clone(), option.clone(), view.clone());
                                    menu = menu.item(PopupMenuItem::new(option.clone()).on_click(
                                        move |_, _, cx| {
                                            let _gone = view.update(cx, |this, cx| {
                                                this.answer(
                                                    space.clone(),
                                                    id.clone(),
                                                    option.clone(),
                                                    None,
                                                    cx,
                                                );
                                            });
                                        },
                                    ));
                                }
                                menu
                            })
                    });
                    card(cx)
                        .p(SM)
                        .child(
                            link(
                                format!("card-{}", task_card.id),
                                format!("{} {}", task_card.id, task_card.title),
                            )
                            .min_w_0()
                            .tab_stop(true)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                            ),
                        )
                        .child(meta(task_card.meta, cx).text_right())
                        .children(answer)
                });
                v_flex()
                    .flex_1()
                    .min_w(BOARD_COLUMN_MIN)
                    .gap(SM)
                    .p(SM)
                    .bg(cx.theme().muted)
                    .rounded(ROW_RADIUS)
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(XS)
                            .child(status_badge(column.title, status, cx))
                            .child(count_badge(column.count.to_string(), cx)),
                    )
                    .children(proposals)
                    .children(cards)
                    .children(
                        column
                            .more
                            .map(|label| div().text_size(TYPE_META).text_color(muted).child(label)),
                    )
            });
        let board = match self.state.project_gap(space, project, Part::Tasks) {
            Some(gap) => div()
                .flex_1()
                .min_w_0()
                .child(gap_line(&gap, "the tasks", cx))
                .into_any_element(),
            None => h_flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .items_start()
                .gap(SM)
                .overflow_scrollbar()
                .id("board")
                .children(columns)
                .into_any_element(),
        };

        let view = self.state.project(space, project);
        let mut side = v_flex().gap(SM);
        let decisions: Vec<_> = self
            .state
            .visible_decisions()
            .map(|queued| self.decision(queued, when, cx))
            .collect();
        if !decisions.is_empty() {
            side = side.child(section("Open decisions")).children(decisions);
        }
        let repositories = view
            .map(|view| view.repositories.as_slice())
            .unwrap_or_default();
        side = side.child(section("Repositories").children(count(repositories.len(), cx)));
        let gap = self.state.project_gap(space, project, Part::Repositories);
        if let Some(gap) = &gap {
            side = side.child(gap_line(gap, "the repositories", cx));
        } else if repositories.is_empty() {
            side = side.child(empty_state("None yet.", None, cx));
        }
        for repository in repositories {
            side = side.child(
                h_flex()
                    .justify_between()
                    .text_size(TYPE_BODY)
                    .child(repository.name.clone())
                    .child(
                        div()
                            .text_color(muted)
                            .child(repository.default_branch.clone()),
                    ),
            );
        }
        side = side.child(self.repository_form(space, project, cx));
        side = side.child(section("Agents here"));
        if let Some(gap) = self.state.project_gap(space, project, Part::Bindings) {
            side = side.child(gap_line(&gap, "the agents", cx));
        }
        for binding in view
            .map(|view| view.bindings.as_slice())
            .unwrap_or_default()
        {
            side = side.child(
                h_flex()
                    .justify_between()
                    .text_size(TYPE_BODY)
                    .child(binding.agent.clone())
                    .child(div().text_color(muted).child(mode_name(binding.mode))),
            );
        }
        side = side.child(section("Machines"));
        if let Some(gap) = self
            .state
            .space(space)
            .and_then(|known| known.gap(Part::Machines))
        {
            side = side.child(gap_line(&gap, "the machines", cx));
        }
        for machine in self
            .state
            .space(space)
            .map(|known| known.machines.as_slice())
            .unwrap_or_default()
        {
            let hub = if machine.is_hub { " · hub" } else { "" };
            side = side.child(
                h_flex()
                    .justify_between()
                    .text_size(TYPE_BODY)
                    .child(format!("{}{hub}", machine.name))
                    .child(status_badge(
                        machine.status.to_string(),
                        machine.status.into(),
                        cx,
                    )),
            );
        }

        h_flex()
            .flex_1()
            .w_full()
            .min_h_0()
            .min_w_0()
            .items_start()
            .gap(LG)
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_h_0()
                    .min_w_0()
                    .gap(MD)
                    .child(board)
                    .child(self.project_composer(project, cx).flex_none()),
            )
            .child(
                decision_scroller("board-sidebar", side, &self.decision_scroll)
                    .w(SIDEBAR_WIDTH)
                    .h_full()
                    .flex_none(),
            )
            .into_any_element()
    }

    fn project_composer(&self, project: &str, cx: &mut Context<Self>) -> Div {
        self.render_composer(
            threadlane_desktop::Draft::Lead,
            format!("Message the lead of {project}"),
            None,
            cx,
        )
    }

    /// The conversation with the lead, then the tasks and open decisions of
    /// this project.
    fn conversation_tab(
        &self,
        space: &str,
        project: &str,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut lines = v_flex().gap(SM);
        if let Some(before) = self.state.earlier(space, project) {
            let (space, project) = (space.to_owned(), project.to_owned());
            lines = lines.child(button("earlier").small().label("Show earlier").on_click(
                cx.listener(move |this, _, _, cx| {
                    this.earlier(space.clone(), project.clone(), before, cx);
                }),
            ));
        }
        if let Some(gap) = self.state.project_gap(space, project, Part::Conversation) {
            lines = lines.child(gap_line(&gap, "the conversation", cx));
        }
        let conversation = self.state.conversation(space, project, when);
        if conversation.is_empty()
            && self
                .state
                .project_gap(space, project, Part::Conversation)
                .is_none()
        {
            lines = lines.child(empty_state(
                "Nothing here yet.",
                Some("Give the lead a goal below."),
                cx,
            ));
        }
        let recorded = self.state.recorded(space, project, when);
        let newest = newest_message(&conversation);
        for (index, line) in conversation.iter().enumerate() {
            let limit = message_limit(newest, index);
            let said = |id: String, who: &str, time: &str, text: &str, reply: bool| {
                let id = format!("conversation-{space}-{project}-{id}");
                let quote = text.to_owned();
                v_flex()
                    .gap(XS)
                    .child(
                        h_flex()
                            .gap(SM)
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .min_w_0()
                                    .child(who.to_owned()),
                            )
                            .child(div().flex_1())
                            .children(reply.then(|| {
                                button(SharedString::from(format!("reply-{id}")))
                                    .ghost()
                                    .small()
                                    .tab_stop(true)
                                    .label("Reply")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.reply_to(&quote, window, cx)
                                    }))
                            }))
                            .child(meta(time.to_owned(), cx).text_right()),
                    )
                    .children(
                        (!text.is_empty())
                            .then(|| self.prose(id, space, text.to_owned(), cx).limit(limit)),
                    )
            };
            lines = match line {
                Line::Day(day) => lines.child(Separator::horizontal().label(day.clone())),
                Line::New => lines.child(Separator::horizontal().label("New")),
                Line::You { id, text, time } => {
                    lines.child(said(format!("you-{id}"), "You", time, text, true))
                }
                Line::Lead { seq, text, time } => {
                    lines.child(said(format!("lead-{seq}"), "Lead", time, text, true))
                }
                Line::Notice { seq, text, time } => {
                    lines.child(said(format!("notice-{seq}"), "Notice", time, text, false))
                }
                Line::Sending { id, text, time } => lines.child(said(
                    format!("sending-{id}"),
                    "You · Sending…",
                    time,
                    text,
                    false,
                )),
                Line::Stopped { time } => {
                    lines.child(said(String::new(), "Lead run stopped", time, "", false))
                }
                Line::DeliveryFailure { id, text, time } => lines.child(said(
                    format!("failed-{id}"),
                    "Delivery failed",
                    time,
                    text,
                    false,
                )),
                Line::Digest { digest, time } => lines.child(
                    v_flex()
                        .gap(XS)
                        .child(meta(time.clone(), cx).text_right())
                        .child(self.digest_entry(space, digest.clone(), cx)),
                ),
                Line::Quiet {
                    id,
                    text,
                    toggle,
                    reasons,
                } => {
                    let space = space.to_owned();
                    let id = *id;
                    let in_space = space.clone();
                    let button = button(SharedString::from(format!("quiet-{space}-{id}")))
                        .small()
                        .ghost()
                        .tab_stop(true)
                        .label(*toggle)
                        .accessibility_label(format!("{toggle}: {text}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.toggle_quiet(&in_space, id);
                            cx.notify();
                        }));
                    lines.child(
                        v_flex()
                            .gap(XS)
                            .child(
                                h_flex()
                                    .gap(SM)
                                    .flex_wrap()
                                    .child(
                                        div()
                                            .text_size(TYPE_BODY)
                                            .text_color(muted)
                                            .min_w_0()
                                            .child(text.clone()),
                                    )
                                    .child(button),
                            )
                            .children(reasons.iter().map(|reason| {
                                self.prose(
                                    format!("quiet-{space}-{id}-{}", reason.id),
                                    &space,
                                    format!("{} · {}", reason.time, reason.text),
                                    cx,
                                )
                            })),
                    )
                }
                Line::Decision { card, time, .. } => {
                    match self
                        .state
                        .present_feed_decision(space, project, card, time, when)
                    {
                        Some(display) => lines.child(self.decision_display(display, cx)),
                        None => lines.child(said(
                            format!("decision-{}", card.id),
                            card.label(),
                            time,
                            &format!("{}: {}", card.id, card.question),
                            false,
                        )),
                    }
                }
            };
        }
        if let Some(key) = self
            .reveal_linked_decision
            .as_ref()
            .filter(|key| key.space == space)
            && !conversation.iter().any(
                |line| matches!(line, Line::Decision { card, .. } if card.id == key.id.to_string()),
            )
            && let Some(decision) = self.state.linked_decision(space, key.id)
        {
            let queued = threadlane_desktop::Queued {
                space: space.to_owned(),
                decision: decision.clone(),
            };
            lines = lines.child(
                self.decision_display(self.state.present_decision(&queued, project, when), cx),
            );
        }
        if let Some(recorded) = recorded {
            lines = lines.child(self.recorded_section(space, project, recorded, cx));
        }

        let mut side = v_flex()
            .w(SIDEBAR_WIDTH)
            .h_full()
            .flex_none()
            .gap(SM)
            .overflow_y_scrollbar()
            .id("conversation-sidebar");
        side = side.child(section("From this conversation"));
        // The hub does not record which message made a task, so the tasks
        // are listed here rather than under the message.
        if let Some(gap) = self.state.project_gap(space, project, Part::Tasks) {
            side = side.child(gap_line(&gap, "the tasks", cx));
        }
        let tasks = self
            .state
            .project(space, project)
            .map(|view| view.tasks.as_slice())
            .unwrap_or_default();
        if tasks.is_empty()
            && self
                .state
                .project_gap(space, project, Part::Tasks)
                .is_none()
        {
            side = side.child(
                div()
                    .text_size(TYPE_BODY)
                    .text_color(muted)
                    .child("No tasks yet."),
            );
        }
        for summary in tasks {
            let screen = Screen::Task {
                space: space.to_owned(),
                task: summary.task.id,
            };
            let status = threadlane_desktop::status_label(
                summary.task.status,
                summary.task.waiting_reason.as_deref(),
            );
            side = side
                .child(
                    link(
                        format!("from-{}", summary.task.id),
                        format!("{} {}", summary.task.id, summary.task.title),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
                )
                .child(
                    div()
                        .self_start()
                        .child(status_badge(status, Status::task(status), cx)),
                );
        }
        side = side.child(section("Open decisions"));
        let open: Vec<_> = self
            .state
            .queue()
            .iter()
            .filter(|queued| {
                queued.space == space
                    && self.state.project_name(space, queued.decision.project_id) == Some(project)
            })
            .collect();
        if open.is_empty() {
            side = side.child(div().text_size(TYPE_BODY).text_color(muted).child("None."));
        }
        for queued in open {
            side = side.child(
                v_flex()
                    .text_size(TYPE_BODY)
                    .child(format!(
                        "{} {}",
                        queued.decision.id, queued.decision.question
                    ))
                    .child(
                        meta(
                            format!("Asked {}", when.moment(queued.decision.created_at)),
                            cx,
                        )
                        .text_right(),
                    ),
            );
        }
        side = side.child(section("What the lead knows"));
        match self.state.lead_knows(space, project) {
            Some(knows) => {
                for line in knows.lines() {
                    side = side.child(div().text_size(TYPE_BODY).child(line));
                }
            }
            None => {
                let gap = self
                    .state
                    .memory_gap(space, project)
                    .unwrap_or(threadlane_desktop::Gap::Loading);
                side = side.child(gap_line(&gap, "the memory", cx));
            }
        }
        let memory = Screen::Project {
            space: space.to_owned(),
            project: project.to_owned(),
            tab: Tab::Memory,
        };
        side = side.child(
            button("open-memory")
                .small()
                .icon(AppIcon::BookOpen)
                .label("Open memory")
                .on_click(cx.listener(move |this, _, _, cx| this.go(memory.clone(), cx))),
        );

        h_flex()
            .flex_1()
            .min_h_0()
            .items_start()
            .gap(LG)
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .gap(SM)
                    .child(self.conversation_scroller(space, project, lines, cx))
                    .child(self.project_composer(project, cx).flex_none()),
            )
            .child(side)
            .into_any_element()
    }

    /// The files this project's runs can see, by group, and the one selected
    /// with its versions and your Edit / Save / Delete (wireframe "Memory").
    fn memory_tab(
        &self,
        space: &str,
        project: &str,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut list = v_flex()
            .w(MEMORY_LIST_WIDTH)
            .flex_none()
            .h_full()
            .overflow_y_scrollbar()
            .id("memory-list")
            .gap(SM);
        if let Some(gap) = self.state.memory_gap(space, project) {
            list = list.child(gap_line(&gap, "the memory", cx));
        }
        let groups = self.state.memory_groups(space, project);
        if groups.is_empty() && self.state.memory_gap(space, project).is_none() {
            list = list.child(empty_state("Nothing remembered yet.", None, cx));
        }
        for group in groups {
            list = list.child(section(group.title));
            for row in group.files {
                let shown = row.path.clone();
                let (in_space, named, path) = (space.to_owned(), project.to_owned(), row.path);
                list = list.child(
                    v_flex()
                        .child(
                            crate::components::row(
                                format!("memory-{path}"),
                                AppIcon::FileText,
                                row.title,
                                None,
                                row.selected,
                                cx,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.select_memory(&in_space, &named, &path, cx);
                                },
                            )),
                        )
                        .child(meta(shown, cx).px(MD)),
                );
            }
        }

        let mut detail = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scrollbar()
            .id("memory-file")
            .gap(SM);
        detail = detail.child(self.dreaming_settings(space, project, when, cx));
        match self.state.memory_selected(space, project) {
            None => {
                detail = detail.child(
                    div()
                        .text_size(TYPE_BODY)
                        .text_color(muted)
                        .child("Choose a file to see what it says and who wrote it."),
                );
            }
            Some(path) => {
                let file = self.state.memory_file(space, project);
                let editing = self.state.memory_editing(space, project);
                detail = detail.child(section(path.to_owned()));
                let mut actions = h_flex().gap(SM);
                if self.state.memory_can_edit(space, project) {
                    let (in_space, named) = (space.to_owned(), project.to_owned());
                    if editing {
                        let (save_space, save_project) = (in_space.clone(), named.clone());
                        actions = actions
                            .child(
                                button("memory-save")
                                    .primary()
                                    .icon(AppIcon::Save)
                                    .label("Save")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.change_memory(
                                            save_space.clone(),
                                            save_project.clone(),
                                            true,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                button("memory-cancel")
                                    .icon(AppIcon::X)
                                    .label("Cancel")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.edit_memory(&in_space, &named, false, cx);
                                    })),
                            );
                    } else {
                        let (delete_space, delete_project) = (in_space.clone(), named.clone());
                        actions = actions
                            .child(
                                button("memory-edit")
                                    .icon(AppIcon::Pencil)
                                    .label("Edit")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.edit_memory(&in_space, &named, true, cx);
                                    })),
                            )
                            .child(
                                button("memory-delete")
                                    .ghost()
                                    .icon(AppIcon::Trash)
                                    .label("Delete")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.change_memory(
                                            delete_space.clone(),
                                            delete_project.clone(),
                                            false,
                                            cx,
                                        );
                                    })),
                            );
                    }
                } else if file.is_some() {
                    actions = actions.child(
                        div()
                            .text_size(TYPE_BODY)
                            .text_color(muted)
                            .child("Written by the hub from the files next to it."),
                    );
                }
                detail = detail.child(actions);
                if let Some(why) = self.state.memory_refused(space, project) {
                    detail = detail.child(
                        div()
                            .text_size(TYPE_BODY)
                            .text_color(cx.theme().danger)
                            .child(why.to_owned()),
                    );
                }
                detail = match (file, editing) {
                    (_, true) => {
                        detail.child(Textarea::new(&self.memory_editor).rounded(FIELD_RADIUS))
                    }
                    (Some(file), false) => detail.child(
                        div()
                            .p(SM)
                            .text_size(TYPE_BODY)
                            .bg(cx.theme().muted)
                            .rounded(ROW_RADIUS)
                            .whitespace_normal()
                            .child(self.prose(
                                format!("memory-body-{space}-{project}-{path}"),
                                space,
                                file.content.clone(),
                                cx,
                            )),
                    ),
                    (None, false) => {
                        detail.child(div().text_size(TYPE_BODY).text_color(muted).child(
                            if self.state.memory_can_edit(space, project) {
                                "Not written yet. Instructions are loaded into every run."
                            } else {
                                "This file is deleted."
                            },
                        ))
                    }
                };
                detail = detail.child(section("Versions"));
                for line in self.state.memory_versions(space, project, when) {
                    detail = detail.child(meta(line, cx));
                }
            }
        }
        h_flex()
            .flex_1()
            .min_h_0()
            .items_start()
            .gap(LG)
            .child(list)
            .child(detail)
            .into_any_element()
    }

    fn dreaming_settings(
        &self,
        space: &str,
        project: &str,
        when: &When,
        cx: &mut Context<Self>,
    ) -> Div {
        let card = self.state.dreaming_card(space, project, when);
        let muted = cx.theme().muted_foreground;
        let (in_space, named) = (space.to_owned(), project.to_owned());
        let mut panel = v_flex()
            .gap(SM)
            .p(MD)
            .rounded(ROW_RADIUS)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                Switch::new("dreaming-switch")
                    .label("Dreaming")
                    .accessibility_label("Dreaming")
                    .checked(card.enabled)
                    .disabled(card.disabled)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.toggle_dreaming(in_space.clone(), named.clone(), *checked, cx);
                    })),
            )
            .child(
                div()
                    .text_size(TYPE_BODY)
                    .text_color(muted)
                    .child(card.cadence),
            );
        if card.enabled {
            let (in_space, named) = (space.to_owned(), project.to_owned());
            panel = panel.child(
                h_flex()
                    .gap(SM)
                    .child(
                        div()
                            .flex_1()
                            .child(field(&self.dream_cadence).disabled(card.disabled)),
                    )
                    .child(
                        button("save-dream-cadence")
                            .icon(AppIcon::Save)
                            .label("Save cadence")
                            .disabled(card.disabled)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.save_dream_cadence(in_space.clone(), named.clone(), cx);
                            })),
                    ),
            );
        }
        panel
            .children(
                card.last
                    .into_iter()
                    .map(|line| div().text_size(TYPE_BODY).child(line)),
            )
            .child(section("Memory check"))
            .children(
                card.findings
                    .into_iter()
                    .map(|line| div().text_size(TYPE_BODY).text_color(muted).child(line)),
            )
    }

    /// The project ceiling, then one row per agent bound here: its mode a
    /// segmented control with what is above the ceiling locked, how many of
    /// its tasks run at once, and, expanded, what it may do here without
    /// asking and its standing rules (wireframe 4).
    fn agents_tab(&self, space: &str, project: &str, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let card = self.state.ceiling_card(space, project);
        let (at_space, at_project, choices) =
            (space.to_owned(), project.to_owned(), card.choices.clone());
        let ceiling = mode_control(format!("ceiling-{project}"), &card.choices).on_click(
            cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                if let Some(mode) = unlocked(&choices, clicked) {
                    this.set_ceiling(at_space.clone(), at_project.clone(), mode, cx);
                }
            }),
        );
        let mut rows = v_flex()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .id("agents-tab")
            .gap(MD)
            .child(
                h_flex()
                    .gap(LG)
                    .p(MD)
                    .justify_between()
                    .flex_wrap()
                    .bg(cx.theme().muted)
                    .rounded(ROW_RADIUS)
                    .child(
                        v_flex().min_w_0().child(section("Project ceiling")).child(
                            div()
                                .text_size(TYPE_BODY)
                                .text_color(muted)
                                .child(card.note.clone()),
                        ),
                    )
                    .child(ceiling),
            )
            .children(locked_note(&card.choices, muted))
            .child(section("Agents in this project"));
        let gap = self.state.project_gap(space, project, Part::Bindings);
        let agents = self.state.agent_rows(space, project);
        if let Some(gap) = &gap {
            rows = rows.child(gap_line(gap, "the agents", cx));
        } else if agents.is_empty() {
            rows = rows.child(div().text_color(muted).child("No agent is bound here yet."));
        }
        for row in agents {
            let agent = row.agent.clone();
            let (in_space, in_project, named, choices) = (
                space.to_owned(),
                project.to_owned(),
                agent.clone(),
                row.choices.clone(),
            );
            let control = mode_control(format!("mode-{agent}"), &row.choices).on_click(
                cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                    if let Some(mode) = unlocked(&choices, clicked) {
                        this.set_mode(
                            in_space.clone(),
                            in_project.clone(),
                            named.clone(),
                            mode,
                            cx,
                        );
                    }
                }),
            );
            let step = |id: &str, icon, name: String, to: Option<u32>| {
                let (space, project, agent) = (space.to_owned(), project.to_owned(), agent.clone());
                icon_button(format!("{id}-{agent}"), icon, name)
                    .disabled(to.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(to) = to {
                            this.set_max_parallel(
                                space.clone(),
                                project.clone(),
                                agent.clone(),
                                to,
                                cx,
                            );
                        }
                    }))
            };
            let at_once = h_flex()
                .gap(XS)
                .items_center()
                .child(
                    div()
                        .text_size(TYPE_BODY)
                        .text_color(muted)
                        .child("At once"),
                )
                .child(step(
                    "fewer",
                    AppIcon::Minus,
                    format!("Fewer of {agent}'s tasks at once"),
                    row.at_once.fewer,
                ))
                .child(
                    div()
                        .text_size(TYPE_BODY)
                        .min_w(STEPPER_WIDTH)
                        .text_right()
                        .child(row.at_once.label.clone()),
                )
                .child(step(
                    "more",
                    AppIcon::Plus,
                    format!("More of {agent}'s tasks at once"),
                    row.at_once.more,
                ));
            let (toggle_space, toggle_project, toggle_agent) =
                (space.to_owned(), project.to_owned(), agent.clone());
            let expand = icon_button(
                format!("expand-{agent}"),
                if row.open {
                    AppIcon::ChevronDown
                } else {
                    AppIcon::ChevronRight
                },
                if row.open {
                    format!("Hide what {agent} may do here")
                } else {
                    format!("Show what {agent} may do here")
                },
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state
                    .toggle_agent(&toggle_space, &toggle_project, &toggle_agent);
                cx.notify();
            }));
            let lead = if row.is_lead { " · lead" } else { "" };
            rows = rows.child(
                h_flex()
                    .gap(LG)
                    .items_center()
                    .flex_wrap()
                    .child(
                        v_flex()
                            .w(MODE_WIDTH)
                            .flex_none()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{agent}{lead}")),
                            )
                            .child(
                                div()
                                    .text_size(TYPE_BODY)
                                    .text_color(muted)
                                    .child(row.role.clone()),
                            ),
                    )
                    .child(control)
                    .child(at_once)
                    .child(expand),
            );
            rows = rows.children(locked_note(&row.choices, muted));
            if row.open {
                rows = rows.children(self.may_do(space, project, &agent, cx));
            }
        }
        rows.into_any_element()
    }

    /// The expanded part of `agent`'s row: the matrix as plain statements,
    /// the rows that can change as dropdowns, and its standing rules.
    fn may_do(
        &self,
        space: &str,
        project: &str,
        agent: &str,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let muted = cx.theme().muted_foreground;
        let may = self.state.may_do(space, project, agent)?;
        let view = cx.entity().downgrade();
        let mut statements = v_flex()
            .gap(XS)
            .flex_1()
            .min_w_0()
            .child(section(may.title.clone()));
        for (index, statement) in may.statements.into_iter().enumerate() {
            let cell: AnyElement = match statement.tier {
                Some(tier) if !statement.choices.is_empty() => {
                    let (space, project, agent, view) = (
                        space.to_owned(),
                        project.to_owned(),
                        agent.to_owned(),
                        view.clone(),
                    );
                    let choices = statement.choices.clone();
                    button(SharedString::from(format!("cell-{agent}-{index}")))
                        .small()
                        .label(format!("{} ▾", statement.cell))
                        .accessibility_label(format!(
                            "{}: {}. Change it",
                            statement.what, statement.cell
                        ))
                        .dropdown_menu(move |mut menu, _, _| {
                            for choice in &choices {
                                let (space, project, agent, view, cell) = (
                                    space.clone(),
                                    project.clone(),
                                    agent.clone(),
                                    view.clone(),
                                    choice.cell,
                                );
                                menu = menu.item(
                                    PopupMenuItem::new(choice.label.clone())
                                        .checked(choice.selected)
                                        .on_click(move |_, _, cx| {
                                            let _gone = view.update(cx, |this, cx| {
                                                this.set_override(
                                                    space.clone(),
                                                    project.clone(),
                                                    agent.clone(),
                                                    tier,
                                                    cell,
                                                    cx,
                                                );
                                            });
                                        }),
                                );
                            }
                            menu
                        })
                        .into_any_element()
                }
                _ => div()
                    .text_size(TYPE_BODY)
                    .child(statement.cell.clone())
                    .into_any_element(),
            };
            statements = statements.child(
                h_flex()
                    .gap(MD)
                    .justify_between()
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_size(TYPE_BODY).child(statement.what))
                            .child(
                                div()
                                    .text_size(TYPE_META)
                                    .text_color(muted)
                                    .child(statement.examples),
                            ),
                    )
                    .child(cell),
            );
        }
        let mut rules = v_flex()
            .gap(XS)
            .w(RULES_WIDTH)
            .max_w_full()
            .flex_none()
            .child(section(may.rules_title.clone()));
        match may.rules {
            Err(gap) => rules = rules.child(gap_line(&gap, "the standing rules", cx)),
            Ok(listed) if listed.is_empty() => {
                rules = rules.child(
                    div()
                        .text_size(TYPE_BODY)
                        .text_color(muted)
                        .child("None yet. Answering a decision with \"Always allow\" makes one."),
                );
            }
            Ok(listed) => {
                for rule in listed {
                    let (space_revoke, id) = (space.to_owned(), rule.id);
                    let actions = match &rule.confirm {
                        None => h_flex().child(
                            button(SharedString::from(format!("revoke-{}", rule.id)))
                                .ghost()
                                .small()
                                .icon(AppIcon::ShieldOff)
                                .label("Revoke")
                                .accessibility_label(format!("Revoke {}", rule.allows))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.ask_revoke(&space_revoke, id);
                                    cx.notify();
                                })),
                        ),
                        Some(_) => h_flex()
                            .gap(XS)
                            .child(
                                button(SharedString::from(format!("revoke-yes-{}", rule.id)))
                                    .danger()
                                    .small()
                                    .icon(AppIcon::ShieldOff)
                                    .label("Revoke")
                                    .accessibility_label(format!(
                                        "Confirm revoking {}",
                                        rule.allows
                                    ))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.revoke_rule(space_revoke.clone(), id, cx);
                                    })),
                            )
                            .child(
                                button(SharedString::from(format!("revoke-keep-{}", rule.id)))
                                    .small()
                                    .label("Keep")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.keep_rule();
                                        cx.notify();
                                    })),
                            ),
                    };
                    rules = rules.child(
                        v_flex()
                            .gap(XS)
                            .p(SM)
                            .bg(cx.theme().background)
                            .rounded(ROW_RADIUS)
                            .child(
                                h_flex()
                                    .gap(SM)
                                    .justify_between()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_size(TYPE_BODY)
                                            .child(rule.allows.clone()),
                                    )
                                    .child(actions),
                            )
                            .child(
                                div()
                                    .text_size(TYPE_META)
                                    .text_color(muted)
                                    .child(rule.from.clone()),
                            )
                            .children(
                                rule.confirm
                                    .map(|question| div().text_size(TYPE_BODY).child(question)),
                            ),
                    );
                }
            }
        }
        rules = rules.child(
            div()
                .pt(SM)
                .text_size(TYPE_BODY)
                .text_color(muted)
                .child(may.tainted),
        );
        Some(
            h_flex()
                .items_start()
                .flex_wrap()
                .gap(LG)
                .p(MD)
                .bg(cx.theme().muted)
                .rounded(ROW_RADIUS)
                .child(statements)
                .child(rules),
        )
    }
}

/// A segmented mode control: the locked positions struck through, not
/// clickable, and named with why.
fn mode_control(id: String, choices: &[ModeChoice]) -> ButtonGroup {
    ButtonGroup::new(SharedString::from(id.clone()))
        .outline()
        .compact()
        .children(choices.iter().map(|choice| {
            let button = button(SharedString::from(format!("{id}-{}", choice.mode)))
                .selected(choice.selected);
            match &choice.locked {
                None => button.label(choice.label),
                // A stored mode above the ceiling stays shown as selected.
                Some(why) => button
                    .disabled(!choice.selected)
                    .tooltip(why.clone())
                    .accessibility_label(format!("{}, locked: {why}", choice.label))
                    .child(div().line_through().child(choice.label)),
            }
        }))
}

/// The mode of the clicked position, unless it is locked.
fn unlocked(choices: &[ModeChoice], clicked: &[usize]) -> Option<Mode> {
    let choice = choices.get(*clicked.first()?)?;
    choice.locked.is_none().then_some(choice.mode)
}

/// `Autonomous is locked: above this project's ceiling, On request.`, for
/// the locked positions of a control, if any.
fn locked_note(choices: &[ModeChoice], muted: Hsla) -> Option<Div> {
    let locked: Vec<&ModeChoice> = choices.iter().filter(|c| c.locked.is_some()).collect();
    let why = locked.first()?.locked.clone()?;
    let names: Vec<&str> = locked.iter().map(|choice| choice.label).collect();
    let verb = if names.len() == 1 { "is" } else { "are" };
    Some(div().text_size(TYPE_META).text_color(muted).child(
        match locked.iter().find(|choice| choice.selected) {
            Some(set) => format!(
                "Set to {}, which is above the ceiling, so it acts at the ceiling. {} {verb} \
                     locked: {why}.",
                set.label,
                names.join(" and ")
            ),
            None => format!("{} {verb} locked: {why}.", names.join(" and ")),
        },
    ))
}

/// How the mode control names `mode`.
pub(super) fn mode_name(mode: Mode) -> &'static str {
    MODES
        .iter()
        .find(|(known, _)| *known == mode)
        .map_or("?", |(_, label)| label)
}

impl Desktop {
    /// The "+ New task" form (wireframe 2, note 3): title, goal, the agent
    /// that does it, its repositories, and the tasks it waits for.
    fn task_form(&self, space: &str, project: &str, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let Some(form) = self.state.task_form(space, project) else {
            return div();
        };
        let label = |text: &'static str| {
            div()
                .w(SHORT_LABEL_WIDTH)
                .text_size(TYPE_BODY)
                .text_color(muted)
                .child(text)
        };
        let agents = self.state.task_agents(space, project);
        let agent_row = if agents.is_empty() {
            div()
                .text_size(TYPE_BODY)
                .text_color(muted)
                .child("No agent here is on. Turn one on in the Agents tab.")
        } else {
            let mut row = h_flex().gap(XS).flex_wrap();
            for agent in agents {
                let picked = form.agent.as_deref() == Some(agent.as_str());
                let chosen = agent.clone();
                row = row.child(
                    button(SharedString::from(format!("task-agent-{agent}")))
                        .small()
                        .label(agent)
                        .selected(picked)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.task_form_mut().agent = Some(chosen.clone());
                            cx.notify();
                        })),
                );
            }
            row
        };
        let mut repositories = h_flex().gap(XS).flex_wrap();
        let names = self.state.task_repositories(space, project);
        if names.is_empty() {
            repositories = repositories.child(
                div()
                    .text_size(TYPE_BODY)
                    .text_color(muted)
                    .child("None registered; it works without one."),
            );
        }
        for name in names {
            let picked = form.repositories.contains(&name);
            let toggled = name.clone();
            repositories = repositories.child(
                button(SharedString::from(format!("task-repository-{name}")))
                    .small()
                    .label(name)
                    .selected(picked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.task_form_mut().toggle_repository(&toggled);
                        cx.notify();
                    })),
            );
        }
        let mut waits = h_flex().gap(XS).flex_wrap();
        let open = self.state.task_dependencies(space, project);
        if open.is_empty() {
            waits = waits.child(
                div()
                    .text_size(TYPE_BODY)
                    .text_color(muted)
                    .child("No open tasks."),
            );
        }
        for (id, name) in open {
            let picked = form.depends_on.contains(&id);
            waits = waits.child(
                button(SharedString::from(format!("task-waits-{id}")))
                    .small()
                    .label(name)
                    .selected(picked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.task_form_mut().toggle_dependency(id);
                        cx.notify();
                    })),
            );
        }
        let (in_space, named) = (space.to_owned(), project.to_owned());
        let (close_space, close_project) = (space.to_owned(), project.to_owned());
        v_flex()
            .gap(SM)
            .p(MD)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(ROW_RADIUS)
            .child(section("New task"))
            .child(field(&self.task_title))
            .child(field(&self.task_goal))
            .child(h_flex().gap(SM).child(label("Agent")).child(agent_row))
            .child(
                h_flex()
                    .gap(SM)
                    .child(label("Repositories"))
                    .child(repositories),
            )
            .child(h_flex().gap(SM).child(label("Waits for")).child(waits))
            .child(
                div()
                    .text_size(TYPE_META)
                    .text_color(muted)
                    .child("It skips the lead and is yours: work you requested."),
            )
            .child(
                h_flex()
                    .gap(SM)
                    .child(
                        button("task-create")
                            .primary()
                            .icon(AppIcon::ListPlus)
                            .label("Create task")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.create_task(in_space.clone(), named.clone(), cx);
                            })),
                    )
                    .child(
                        button("task-close")
                            .icon(AppIcon::X)
                            .label("Cancel")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state
                                    .open_task_form(&close_space, &close_project, false);
                                cx.notify();
                            })),
                    ),
            )
    }

    /// "+ Add repository", or the form it opens: name, remote, default
    /// branch.
    fn repository_form(&self, space: &str, project: &str, cx: &mut Context<Self>) -> AnyElement {
        let (in_space, named) = (space.to_owned(), project.to_owned());
        if !self.state.repository_form_open(space, project) {
            return button("add-repository")
                .small()
                .ghost()
                .self_start()
                .tab_stop(true)
                .icon(AppIcon::GitFork)
                .label("Add repository")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.open_repository_form(&in_space, &named, true);
                    cx.notify();
                }))
                .into_any_element();
        }
        let (close_space, close_project) = (space.to_owned(), project.to_owned());
        v_flex()
            .gap(SM)
            .child(field(&self.repository_name))
            .child(field(&self.repository_remote))
            .child(field(&self.repository_branch))
            .child(
                div()
                    .text_size(TYPE_META)
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Every other paired space is checked first: a repository belongs to one.",
                    ),
            )
            .child(
                h_flex()
                    .gap(SM)
                    .child(
                        button("repository-add")
                            .small()
                            .primary()
                            .icon(AppIcon::GitFork)
                            .label("Add")
                            .accessibility_label("Add repository")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_repository(in_space.clone(), named.clone(), cx);
                            })),
                    )
                    .child(
                        button("repository-close")
                            .small()
                            .icon(AppIcon::X)
                            .label("Cancel")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.open_repository_form(
                                    &close_space,
                                    &close_project,
                                    false,
                                );
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// "Recorded, not acted on (N)", collapsed until clicked: what arrived
    /// that no one asked for, so no run started.
    fn recorded_section(
        &self,
        space: &str,
        project: &str,
        recorded: threadlane_desktop::Recorded,
        cx: &mut Context<Self>,
    ) -> Div {
        let muted = cx.theme().muted_foreground;
        let (in_space, named) = (space.to_owned(), project.to_owned());
        let mark = if recorded.open { "▾" } else { "▸" };
        let mut part = v_flex().gap(XS).pt(SM).child(
            link("recorded-toggle", format!("{mark} {}", recorded.title)).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.state.toggle_recorded(&in_space, &named);
                    cx.notify();
                },
            )),
        );
        if recorded.open {
            part = part.child(
                div()
                    .text_size(TYPE_META)
                    .text_color(muted)
                    .child("These arrived, but no one asked for the work, so no run started."),
            );
            for row in recorded.rows {
                part = part.child(
                    h_flex()
                        .gap(SM)
                        .items_start()
                        .text_size(TYPE_BODY)
                        .child(
                            div()
                                .w(TIMESTAMP_WIDTH)
                                .flex_none()
                                .text_right()
                                .text_color(muted)
                                .child(format!("{} · {}", row.time, row.source)),
                        )
                        .child(div().min_w_0().child(row.text)),
                );
            }
            if let Some(more) = recorded.more {
                part = part.child(div().text_size(TYPE_META).text_color(muted).child(more));
            }
        }
        part
    }
}
