//! A project (wireframes 2, 3, 4): its tasks by status, the conversation
//! with its lead, the mode of each agent bound in it, and its memory.

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::model::Mode;
use threadlane_desktop::{Line, Part, When};

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
        let tabs = [Tab::Tasks, Tab::Conversation, Tab::Agents, Tab::Memory];
        let selected = tabs.iter().position(|known| *known == tab).unwrap_or(0);
        let (in_space, named) = (space.to_owned(), project.to_owned());
        let bar = TabBar::new("project-tabs")
            .underline()
            .selected_index(selected)
            .on_click(cx.listener(move |this, index: &usize, _, cx| {
                let tab = tabs.get(*index).copied().unwrap_or(Tab::Tasks);
                this.go(
                    Screen::Project {
                        space: in_space.clone(),
                        project: named.clone(),
                        tab,
                    },
                    cx,
                );
            }))
            .child("Tasks")
            .child("Conversation")
            .child("Agents")
            .child("Memory");
        let paused = self
            .state
            .space(space)
            .and_then(|known| known.projects.iter().find(|p| p.name == project))
            .is_some_and(|summary| summary.paused);
        let lead = self.state.lead_line(space, project, when);
        let body = match tab {
            Tab::Tasks => self.tasks_tab(space, project, when, cx),
            Tab::Conversation => self.conversation_tab(space, project, when, cx),
            Tab::Agents => self.agents_tab(space, project, cx),
            Tab::Memory => self.memory_tab(space, project, when, cx),
        };
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
                    })),
            )
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

    /// One row per agent bound here, its mode a segmented control.
    fn agents_tab(&self, space: &str, project: &str, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let bindings = self
            .state
            .project(space, project)
            .map(|view| view.bindings.as_slice())
            .unwrap_or_default();
        let mut rows = v_flex().gap_3().child(section("Agents in this project"));
        let gap = self.state.project_gap(space, project, Part::Bindings);
        if let Some(gap) = &gap {
            rows = rows.child(gap_line(gap, "the agents", cx));
        } else if bindings.is_empty() {
            rows = rows.child(div().text_color(muted).child("No agent is bound here yet."));
        }
        for binding in bindings {
            let (space, project, agent) =
                (space.to_owned(), project.to_owned(), binding.agent.clone());
            let lead = if binding.is_lead { " · lead" } else { "" };
            let control = ButtonGroup::new(SharedString::from(format!("mode-{agent}")))
                .outline()
                .compact()
                .children(MODES.iter().map(|(mode, label)| {
                    Button::new(SharedString::from(format!("mode-{agent}-{mode}")))
                        .label(*label)
                        .selected(binding.mode == *mode)
                }))
                .on_click(cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                    let Some((mode, _)) = clicked.first().and_then(|index| MODES.get(*index))
                    else {
                        return;
                    };
                    this.set_mode(space.clone(), project.clone(), agent.clone(), *mode, cx);
                }));
            rows = rows.child(
                h_flex()
                    .gap_4()
                    .child(
                        v_flex()
                            .w(px(220.))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{}{lead}", binding.agent)),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(binding.role.clone()),
                            ),
                    )
                    .child(control),
            );
        }
        rows.into_any_element()
    }
}

/// How the mode control names `mode`.
pub(super) fn mode_name(mode: Mode) -> &'static str {
    MODES
        .iter()
        .find(|(known, _)| *known == mode)
        .map_or("?", |(_, label)| label)
}
