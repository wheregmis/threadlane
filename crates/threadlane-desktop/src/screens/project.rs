//! A project (wireframes 2, 3, 4): its tasks by status, the conversation
//! with its lead, the mode of each agent bound in it, and its memory; on
//! its header "Pause project" and "+ New task", which skips the lead.

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_core::model::Mode;
use threadlane_desktop::{Line, ModeChoice, Part, When};

use super::{gap_line, heading, link, section};
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
            .children(Tab::ALL.map(Tab::label));
        let paused = self.state.is_paused(space, project);
        let lead = self.state.lead_line(space, project, when);
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
            .gap_2()
            .child(
                Button::new("pause-project")
                    .small()
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
                Button::new("new-task")
                    .small()
                    .primary()
                    .label("+ New task")
                    .accessibility_label("New task, skipping the lead")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state
                            .open_task_form(&task_space, &task_project, !form_open);
                        cx.notify();
                    })),
            );
        let form = form_open.then(|| self.task_form(space, project, cx));
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(heading(project.to_owned()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if paused {
                                format!("{space} · paused")
                            } else {
                                space.to_owned()
                            }),
                    )
                    .children(lead.map(|line| {
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(line)
                    }))
                    .child(div().flex_1())
                    .child(actions),
            )
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
                let title = format!("{} {}", column.title, column.cards.len());
                let cards = column.cards.into_iter().map(|card| {
                    let screen = Screen::Task {
                        space: space.to_owned(),
                        task: card.id,
                    };
                    let answer = card.decision.map(|decision| {
                        let (space, id, view) =
                            (space.to_owned(), decision.to_string(), view.clone());
                        let options = card.options.clone();
                        Button::new(SharedString::from(format!("answer-task-{}", card.id)))
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
                                                    cx,
                                                );
                                            });
                                        },
                                    ));
                                }
                                menu
                            })
                    });
                    v_flex()
                        .gap_1()
                        .p_2()
                        .min_w_0()
                        .bg(cx.theme().background)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .child(
                            link(
                                format!("card-{}", card.id),
                                format!("{} {}", card.id, card.title),
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                            ),
                        )
                        .child(div().text_xs().text_color(muted).child(card.meta))
                        .children(answer)
                });
                v_flex()
                    .flex_1()
                    .min_w(px(120.))
                    .gap_2()
                    .p_2()
                    .bg(cx.theme().muted)
                    .rounded(cx.theme().radius)
                    .child(div().text_xs().text_color(muted).child(title))
                    .children(cards)
            });
        let board = match self.state.project_gap(space, project, Part::Tasks) {
            Some(gap) => div()
                .flex_1()
                .min_w_0()
                .child(gap_line(&gap, "the tasks", cx))
                .into_any_element(),
            None => h_flex()
                .id("board")
                .flex_1()
                .min_w_0()
                .items_start()
                .gap_2()
                .overflow_x_scroll()
                .children(columns)
                .into_any_element(),
        };

        let view = self.state.project(space, project);
        let mut side = v_flex().w(px(220.)).flex_none().gap_2();
        let repositories = view
            .map(|view| view.repositories.as_slice())
            .unwrap_or_default();
        side = side.child(section(format!("Repositories ({})", repositories.len())));
        let gap = self.state.project_gap(space, project, Part::Repositories);
        if let Some(gap) = &gap {
            side = side.child(gap_line(gap, "the repositories", cx));
        } else if repositories.is_empty() {
            side = side.child(div().text_sm().text_color(muted).child("None yet."));
        }
        for repository in repositories {
            side = side.child(
                h_flex()
                    .justify_between()
                    .text_sm()
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
                    .text_sm()
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
                    .text_sm()
                    .child(format!("{}{hub}", machine.name))
                    .child(div().text_color(muted).child(machine.status.to_string())),
            );
        }

        h_flex()
            .flex_1()
            .w_full()
            .min_h_0()
            .min_w_0()
            .items_start()
            .gap_4()
            .child(board)
            .child(side)
            .into_any_element()
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
        let mut lines = v_flex()
            .id("conversation")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_2();
        if let Some(before) = self.state.earlier(space, project) {
            let (space, project) = (space.to_owned(), project.to_owned());
            lines = lines.child(
                Button::new("earlier")
                    .small()
                    .label("Show earlier")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.earlier(space.clone(), project.clone(), before, cx);
                    })),
            );
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
            lines = lines.child(
                div()
                    .text_color(muted)
                    .child("Nothing here yet. Give the lead a goal below."),
            );
        }
        let recorded = self.state.recorded(space, project, when);
        for line in conversation {
            let said = |who: &str, time: String, text: String| {
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(who.to_owned()),
                            )
                            .child(div().text_xs().text_color(muted).child(time)),
                    )
                    .child(text)
            };
            lines = match line {
                Line::Day(day) => lines.child(div().pt_2().text_xs().text_color(muted).child(day)),
                Line::You { text, time } => lines.child(said("You", time, text)),
                Line::Lead { text, time } => lines.child(said("Lead", time, text)),
                Line::Notice { text, time } => lines.child(said("Notice", time, text)),
                Line::Decision {
                    card,
                    time,
                    pending,
                } => {
                    let queued = self.state.queue().iter().find(|queued| {
                        queued.space == space && queued.decision.id.to_string() == card.id
                    });
                    match queued {
                        Some(queued) if pending => lines.child(self.decision(queued, cx)),
                        _ => lines.child(said(
                            "Decision",
                            time,
                            format!("{}: {} (answered)", card.id, card.question),
                        )),
                    }
                }
            };
        }
        if let Some(recorded) = recorded {
            lines = lines.child(self.recorded_section(space, project, recorded, cx));
        }

        let mut side = v_flex().w(px(220.)).flex_none().gap_2();
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
            side = side.child(div().text_sm().text_color(muted).child("No tasks yet."));
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
            side = side.child(
                link(
                    format!("from-{}", summary.task.id),
                    format!("{} {} · {status}", summary.task.id, summary.task.title),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
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
        let any_open = !open.is_empty();
        if open.is_empty() {
            side = side.child(div().text_sm().text_color(muted).child("None."));
        }
        for queued in open {
            side = side.child(
                v_flex()
                    .text_sm()
                    .child(format!(
                        "{} {}",
                        queued.decision.id, queued.decision.question
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("Asked {}", when.moment(queued.decision.created_at))),
                    ),
            );
        }
        side = side.child(section("What the lead knows"));
        match self.state.lead_knows(space, project) {
            Some(knows) => {
                for line in knows.lines() {
                    side = side.child(div().text_sm().child(line));
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
            Button::new("open-memory")
                .small()
                .label("Open memory")
                .on_click(cx.listener(move |this, _, _, cx| this.go(memory.clone(), cx))),
        );

        h_flex()
            .flex_1()
            .min_h_0()
            .items_start()
            .gap_4()
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .gap_2()
                    .child(lines)
                    .children(any_open.then(|| self.note_field()))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.composer)))
                            .child(Button::new("send").primary().label("Send").on_click(
                                cx.listener(|this, _, window, cx| this.send(window, cx)),
                            )),
                    ),
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
            .id("memory-list")
            .w(px(300.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .gap_2();
        if let Some(gap) = self.state.memory_gap(space, project) {
            list = list.child(gap_line(&gap, "the memory", cx));
        }
        let groups = self.state.memory_groups(space, project);
        if groups.is_empty() && self.state.memory_gap(space, project).is_none() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("Nothing remembered yet."),
            );
        }
        for group in groups {
            list = list.child(section(group.title));
            for row in group.files {
                let shown = row.path.clone();
                let (in_space, named, path) = (space.to_owned(), project.to_owned(), row.path);
                list = list.child(
                    v_flex()
                        .child(
                            link(format!("memory-{path}"), row.title)
                                .selected(row.selected)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_memory(&in_space, &named, &path, cx);
                                })),
                        )
                        .child(div().text_xs().text_color(muted).child(shown)),
                );
            }
        }

        let mut detail = v_flex()
            .id("memory-file")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .gap_2();
        match self.state.memory_selected(space, project) {
            None => {
                detail = detail.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("Choose a file to see what it says and who wrote it."),
                );
            }
            Some(path) => {
                let file = self.state.memory_file(space, project);
                let editing = self.state.memory_editing(space, project);
                detail = detail.child(section(path.to_owned()));
                let mut actions = h_flex().gap_2();
                if self.state.memory_can_edit(space, project) {
                    let (in_space, named) = (space.to_owned(), project.to_owned());
                    if editing {
                        let (save_space, save_project) = (in_space.clone(), named.clone());
                        actions = actions
                            .child(Button::new("memory-save").primary().label("Save").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.change_memory(
                                        save_space.clone(),
                                        save_project.clone(),
                                        true,
                                        cx,
                                    );
                                }),
                            ))
                            .child(Button::new("memory-cancel").label("Cancel").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.edit_memory(&in_space, &named, false, cx);
                                }),
                            ));
                    } else {
                        let (delete_space, delete_project) = (in_space.clone(), named.clone());
                        actions =
                            actions
                                .child(Button::new("memory-edit").label("Edit").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.edit_memory(&in_space, &named, true, cx);
                                    }),
                                ))
                                .child(
                                    Button::new("memory-delete")
                                        .ghost()
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
                            .text_sm()
                            .text_color(muted)
                            .child("Written by the hub from the files next to it."),
                    );
                }
                detail = detail.child(actions);
                if let Some(why) = self.state.memory_refused(space, project) {
                    detail = detail.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(why.to_owned()),
                    );
                }
                detail = match (file, editing) {
                    (_, true) => detail.child(Textarea::new(&self.memory_editor)),
                    (Some(file), false) => detail.child(
                        div()
                            .p_2()
                            .text_sm()
                            .bg(cx.theme().muted)
                            .rounded(cx.theme().radius)
                            .whitespace_normal()
                            .child(file.content.clone()),
                    ),
                    (None, false) => detail.child(div().text_sm().text_color(muted).child(
                        if self.state.memory_can_edit(space, project) {
                            "Not written yet. Instructions are loaded into every run."
                        } else {
                            "This file is deleted."
                        },
                    )),
                };
                detail = detail.child(section("Versions"));
                for line in self.state.memory_versions(space, project, when) {
                    detail = detail.child(div().text_sm().text_color(muted).child(line));
                }
            }
        }
        h_flex()
            .flex_1()
            .min_h_0()
            .items_start()
            .gap_4()
            .child(list)
            .child(detail)
            .into_any_element()
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
            .id("agents-tab")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .child(
                h_flex()
                    .gap_4()
                    .p_3()
                    .justify_between()
                    .bg(cx.theme().muted)
                    .rounded(cx.theme().radius)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(section("Project ceiling"))
                            .child(div().text_sm().text_color(muted).child(card.note.clone())),
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
            let step = |id: &str, label: &'static str, name: String, to: Option<u32>| {
                let (space, project, agent) = (space.to_owned(), project.to_owned(), agent.clone());
                Button::new(SharedString::from(format!("{id}-{agent}")))
                    .xsmall()
                    .label(label)
                    .accessibility_label(name)
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
                .gap_1()
                .items_center()
                .child(div().text_sm().text_color(muted).child("At once"))
                .child(step(
                    "fewer",
                    "−",
                    format!("Fewer of {agent}'s tasks at once"),
                    row.at_once.fewer,
                ))
                .child(
                    div()
                        .text_sm()
                        .min_w(px(56.))
                        .child(row.at_once.label.clone()),
                )
                .child(step(
                    "more",
                    "+",
                    format!("More of {agent}'s tasks at once"),
                    row.at_once.more,
                ));
            let (toggle_space, toggle_project, toggle_agent) =
                (space.to_owned(), project.to_owned(), agent.clone());
            let expand = Button::new(SharedString::from(format!("expand-{agent}")))
                .ghost()
                .xsmall()
                .label(if row.open { "▾" } else { "▸" })
                .accessibility_label(if row.open {
                    format!("Hide what {agent} may do here")
                } else {
                    format!("Show what {agent} may do here")
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state
                        .toggle_agent(&toggle_space, &toggle_project, &toggle_agent);
                    cx.notify();
                }));
            let lead = if row.is_lead { " · lead" } else { "" };
            rows = rows.child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .child(
                        v_flex()
                            .w(px(200.))
                            .flex_none()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{agent}{lead}")),
                            )
                            .child(div().text_sm().text_color(muted).child(row.role.clone())),
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
            .gap_1()
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
                    Button::new(SharedString::from(format!("cell-{agent}-{index}")))
                        .xsmall()
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
                    .text_sm()
                    .child(statement.cell.clone())
                    .into_any_element(),
            };
            statements = statements.child(
                h_flex()
                    .gap_3()
                    .justify_between()
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_sm().child(statement.what))
                            .child(div().text_xs().text_color(muted).child(statement.examples)),
                    )
                    .child(cell),
            );
        }
        let mut rules = v_flex()
            .gap_1()
            .w(px(320.))
            .flex_none()
            .child(section(may.rules_title.clone()));
        match may.rules {
            Err(gap) => rules = rules.child(gap_line(&gap, "the standing rules", cx)),
            Ok(listed) if listed.is_empty() => {
                rules = rules.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("None yet. Answering a decision with \"Always allow\" makes one."),
                );
            }
            Ok(listed) => {
                for rule in listed {
                    let (space_revoke, id) = (space.to_owned(), rule.id);
                    let actions = match &rule.confirm {
                        None => h_flex().child(
                            Button::new(SharedString::from(format!("revoke-{}", rule.id)))
                                .ghost()
                                .xsmall()
                                .label("Revoke")
                                .accessibility_label(format!("Revoke {}", rule.allows))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.ask_revoke(&space_revoke, id);
                                    cx.notify();
                                })),
                        ),
                        Some(_) => h_flex()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!("revoke-yes-{}", rule.id)))
                                    .danger()
                                    .xsmall()
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
                                Button::new(SharedString::from(format!("revoke-keep-{}", rule.id)))
                                    .xsmall()
                                    .label("Keep")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.keep_rule();
                                        cx.notify();
                                    })),
                            ),
                    };
                    rules = rules.child(
                        v_flex()
                            .gap_1()
                            .p_2()
                            .bg(cx.theme().background)
                            .rounded(cx.theme().radius)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .justify_between()
                                    .child(div().min_w_0().text_sm().child(rule.allows.clone()))
                                    .child(actions),
                            )
                            .child(div().text_xs().text_color(muted).child(rule.from.clone()))
                            .children(rule.confirm.map(|question| div().text_sm().child(question))),
                    );
                }
            }
        }
        rules = rules.child(div().pt_2().text_sm().text_color(muted).child(may.tainted));
        Some(
            h_flex()
                .items_start()
                .gap_4()
                .p_3()
                .bg(cx.theme().muted)
                .rounded(cx.theme().radius)
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
            let button = Button::new(SharedString::from(format!("{id}-{}", choice.mode)))
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
    Some(div().text_xs().text_color(muted).child(
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
        let label = |text: &'static str| div().w(px(100.)).text_sm().text_color(muted).child(text);
        let agents = self.state.task_agents(space, project);
        let agent_row = if agents.is_empty() {
            div()
                .text_sm()
                .text_color(muted)
                .child("No agent here is on. Turn one on in the Agents tab.")
        } else {
            let mut row = h_flex().gap_1().flex_wrap();
            for agent in agents {
                let picked = form.agent.as_deref() == Some(agent.as_str());
                let chosen = agent.clone();
                row = row.child(
                    Button::new(SharedString::from(format!("task-agent-{agent}")))
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
        let mut repositories = h_flex().gap_1().flex_wrap();
        let names = self.state.task_repositories(space, project);
        if names.is_empty() {
            repositories = repositories.child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("None registered; it works without one."),
            );
        }
        for name in names {
            let picked = form.repositories.contains(&name);
            let toggled = name.clone();
            repositories = repositories.child(
                Button::new(SharedString::from(format!("task-repository-{name}")))
                    .small()
                    .label(name)
                    .selected(picked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.task_form_mut().toggle_repository(&toggled);
                        cx.notify();
                    })),
            );
        }
        let mut waits = h_flex().gap_1().flex_wrap();
        let open = self.state.task_dependencies(space, project);
        if open.is_empty() {
            waits = waits.child(div().text_sm().text_color(muted).child("No open tasks."));
        }
        for (id, name) in open {
            let picked = form.depends_on.contains(&id);
            waits = waits.child(
                Button::new(SharedString::from(format!("task-waits-{id}")))
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
            .gap_2()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(section("New task"))
            .child(Input::new(&self.task_title))
            .child(Input::new(&self.task_goal))
            .child(h_flex().gap_2().child(label("Agent")).child(agent_row))
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Repositories"))
                    .child(repositories),
            )
            .child(h_flex().gap_2().child(label("Waits for")).child(waits))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("It skips the lead and is yours: work you requested."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("task-create")
                            .primary()
                            .label("Create task")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.create_task(in_space.clone(), named.clone(), cx);
                            })),
                    )
                    .child(
                        Button::new("task-close")
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
            return Button::new("add-repository")
                .small()
                .ghost()
                .label("+ Add repository")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.open_repository_form(&in_space, &named, true);
                    cx.notify();
                }))
                .into_any_element();
        }
        let (close_space, close_project) = (space.to_owned(), project.to_owned());
        v_flex()
            .gap_2()
            .child(Input::new(&self.repository_name))
            .child(Input::new(&self.repository_remote))
            .child(Input::new(&self.repository_branch))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Every other paired space is checked first: a repository belongs to one.",
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("repository-add")
                            .small()
                            .primary()
                            .label("Add")
                            .accessibility_label("Add repository")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_repository(in_space.clone(), named.clone(), cx);
                            })),
                    )
                    .child(
                        Button::new("repository-close")
                            .small()
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
        let mut part = v_flex().gap_1().pt_2().child(
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
                    .text_xs()
                    .text_color(muted)
                    .child("These arrived, but no one asked for the work, so no run started."),
            );
            for row in recorded.rows {
                part = part.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .text_sm()
                        .child(
                            div()
                                .w(px(110.))
                                .flex_none()
                                .text_color(muted)
                                .child(format!("{} · {}", row.time, row.source)),
                        )
                        .child(div().min_w_0().child(row.text)),
                );
            }
            if let Some(more) = recorded.more {
                part = part.child(div().text_xs().text_color(muted).child(more));
            }
        }
        part
    }
}
