//! A project (wireframes 2, 3, 4): its tasks by status, the conversation
//! with its lead, the mode of each agent bound in it, and its memory; on
//! its header "Pause project" and "New task", which skips the lead.

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::{ScrollableElement as _, ScrollbarAxis};
use gpui_kit::component::separator::Separator;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab as KitTab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::model::Mode;
use threadlane_desktop::{
    Consequence, EmptyRegion, Line, ModeChoice, Part, When, message_limit, newest_message,
};

use super::{delete_project::delete_project_item, empty, gap_line, link, menu_item};
use crate::components::{
    button, card, count, count_badge, decision_scroller, field, icon_button, meta, page_header,
    section, status_badge,
};
use crate::icons::AppIcon;
use crate::tokens::{
    BOARD_COLUMN_MIN, FIELD_RADIUS, LG, MD, MEMORY_LIST_WIDTH, MODE_WIDTH, READING_WIDTH,
    ROW_RADIUS, RULES_WIDTH, SHORT_LABEL_WIDTH, SIDEBAR_WIDTH, SM, Status, TIMESTAMP_WIDTH,
    TYPE_BODY, TYPE_META, XS,
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
    pub(crate) fn space_tasks(&self, space: &str, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex()
            .flex_1()
            .min_h_0()
            .gap(SM)
            .overflow_y_scrollbar()
            .id("space-tasks")
            .child(page_header("Tasks", Some(meta(space.to_owned(), cx)), None));
        if let Some(known) = self.state.space(space) {
            if let Some(gap) = known.gap(Part::Tasks) {
                list = list.child(gap_line(&gap, "tasks", space, cx));
            } else if known.tasks.is_empty() {
                list = list.child(meta("No tasks in this space yet.", cx));
            }
            let projects: Vec<_> = known
                .active_projects()
                .filter(|p| !p.is_general)
                .map(|p| p.name.clone())
                .collect();
            let in_space = space.to_owned();
            if !projects.is_empty() {
                let view = cx.entity().downgrade();
                list = list.child(
                    button("space-new-task")
                        .primary()
                        .self_start()
                        .label("New task")
                        .dropdown_menu(move |mut menu, _, _| {
                            for project in &projects {
                                let (space, project, view) =
                                    (in_space.clone(), project.clone(), view.clone());
                                menu = menu.item(PopupMenuItem::new(project.clone()).on_click(
                                    move |_, _, cx| {
                                        let _ = view.update(cx, |this, cx| {
                                            this.state.open_task_form(&space, &project, true);
                                            this.go(
                                                Screen::Project {
                                                    space: space.clone(),
                                                    project: project.clone(),
                                                    tab: Tab::Tasks,
                                                },
                                                cx,
                                            );
                                        });
                                    },
                                ));
                            }
                            menu
                        }),
                );
            } else if known.tasks.is_empty() && known.gap(Part::Projects).is_none() {
                list = list.child(
                    h_flex()
                        .gap(SM)
                        .child(
                            button("space-create-project")
                                .primary()
                                .label("Create a project")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.go(
                                        Screen::NewProject {
                                            space: in_space.clone(),
                                        },
                                        cx,
                                    )
                                })),
                        )
                        .children(self.main_chat_screen(space).map(|screen| {
                            button("space-open-chat")
                                .outline()
                                .label("Open Main Chat")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                                )
                        })),
                );
            }
            for summary in &known.tasks {
                let task = &summary.task;
                let screen = Screen::Task {
                    space: space.to_owned(),
                    task: task.id,
                };
                let status =
                    threadlane_desktop::status_label(task.status, task.waiting_reason.as_deref());
                list = list.child(
                    h_flex()
                        .gap(MD)
                        .child(
                            link(
                                format!("space-task-{}", task.id),
                                format!("{} {}", task.id, task.title),
                            )
                            .flex_1()
                            .min_w_0()
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                            ),
                        )
                        .child(meta(summary.project.clone(), cx))
                        .child(status_badge(status, Status::task(status), cx)),
                );
            }
        }
        list.into_any_element()
    }

    pub(crate) fn project(
        &self,
        space: &str,
        project: &str,
        tab: Tab,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if tab == Tab::Conversation && self.is_main_chat(space, project) {
            let context = h_flex().gap(SM).child(meta(space.to_owned(), cx)).children(
                self.state
                    .lead_line(space, project, when)
                    .map(|line| meta(line, cx)),
            );
            let actions = self.state.lead_stop(space, project).map(|target| {
                h_flex().child(self.stop_button(space, target, "Stop the space Lead".into(), cx))
            });
            return v_flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .w_full()
                .max_w(READING_WIDTH)
                .self_center()
                .gap(MD)
                .child(page_header("Main Chat", Some(context), actions))
                .child(self.conversation_tab(space, project, when, cx))
                .into_any_element();
        }
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
                let label = if tab == Tab::Conversation {
                    "History"
                } else {
                    tab.label()
                };
                let icon = match tab {
                    Tab::Tasks => AppIcon::Columns3,
                    Tab::Conversation => AppIcon::MessagesSquare,
                    Tab::Agents => AppIcon::Bot,
                    Tab::Routines => AppIcon::CalendarClock,
                    Tab::Memory => AppIcon::BookOpen,
                };
                KitTab::new().aria_label(label).child(
                    h_flex()
                        .gap(XS)
                        .flex_none()
                        .child(Icon::new(icon).size(TYPE_BODY))
                        .child(label),
                )
            }));
        let archived = self.state.is_archived(space, project);
        let paused = self.state.is_paused(space, project);
        let lead_gap = self.state.project_gap(space, project, Part::Lead);
        let lead = lead_gap
            .is_none()
            .then(|| self.state.lead_line(space, project, when))
            .flatten();
        let lead_error = lead_gap
            .filter(|gap| matches!(gap, threadlane_desktop::Gap::Failed(_)))
            .map(|gap| gap_line(&gap, "lead status", space, cx));
        let stop = self.state.lead_stop(space, project).map(|target| {
            self.stop_button(space, target, format!("Stop the lead of {project}"), cx)
        });
        let ceiling = self.state.project_ceiling(space, project);
        let lead_mode = self.state.lead_mode(space, project);
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
        let mut actions = h_flex()
            .gap(SM)
            .flex_wrap()
            .children(self.main_chat_screen(space).map(|screen| {
                button("project-main-chat")
                    .small()
                    .ghost()
                    .icon(AppIcon::MessagesSquare)
                    .label("Main Chat")
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
            }));
        if archived {
            actions = actions.child(
                button("unarchive-project")
                    .small()
                    .outline()
                    .label("Unarchive")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_archived(pause_space.clone(), pause_project.clone(), false, cx);
                    })),
            );
        } else {
            actions = actions
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
                            this.set_paused(
                                pause_space.clone(),
                                pause_project.clone(),
                                !paused,
                                cx,
                            );
                        })),
                )
                .children(self.state.offers_archive(space, project).then(|| {
                    let action = Consequence::ArchiveProject(space.to_owned(), project.to_owned());
                    button("archive-project")
                        .small()
                        .ghost()
                        .label("Archive project…")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_action(action.clone(), window, cx);
                        }))
                }))
                .child(
                    button("new-task")
                        .small()
                        .primary()
                        .icon(AppIcon::ListPlus)
                        .label("New task")
                        .accessibility_label("Create a task directly")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state
                                .open_task_form(&task_space, &task_project, !form_open);
                            cx.notify();
                        })),
                );
        }
        if self.state.offers_delete(space, project) {
            let (space, project) = (space.to_owned(), project.to_owned());
            if archived {
                actions = actions.child(
                    button("delete-archived-project")
                        .small()
                        .danger()
                        .icon(AppIcon::Trash)
                        .label("Delete project…")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_project_deletion(
                                space.clone(),
                                project.clone(),
                                window,
                                cx,
                            );
                        })),
                );
            } else {
                let view = cx.entity().downgrade();
                actions = actions.child(
                    button("project-overflow")
                        .small()
                        .ghost()
                        .label("More")
                        .accessibility_label("More project actions")
                        .dropdown_menu(move |menu, _, cx| {
                            menu.item(delete_project_item(
                                &view,
                                space.clone(),
                                project.clone(),
                                cx,
                            ))
                        }),
                );
            }
        }
        let form = (!archived && form_open).then(|| self.task_form(space, project, cx));
        let context = h_flex()
            .gap(SM)
            .flex_wrap()
            .child(meta(space.to_owned(), cx))
            .child(meta(format!("Ceiling · {}", mode_name(ceiling)), cx))
            .children(paused.then(|| status_badge("Paused", Status::Waiting, cx)))
            .children(archived.then(|| status_badge("Archived", Status::Idle, cx)))
            .children(lead_mode.map(|mode| meta(format!("Lead mode · {}", mode_name(mode)), cx)))
            .children(lead.map(|line| meta(line, cx)))
            .children(stop.filter(|_| !archived));
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .gap(MD)
            .child(page_header(
                project.to_owned(),
                Some(context),
                Some(actions),
            ))
            .child(bar)
            .children(lead_error)
            .children(form)
            .child(
                v_flex()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .when(tab == Tab::Conversation, |body| {
                        body.max_w(READING_WIDTH).self_center()
                    })
                    .child(body),
            )
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
        let archived = self.state.is_archived(space, project);
        let board_columns = self.state.board(space, project, when);
        let is_empty = self
            .state
            .project_gap(space, project, Part::Tasks)
            .is_none()
            && board_columns.iter().all(|column| column.count == 0);
        let column_count = board_columns.len();
        let board_min = BOARD_COLUMN_MIN * column_count + SM * column_count.saturating_sub(1);
        let columns = board_columns.into_iter().map(|column| {
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
                            .children(button("Start", proposal.start).filter(|_| !archived))
                            .children(
                                button("Decline", proposal.decline)
                                    .filter(|_| !archived)
                                    .map(Button::ghost),
                            ),
                    )
            });
            let cards = column.cards.into_iter().map(|task_card| {
                let screen = Screen::Task {
                    space: space.to_owned(),
                    task: task_card.id,
                };
                let answer = task_card.decision.filter(|_| !archived).map(|decision| {
                    let (space, id, view) = (space.to_owned(), decision.to_string(), view.clone());
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
                let context_screen = screen.clone();
                let id = task_card.id;
                let menu_space = space.to_owned();
                let menu_view = view.clone();
                div()
                    .id(SharedString::from(format!("board-menu-{id}")))
                    .child(
                        card(cx)
                            .p(SM)
                            .child(
                                button(SharedString::from(format!("card-{}", task_card.id)))
                                    .text()
                                    .w_full()
                                    .accessibility_label(format!(
                                        "Open {} {}",
                                        task_card.id, task_card.title
                                    ))
                                    .child(crate::components::work_label(
                                        format!("{} {}", task_card.id, task_card.title),
                                        task_card.elapsed,
                                        status,
                                        cx,
                                    ))
                                    .min_w_0()
                                    .tab_stop(true)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.go(screen.clone(), cx)
                                    })),
                            )
                            .child(meta(task_card.meta, cx).text_right())
                            .children(task_card.merge.map(|merge| meta(merge, cx).text_right()))
                            .children(answer),
                    )
                    .context_menu(move |menu, _, _| {
                        let open = context_screen.clone();
                        let mut menu = menu
                            .item(menu_item(&menu_view, "Open", move |this, _, cx| {
                                this.go(open.clone(), cx)
                            }))
                            .item(menu_item(&menu_view, "Copy id", move |this, _, cx| {
                                this.copy(id.to_string(), cx)
                            }));
                        if task_card.can_cancel && !archived {
                            let space = menu_space.clone();
                            menu = menu.item(menu_item(
                                &menu_view,
                                "Cancel…",
                                move |this, window, cx| {
                                    this.confirm_action(
                                        Consequence::CancelTask(space.clone(), id),
                                        window,
                                        cx,
                                    );
                                },
                            ));
                        }
                        menu
                    })
            });
            v_flex()
                .min_w_0()
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
                .child(gap_line(&gap, "tasks", space, cx))
                .into_any_element(),
            None => div()
                .relative()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(
                    h_flex()
                        .id("board")
                        .size_full()
                        .items_start()
                        .overflow_scroll()
                        .track_scroll(&self.board_scroll)
                        .child(
                            div()
                                .grid()
                                .grid_cols(column_count as u16)
                                .w_full()
                                .min_w(board_min)
                                .flex_none()
                                .items_start()
                                .gap(SM)
                                .children(columns),
                        ),
                )
                .scrollbar(&self.board_scroll, ScrollbarAxis::Both)
                .into_any_element(),
        };

        let view = self.state.project(space, project);
        let mut side = v_flex().gap(SM);
        let decisions: Vec<_> = self
            .state
            .visible_decisions()
            .filter(|_| !archived)
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
            side = side.child(gap_line(gap, "repositories", space, cx));
        } else if repositories.is_empty() {
            side = side.child(empty(EmptyRegion::Repositories, cx));
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
        side = side.children((!archived).then(|| self.repository_form(space, project, cx)));
        side = side.child(section("Agents here"));
        if let Some(gap) = self.state.project_gap(space, project, Part::Bindings) {
            side = side.child(gap_line(&gap, "agents", space, cx));
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
            side = side.child(gap_line(&gap, "machines", space, cx));
        }
        for machine in self
            .state
            .space(space)
            .map(|known| known.machines.as_slice())
            .unwrap_or_default()
        {
            let hub = if machine.is_hub { " · hub" } else { "" };
            let screen = Screen::Machine {
                space: space.to_owned(),
                machine: machine.id,
            };
            side = side.child(
                h_flex()
                    .gap(XS)
                    .text_size(TYPE_BODY)
                    .child(
                        link(
                            format!("board-machine-{}", machine.id),
                            format!("{}{hub}", machine.name),
                        )
                        .flex_1()
                        .min_w_0()
                        .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
                    )
                    .child(
                        status_badge(machine.status.to_string(), machine.status.into(), cx)
                            .flex_none(),
                    ),
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
                    .children(is_empty.then(|| empty(EmptyRegion::Board, cx)))
                    .child(board),
            )
            .child(
                decision_scroller("board-sidebar", side, &self.decision_scroll)
                    .w(SIDEBAR_WIDTH)
                    .h_full()
                    .flex_none(),
            )
            .into_any_element()
    }

    pub(crate) fn is_main_chat(&self, space: &str, project: &str) -> bool {
        self.state
            .space(space)
            .and_then(threadlane_desktop::Space::main_chat_project)
            == Some(project)
    }

    pub(crate) fn main_chat_screen(&self, space: &str) -> Option<Screen> {
        let project = self.state.space(space)?.main_chat_project()?;
        Some(Screen::Project {
            space: space.to_owned(),
            project: project.to_owned(),
            tab: Tab::Conversation,
        })
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
            lines = lines.child(gap_line(&gap, "conversation", space, cx));
        }
        let main_chat = self.is_main_chat(space, project);
        let conversation = self.state.conversation(space, project, when);
        if conversation.is_empty()
            && self
                .state
                .project_gap(space, project, Part::Conversation)
                .is_none()
        {
            if main_chat {
                lines = lines.child(empty(EmptyRegion::Conversation, cx));
            } else {
                lines = lines
                    .child(meta(format!("No messages for {project} yet."), cx))
                    .children(self.main_chat_screen(space).map(|screen| {
                        button("history-main-chat")
                            .self_start()
                            .outline()
                            .label(format!("Open {space} Main Chat"))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                            )
                    }));
            }
        }
        let recorded = self.state.recorded(space, project, when);
        let newest = newest_message(&conversation);
        for (index, line) in conversation.iter().enumerate() {
            let limit = message_limit(newest, index);
            let said = |id: String, who: &str, time: &str, text: &str, reply: bool| {
                let reply = reply && main_chat;
                let id = format!("conversation-{space}-{project}-{id}");
                let quote = text.to_owned();
                let menu_text = quote.clone();
                let menu_view = cx.entity().downgrade();
                v_flex()
                    .id(SharedString::from(format!("menu-{id}")))
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
                    .context_menu(move |menu, _, _| {
                        let text = menu_text.clone();
                        let mut menu =
                            menu.item(menu_item(&menu_view, "Copy text", move |this, _, cx| {
                                this.copy(text.clone(), cx)
                            }));
                        if reply {
                            let quote = menu_text.clone();
                            menu = menu.item(menu_item(
                                &menu_view,
                                "Reply",
                                move |this, window, cx| this.reply_to(&quote, window, cx),
                            ));
                        }
                        menu
                    })
            };
            lines = match line {
                Line::Day(day) => lines.child(Separator::horizontal().label(day.clone())),
                Line::New => lines.child(Separator::horizontal().label("New")),
                Line::You {
                    id,
                    text,
                    time,
                    images,
                } => lines.child(
                    said(format!("you-{id}"), "You", time, text, !text.is_empty())
                        .children(crate::components::chat_images::previews(images, cx)),
                ),
                Line::SpaceLead { id, text, time } => lines.child(said(
                    format!("space-lead-{id}"),
                    "General lead",
                    time,
                    text,
                    false,
                )),
                Line::ProjectReply {
                    id,
                    project: from,
                    text,
                    time,
                } => {
                    let screen = Screen::Project {
                        space: space.to_owned(),
                        project: from.clone(),
                        tab: Tab::Conversation,
                    };
                    lines.child(
                        v_flex()
                            .gap(XS)
                            .child(said(
                                format!("reply-{id}"),
                                &format!("{from} lead"),
                                time,
                                text,
                                false,
                            ))
                            .child(
                                h_flex().child(
                                    button(SharedString::from(format!("open-reply-{id}")))
                                        .small()
                                        .ghost()
                                        .tab_stop(true)
                                        .label(format!("Open {from}"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.go(screen.clone(), cx)
                                        })),
                                ),
                            ),
                    )
                }
                Line::Lead { seq, text, time } => {
                    lines.child(said(format!("lead-{seq}"), "Lead", time, text, true))
                }
                Line::Notice { seq, text, time } => {
                    lines.child(said(format!("notice-{seq}"), "Notice", time, text, false))
                }
                Line::Sending {
                    id,
                    text,
                    time,
                    images,
                } => lines.child(
                    said(format!("sending-{id}"), "You · Sending…", time, text, false)
                        .children(crate::components::chat_images::previews(images, cx)),
                ),
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

        let side = (!main_chat).then(|| self.conversation_sidebar(space, project, when, cx));
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
                    .children(main_chat.then(|| {
                        self.render_composer(
                            threadlane_desktop::Draft::Lead,
                            "Message the space Lead",
                            None,
                            cx,
                        )
                        .flex_none()
                    })),
            )
            .children(side)
            .into_any_element()
    }

    fn conversation_sidebar(
        &self,
        space: &str,
        project: &str,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
            side = side.child(gap_line(&gap, "tasks", space, cx));
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
            side = side.child(empty(EmptyRegion::Board, cx));
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
                    h_flex()
                        .gap(XS)
                        .justify_between()
                        .w_full()
                        .child(status_badge(status, Status::task(status), cx))
                        .children(
                            threadlane_desktop::work_elapsed(summary, when)
                                .map(|elapsed| meta(elapsed, cx).text_right()),
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
        let decision_gap = self
            .state
            .space(space)
            .and_then(|space| space.gap(Part::Decisions));
        if let Some(gap) = decision_gap {
            side = side.child(gap_line(&gap, "decisions", space, cx));
        } else if open.is_empty() {
            side = side.child(empty(EmptyRegion::Decisions, cx));
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
                side = side.child(gap_line(&gap, "memory", space, cx));
            }
        }
        let memory = Screen::Project {
            space: space.to_owned(),
            project: project.to_owned(),
            tab: Tab::Memory,
        };
        side.child(
            button("open-memory")
                .small()
                .icon(AppIcon::BookOpen)
                .label("Open memory")
                .on_click(cx.listener(move |this, _, _, cx| this.go(memory.clone(), cx))),
        )
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
            list = list.child(gap_line(&gap, "memory", space, cx));
        }
        let groups = self.state.memory_groups(space, project);
        if groups.is_empty() && self.state.memory_gap(space, project).is_none() {
            list = list.child(empty(EmptyRegion::Memory, cx));
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
                let archived = self.state.is_archived(space, project);
                let can_edit = self.state.memory_can_edit(space, project);
                let editing = !archived && self.state.memory_editing(space, project);
                detail = detail.child(section(path.to_owned()));
                let mut actions = h_flex().gap(SM);
                if can_edit && !archived {
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
                        let delete = Consequence::DeleteMemory(
                            in_space.clone(),
                            named.clone(),
                            path.to_owned(),
                        );
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
                                    .label("Delete…")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.confirm_action(delete.clone(), window, cx);
                                    })),
                            );
                    }
                } else if file.is_some() && !can_edit {
                    actions = actions.child(
                        div()
                            .text_size(TYPE_BODY)
                            .text_color(muted)
                            .child("Written by the hub from the files next to it."),
                    );
                }
                detail = detail.child(actions);
                if let Some(why) = self.state.memory_refused(space, project) {
                    detail = detail.child(crate::components::empty_state(
                        format!("Could not change {path}."),
                        Some(why),
                        cx,
                    ));
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
                            if can_edit {
                                "Not written yet. Instructions are loaded into every run."
                            } else {
                                "This file is deleted."
                            },
                        ))
                    }
                };
                detail = detail.child(section("Versions"));
                let versions = self.state.memory_versions(space, project, when);
                if let Some(gap) = self.state.memory_versions_gap(space, project) {
                    detail = detail.child(gap_line(&gap, "memory versions", space, cx));
                } else if versions.is_empty() {
                    detail = detail.child(empty(EmptyRegion::MemoryVersions, cx));
                }
                detail = detail.children(versions.into_iter().map(|line| meta(line, cx)));
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
        let disabled = card.disabled || self.state.is_archived(space, project);
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
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.toggle_dreaming(in_space.clone(), named.clone(), *checked, cx);
                    })),
            )
            .child(meta("Reviews this project’s tasks and memory. Product limitations can be reported to wheregmis/threadlane on GitHub under your publication permissions.", cx));
        panel = if let Some(gap) = &card.cadence_gap {
            panel.child(gap_line(gap, "dreaming settings", space, cx))
        } else {
            panel.child(
                div()
                    .text_size(TYPE_BODY)
                    .text_color(muted)
                    .child(card.cadence),
            )
        };
        if card.enabled {
            let (in_space, named) = (space.to_owned(), project.to_owned());
            panel = panel.child(
                h_flex()
                    .gap(SM)
                    .child(
                        div()
                            .flex_1()
                            .child(field(&self.dream_cadence).disabled(disabled)),
                    )
                    .child(
                        button("save-dream-cadence")
                            .icon(AppIcon::Save)
                            .label("Save cadence")
                            .disabled(disabled)
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
                card.findings_gap
                    .as_ref()
                    .map(|gap| gap_line(gap, "memory checks", space, cx)),
            )
            .children(
                card.findings
                    .into_iter()
                    .filter(|_| card.findings_gap.is_none())
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
        let archived = self.state.is_archived(space, project);
        let (at_space, at_project, choices) =
            (space.to_owned(), project.to_owned(), card.choices.clone());
        let ceiling = mode_control(format!("ceiling-{project}"), &card.choices).on_click(
            cx.listener(move |this, clicked: &Vec<usize>, window, cx| {
                if let Some(mode) = unlocked(&choices, clicked) {
                    this.confirm_action(
                        Consequence::SetCeiling(at_space.clone(), Some(at_project.clone()), mode),
                        window,
                        cx,
                    );
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
                    .child(ceiling.disabled(archived)),
            )
            .children(locked_note(&card.choices, muted))
            .child(section("Agents in this project"));
        let gap = self.state.project_gap(space, project, Part::Bindings);
        let agents = self.state.agent_rows(space, project);
        if let Some(gap) = &gap {
            rows = rows.child(gap_line(gap, "agents", space, cx));
        } else if agents.is_empty() {
            rows = rows.child(empty(EmptyRegion::ProjectAgents, cx));
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
                    .child(control.disabled(archived))
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
        let archived = self.state.is_archived(space, project);
        let view = cx.entity().downgrade();
        let mut statements = v_flex()
            .gap(XS)
            .flex_1()
            .min_w_0()
            .child(section(may.title.clone()));
        for (index, statement) in may.statements.into_iter().enumerate() {
            let cell: AnyElement = match statement.tier {
                Some(tier) if !archived && !statement.choices.is_empty() => {
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
            Err(gap) => rules = rules.child(gap_line(&gap, "standing rules", space, cx)),
            Ok(listed) if listed.is_empty() => {
                rules = rules.child(empty(EmptyRegion::Rules, cx));
            }
            Ok(listed) => {
                for rule in listed {
                    let (space_revoke, id) = (space.to_owned(), rule.id);
                    let actions = button(SharedString::from(format!("revoke-{}", rule.id)))
                        .ghost()
                        .small()
                        .icon(AppIcon::ShieldOff)
                        .label("Revoke…")
                        .accessibility_label(format!("Revoke {}", rule.allows))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_action(
                                Consequence::RevokeRule(space_revoke.clone(), id),
                                window,
                                cx,
                            );
                        }));
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
                                    .children((!archived).then_some(actions)),
                            )
                            .child(
                                div()
                                    .text_size(TYPE_META)
                                    .text_color(muted)
                                    .child(rule.from.clone()),
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
    /// The "New task" form (wireframe 2, note 3): title, goal, the agent
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
            .child(
                h_flex()
                    .gap(SM)
                    .child(label("Assigned agent"))
                    .child(agent_row),
            )
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
                    .child("Creates a task directly and assigns it to the selected agent."),
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
