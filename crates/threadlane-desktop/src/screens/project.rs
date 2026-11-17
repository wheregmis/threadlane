//! A project (wireframes 2, 3, 4): its tasks by status, the conversation
//! with its lead, and the mode of each agent bound in it.

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::model::Mode;
use threadlane_desktop::{Line, When};

use super::{heading, section};
use crate::{Desktop, Screen, Tab};

/// The four positions of the mode control (Plan.md §2.3).
const MODES: [(Mode, &str); 4] = [
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
        let tabs = [Tab::Tasks, Tab::Conversation, Tab::Agents];
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
            .child("Agents");
        let paused = self
            .state
            .space(space)
            .and_then(|known| known.projects.iter().find(|p| p.name == project))
            .is_some_and(|summary| summary.paused);
        let body = match tab {
            Tab::Tasks => self.tasks_tab(space, project, when, cx),
            Tab::Conversation => self.conversation_tab(space, project, when, cx),
            Tab::Agents => self.agents_tab(space, project, cx),
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                h_flex().gap_2().child(heading(project.to_owned())).child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if paused {
                            format!("{space} · paused")
                        } else {
                            space.to_owned()
                        }),
                ),
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
                    let answer = card.decision.map(|_| {
                        let screen = screen.clone();
                        Button::new(SharedString::from(format!("answer-task-{}", card.id)))
                            .small()
                            .primary()
                            .label("Answer")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                            )
                    });
                    v_flex()
                        .id(SharedString::from(format!("card-{}", card.id)))
                        .gap_1()
                        .p_2()
                        .bg(cx.theme().background)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
                        .child(div().text_sm().child(format!("{} {}", card.id, card.title)))
                        .child(div().text_xs().text_color(muted).child(card.meta))
                        .children(answer)
                });
                v_flex()
                    .w(px(170.))
                    .flex_none()
                    .gap_2()
                    .p_2()
                    .bg(cx.theme().muted)
                    .rounded(cx.theme().radius)
                    .child(div().text_xs().text_color(muted).child(title))
                    .children(cards)
            });
        let board = h_flex()
            .id("board")
            .flex_1()
            .min_w_0()
            .items_start()
            .gap_2()
            .overflow_x_scroll()
            .children(columns);

        let view = self.state.project(space, project);
        let mut side = v_flex().w(px(250.)).flex_none().gap_2();
        let repositories = view
            .map(|view| view.repositories.as_slice())
            .unwrap_or_default();
        side = side.child(section(format!("Repositories ({})", repositories.len())));
        if repositories.is_empty() {
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
            .min_h_0()
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
        let conversation = self.state.conversation(space, project, when);
        if conversation.is_empty() {
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

        let mut side = v_flex().w(px(250.)).flex_none().gap_2();
        side = side.child(section("From this conversation"));
        let tasks = self
            .state
            .project(space, project)
            .map(|view| view.tasks.as_slice())
            .unwrap_or_default();
        if tasks.is_empty() {
            side = side.child(div().text_sm().text_color(muted).child("No tasks yet."));
        }
        for summary in tasks {
            let screen = Screen::Task {
                space: space.to_owned(),
                task: summary.task.id,
            };
            side = side.child(
                h_flex()
                    .id(SharedString::from(format!("from-{}", summary.task.id)))
                    .justify_between()
                    .gap_2()
                    .text_sm()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
                    .child(format!("{} {}", summary.task.id, summary.task.title))
                    .child(
                        div()
                            .text_color(muted)
                            .child(threadlane_desktop::status_label(
                                summary.task.status,
                                summary.task.waiting_reason.as_deref(),
                            )),
                    ),
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
                    .child(self.note_field())
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

    /// One row per agent bound here, its mode a segmented control.
    fn agents_tab(&self, space: &str, project: &str, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let bindings = self
            .state
            .project(space, project)
            .map(|view| view.bindings.as_slice())
            .unwrap_or_default();
        let mut rows = v_flex().gap_3().child(section("Agents in this project"));
        if bindings.is_empty() {
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

fn mode_name(mode: Mode) -> &'static str {
    MODES
        .iter()
        .find(|(known, _)| *known == mode)
        .map_or("?", |(_, label)| label)
}
