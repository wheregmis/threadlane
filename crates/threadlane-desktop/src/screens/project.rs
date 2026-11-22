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
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::model::Mode;
use threadlane_desktop::{
    Consequence, EarlierStatus, EmptyRegion, Line, ModeChoice, Part, ProjectTasksViewMode,
    SpaceTasksViewMode, TaskGroup, When, build_task_dag, message_limit, newest_message,
    status_label, work_elapsed,
};

use super::{delete_project::delete_project_item, empty, gap_line, link, menu_item};
use crate::components::{
    button, card, count, count_badge, decision_scroller, field, meta, page_header, section,
    status_badge, status_dot,
};
use crate::icons::AppIcon;
use crate::tokens::{
    BADGE_RADIUS, BOARD_COLUMN_MIN, CONTROL_HEIGHT, FIELD_RADIUS, LG, MD, MEMORY_LIST_WIDTH,
    READING_WIDTH, ROW_RADIUS, RULES_WIDTH, SCROLLBAR_GUTTER, SHORT_LABEL_WIDTH,
    SIDEBAR_SECTION_MAX_HEIGHT, SIDEBAR_WIDTH, SM, Status, TABULAR, TIMESTAMP_WIDTH, TYPE_BODY,
    TYPE_META, XL, XS,
};
use crate::{Desktop, Screen, Tab};

/// The four positions of the mode control (Plan.md §2.3).
pub(super) const MODES: [(Mode, &str); 4] = [
    (Mode::Off, "Off"),
    (Mode::OnRequest, "On request"),
    (Mode::Assisted, "Assisted"),
    (Mode::Autonomous, "Autonomous"),
];

/// Keep at least 24 rem for the chat when both rails fit.
fn chat_details_in_sheet(viewport_width: Pixels, rem: Pixels, sidebar_collapsed: bool) -> bool {
    let left = if sidebar_collapsed {
        Pixels::ZERO
    } else {
        SIDEBAR_WIDTH
    };
    let available = (viewport_width - left - LG * 2.).min(READING_WIDTH.to_pixels(rem));
    available - SIDEBAR_WIDTH - LG < rem * 24.
}

impl Desktop {
    pub(crate) fn space_tasks(
        &self,
        space: &str,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let known = self.state.space(space);
        let active_projects: Vec<String> = known
            .map(|known| {
                known
                    .active_projects()
                    .filter(|p| !p.is_general)
                    .map(|p| p.name.clone())
                    .collect()
            })
            .unwrap_or_default();

        let view_mode = self.state.space_tasks_view_mode();
        let mode_toggle = ButtonGroup::new("space-tasks-view-mode")
            .child(
                button("space-tasks-mode-list")
                    .small()
                    .outline()
                    .icon(AppIcon::FileText)
                    .label("List")
                    .selected(view_mode == SpaceTasksViewMode::List)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state
                            .set_space_tasks_view_mode(SpaceTasksViewMode::List);
                        cx.notify();
                    })),
            )
            .child(
                button("space-tasks-mode-graph")
                    .small()
                    .outline()
                    .icon(AppIcon::GitFork)
                    .label("Graph")
                    .selected(view_mode == SpaceTasksViewMode::Graph)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state
                            .set_space_tasks_view_mode(SpaceTasksViewMode::Graph);
                        cx.notify();
                    })),
            );

        let new_task_button = (!active_projects.is_empty()).then(|| {
            let in_space = space.to_owned();
            let projects = active_projects.clone();
            let view = cx.entity().downgrade();
            button("space-new-task")
                .small()
                .outline()
                .icon(AppIcon::ListPlus)
                .label("New task…")
                .dropdown_menu(move |mut menu, _, _| {
                    for project in &projects {
                        let (space, project, view) =
                            (in_space.clone(), project.clone(), view.clone());
                        menu = menu.item(PopupMenuItem::new(project.clone()).on_click(
                            move |_, window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.go(
                                        Screen::Project {
                                            space: space.clone(),
                                            project: project.clone(),
                                            tab: Tab::Tasks,
                                        },
                                        cx,
                                    );
                                    this.show_new_task_form(&space, &project, window, cx);
                                });
                            },
                        ));
                    }
                    menu
                })
        });

        let header_actions = Some(
            h_flex()
                .gap(SM)
                .items_center()
                .child(mode_toggle)
                .children(new_task_button),
        );

        let mut list = v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .h_full()
            .gap(MD)
            .id("space-tasks")
            .child(page_header(
                "Tasks",
                Some(meta(space.to_owned(), cx)),
                header_actions,
            ));

        if let Some(known) = known {
            if let Some(gap) = known.gap(Part::Tasks) {
                list = list.child(gap_line(&gap, "tasks", space, cx));
            } else if known.tasks.is_empty() {
                list = list.child(meta("No tasks in this space yet.", cx));
                if active_projects.is_empty() && known.gap(Part::Projects).is_none() {
                    let in_space = space.to_owned();
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
                                    .label("Open space chat")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.go(screen.clone(), cx)
                                    }))
                            })),
                    );
                }
            }

            if !known.tasks.is_empty() {
                let filter = self.state.space_task_filter(space);
                let view = self.state.space_tasks_view(space, when);

                // Inline filter bar: status chips + Project & Agent dropdowns.
                let all_space = space.to_owned();
                let mut filters = h_flex().gap(XS).flex_wrap().items_center().child(
                    button("tasks-filter-all")
                        .small()
                        .outline()
                        .label(format!("All  {}", view.total))
                        .selected(filter.status.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.set_space_task_status_filter(&all_space, None);
                            cx.notify();
                        })),
                );
                for &(group, total) in &view.group_counts {
                    let in_space = space.to_owned();
                    filters = filters.child(
                        button(SharedString::from(format!(
                            "tasks-filter-status-{}",
                            group.label()
                        )))
                        .small()
                        .outline()
                        .label(format!("{}  {total}", group.label()))
                        .selected(filter.status == Some(group))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let next =
                                if this.state.space_task_filter(&in_space).status == Some(group) {
                                    None
                                } else {
                                    Some(group)
                                };
                            this.state.set_space_task_status_filter(&in_space, next);
                            cx.notify();
                        })),
                    );
                }

                if view.projects.len() > 1 {
                    let projects = view.projects.clone();
                    let in_space = space.to_owned();
                    let entity = cx.entity().downgrade();
                    let label = filter
                        .project
                        .as_deref()
                        .map_or_else(|| "Project: All".to_owned(), |p| format!("Project: {p}"));
                    filters = filters.child(
                        button("tasks-filter-project")
                            .small()
                            .outline()
                            .selected(filter.project.is_some())
                            .icon(AppIcon::ChevronDown)
                            .label(label)
                            .dropdown_menu(move |mut menu, _, _| {
                                let (space_all, entity_all) = (in_space.clone(), entity.clone());
                                menu = menu.item(PopupMenuItem::new("All projects").on_click(
                                    move |_, _, cx| {
                                        let _ = entity_all.update(cx, |this, cx| {
                                            this.state
                                                .set_space_task_project_filter(&space_all, None);
                                            cx.notify();
                                        });
                                    },
                                ));
                                for project in &projects {
                                    let (space_p, project_p, entity_p) =
                                        (in_space.clone(), project.clone(), entity.clone());
                                    menu = menu.item(PopupMenuItem::new(project.clone()).on_click(
                                        move |_, _, cx| {
                                            let _ = entity_p.update(cx, |this, cx| {
                                                this.state.set_space_task_project_filter(
                                                    &space_p,
                                                    Some(project_p.clone()),
                                                );
                                                cx.notify();
                                            });
                                        },
                                    ));
                                }
                                menu
                            }),
                    );
                }

                if view.agents.len() > 1 {
                    let agents = view.agents.clone();
                    let in_space = space.to_owned();
                    let entity = cx.entity().downgrade();
                    let label = filter
                        .agent
                        .as_deref()
                        .map_or_else(|| "Agent: All".to_owned(), |a| format!("Agent: {a}"));
                    filters = filters.child(
                        button("tasks-filter-agent")
                            .small()
                            .outline()
                            .selected(filter.agent.is_some())
                            .icon(AppIcon::ChevronDown)
                            .label(label)
                            .dropdown_menu(move |mut menu, _, _| {
                                let (space_all, entity_all) = (in_space.clone(), entity.clone());
                                menu = menu.item(PopupMenuItem::new("All agents").on_click(
                                    move |_, _, cx| {
                                        let _ = entity_all.update(cx, |this, cx| {
                                            this.state
                                                .set_space_task_agent_filter(&space_all, None);
                                            cx.notify();
                                        });
                                    },
                                ));
                                for agent in &agents {
                                    let (space_a, agent_a, entity_a) =
                                        (in_space.clone(), agent.clone(), entity.clone());
                                    menu = menu.item(PopupMenuItem::new(agent.clone()).on_click(
                                        move |_, _, cx| {
                                            let _ = entity_a.update(cx, |this, cx| {
                                                this.state.set_space_task_agent_filter(
                                                    &space_a,
                                                    Some(agent_a.clone()),
                                                );
                                                cx.notify();
                                            });
                                        },
                                    ));
                                }
                                menu
                            }),
                    );
                }

                if filter.is_active() {
                    let clear_space = space.to_owned();
                    filters = filters.child(
                        button("tasks-filter-clear")
                            .small()
                            .ghost()
                            .icon(AppIcon::X)
                            .label("Clear filters")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.clear_space_task_filter(&clear_space);
                                cx.notify();
                            })),
                    );
                }

                list = list.child(filters);

                if view_mode == SpaceTasksViewMode::Graph {
                    let filtered_tasks: Vec<_> = known
                        .tasks
                        .iter()
                        .filter(|summary| {
                            filter
                                .project
                                .as_deref()
                                .is_none_or(|p| summary.project == p)
                                && filter.agent.as_deref().is_none_or(|a| summary.agent == a)
                                && filter
                                    .status
                                    .is_none_or(|g| TaskGroup::of(summary.task.status) == g)
                        })
                        .cloned()
                        .collect();
                    let dag = build_task_dag(&filtered_tasks, when);
                    list = list.child(self.task_dag_view("space-task-dag", space, dag, cx));
                    return list.into_any_element();
                }

                if view.sections.is_empty() {
                    let clear_space = space.to_owned();
                    list = list.child(
                        h_flex()
                            .gap(SM)
                            .items_center()
                            .child(meta("No tasks match these filters.", cx))
                            .child(
                                button("tasks-empty-clear")
                                    .small()
                                    .ghost()
                                    .label("Clear filters")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.clear_space_task_filter(&clear_space);
                                        cx.notify();
                                    })),
                            ),
                    );
                }

                let mono = cx.theme().mono_font_family.clone();
                for group_section in view.sections {
                    let mut block = v_flex().gap(XS);
                    if group_section.group == TaskGroup::Done
                        && filter.status != Some(TaskGroup::Done)
                    {
                        let toggle_space = space.to_owned();
                        block = block.child(
                            button("tasks-toggle-done")
                                .ghost()
                                .small()
                                .self_start()
                                .icon(if group_section.collapsed {
                                    AppIcon::ChevronRight
                                } else {
                                    AppIcon::ChevronDown
                                })
                                .child(
                                    h_flex()
                                        .gap(SM)
                                        .items_center()
                                        .child(group_section.group.label())
                                        .children(count(group_section.total, cx)),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.toggle_done_tasks_expanded(&toggle_space);
                                    cx.notify();
                                })),
                        );
                    } else {
                        block = block.child(
                            section(group_section.group.label())
                                .children(count(group_section.total, cx)),
                        );
                    }

                    for row in group_section.rows {
                        let screen = Screen::Task {
                            space: space.to_owned(),
                            task: row.id,
                        };
                        let status_kind = Status::task(row.status);
                        let title_color = if row.quiet {
                            cx.theme().muted_foreground
                        } else {
                            cx.theme().foreground
                        };
                        let row_el = button(SharedString::from(format!("space-task-{}", row.id)))
                            .ghost()
                            .w_full()
                            .min_h(CONTROL_HEIGHT)
                            .rounded(ROW_RADIUS)
                            .px(SM)
                            .accessibility_label(format!(
                                "{} {}, {}, {}",
                                row.id, row.title, row.project, row.status
                            ))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap(SM)
                                    .items_center()
                                    .child(
                                        h_flex()
                                            .w(TYPE_META)
                                            .flex_none()
                                            .justify_center()
                                            .children(
                                                (!row.quiet).then(|| status_dot(status_kind, cx)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w(rems(4.5))
                                            .flex_none()
                                            .text_size(TYPE_META)
                                            .font_family(mono.clone())
                                            .font_features(TABULAR.clone())
                                            .text_color(cx.theme().muted_foreground)
                                            .child(row.id.to_string()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(TYPE_BODY)
                                            .text_color(title_color)
                                            .child(row.title),
                                    )
                                    .child(
                                        meta(row.project, cx).w(rems(7.5)).flex_none().truncate(),
                                    )
                                    .child(meta(row.agent, cx).w(rems(6.5)).flex_none().truncate())
                                    .child(
                                        meta(row.time, cx)
                                            .w(rems(5.5))
                                            .flex_none()
                                            .font_features(TABULAR.clone())
                                            .text_right(),
                                    )
                                    .child(
                                        h_flex()
                                            .w(rems(8.5))
                                            .flex_none()
                                            .justify_end()
                                            .children((!row.quiet).then(|| {
                                                status_badge(row.status, status_kind, cx)
                                            })),
                                    ),
                            );
                        block = block.child(row_el);
                    }
                    list = list.child(block);
                }
            }
        }
        list.overflow_y_scrollbar().into_any_element()
    }

    pub(crate) fn project(
        &mut self,
        space: &str,
        project: &str,
        tab: Tab,
        when: &When,
        window: &mut Window,
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
                .child(page_header("Chat", Some(context), actions))
                .child(self.conversation_tab(space, project, when, false, cx))
                .into_any_element();
        }
        let (in_space, named) = (space.to_owned(), project.to_owned());
        // Keyed by the tab shown, so the bar's sliding indicator starts fresh
        // under it instead of carrying a half-finished slide from another tab.
        let bar = TabBar::new(SharedString::from(format!("project-tabs-{}", tab.label())))
            .underline()
            .menu(true)
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
                let label = tab.label();
                let icon = match tab {
                    Tab::Tasks => AppIcon::Columns3,
                    Tab::Conversation => AppIcon::MessagesSquare,
                    Tab::Memory => AppIcon::BookOpen,
                    Tab::Team => AppIcon::Bot,
                    Tab::Routines => AppIcon::CalendarClock,
                    Tab::Setup => AppIcon::Settings,
                };
                KitTab::new()
                    .label(label)
                    .aria_label(label)
                    .icon(Icon::new(icon).small())
                    .tooltip(move |window, cx| Tooltip::new(tab.tooltip()).build(window, cx))
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
        let body = match tab {
            Tab::Tasks => self.tasks_tab(space, project, when, cx),
            Tab::Conversation => self.conversation_tab(
                space,
                project,
                when,
                chat_details_in_sheet(
                    window.viewport_size().width,
                    window.rem_size(),
                    self.sidebar_collapsed,
                ),
                cx,
            ),
            Tab::Memory => self.memory_tab(space, project, when, cx),
            Tab::Team => self.agents_tab(space, project, cx),
            Tab::Routines => self.routines_tab(space, project, when, cx),
            Tab::Setup => self.setup_tab(space, project, cx),
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
                    .label("Space chat")
                    .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
            }));
        if matches!(tab, Tab::Tasks | Tab::Setup) {
            actions = actions.child(self.work_link(
                "project-outputs",
                "Outputs",
                space,
                crate::WorkPage::Outputs {
                    project: project.into(),
                    search: String::new(),
                },
                cx,
            ));
        }
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
            if self.state.offers_delete(space, project) {
                let (space, project) = (space.to_owned(), project.to_owned());
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
            }
        } else {
            let offers_archive = self.state.offers_archive(space, project);
            let offers_delete = self.state.offers_delete(space, project);
            let view = cx.entity().downgrade();
            let (menu_space, menu_project) = (space.to_owned(), project.to_owned());
            actions = actions
                .child(
                    button("new-task")
                        .small()
                        .outline()
                        .icon(AppIcon::ListPlus)
                        .label("New task…")
                        .accessibility_label("Create a task directly")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if form_open {
                                this.hide_new_task_form(&task_space, &task_project, window, cx);
                            } else {
                                this.show_new_task_form(&task_space, &task_project, window, cx);
                            }
                        })),
                )
                .child(
                    button("project-overflow")
                        .small()
                        .ghost()
                        .label("More")
                        .dropdown_caret(true)
                        .accessibility_label("More project actions")
                        .dropdown_menu(move |mut menu, _, cx| {
                            let (ps, pp) = (pause_space.clone(), pause_project.clone());
                            menu = menu.item(menu_item(
                                &view,
                                if paused {
                                    "Resume project"
                                } else {
                                    "Pause project"
                                },
                                move |this, _, cx| {
                                    this.set_paused(ps.clone(), pp.clone(), !paused, cx);
                                },
                            ));
                            if offers_archive {
                                let action = Consequence::ArchiveProject(
                                    menu_space.clone(),
                                    menu_project.clone(),
                                );
                                menu = menu.item(menu_item(
                                    &view,
                                    "Archive project…",
                                    move |this, window, cx| {
                                        this.confirm_action(action.clone(), window, cx)
                                    },
                                ));
                            }
                            if offers_delete {
                                menu = menu.separator().item(delete_project_item(
                                    &view,
                                    menu_space.clone(),
                                    menu_project.clone(),
                                    cx,
                                ));
                            }
                            menu
                        }),
                );
        }
        let form = (!archived && form_open).then(|| self.task_form(space, project, cx));
        let context = h_flex()
            .gap(SM)
            .flex_wrap()
            .child(meta(space.to_owned(), cx))
            .child(meta(format!("Mode · {}", mode_name(ceiling)), cx))
            .children(paused.then(|| status_badge("Paused", Status::Waiting, cx)))
            .children(archived.then(|| status_badge("Archived", Status::Idle, cx)))
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
        let mut board_columns = self.state.board(space, project, when);
        // A dragged card can always be dropped on Cancelled, even before any task is there.
        if cx.has_active_drag()
            && !archived
            && !board_columns
                .iter()
                .any(|column| column.title == "Cancelled")
        {
            board_columns.push(threadlane_desktop::Column {
                title: "Cancelled",
                cards: Vec::new(),
                count: 0,
                more: None,
                proposals: Vec::new(),
            });
        }
        let is_empty = self
            .state
            .project_gap(space, project, Part::Tasks)
            .is_none()
            && board_columns.iter().all(|column| column.count == 0);
        let columns = board_columns.into_iter().map(|column| {
            let blank = column.cards.is_empty() && column.proposals.is_empty();
            let status = Status::task(column.title);
            let column_title = column.title;
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
                    .child(meta(proposal.meta, cx))
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
                let group = SharedString::from(format!("board-card-{id}"));
                let quick = (!archived).then(|| self.card_actions(space, &task_card, &group, cx));
                let detail = task_card
                    .meta
                    .strip_prefix(task_card.agent.as_str())
                    .map_or(task_card.meta.as_str(), |rest| {
                        rest.trim_start_matches(" · ")
                    })
                    .to_owned();
                let drag = BoardDrag {
                    space: space.to_owned(),
                    id,
                    title: format!("{id} {}", task_card.title),
                    from: column_title,
                };
                div()
                    .id(SharedString::from(format!("board-menu-{id}")))
                    .group(group)
                    .when(!archived, |this| {
                        this.on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                    })
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
                            .child(
                                h_flex()
                                    .gap(XS)
                                    .min_w_0()
                                    .flex_wrap()
                                    .child(
                                        Tag::secondary()
                                            .small()
                                            .rounded(BADGE_RADIUS)
                                            .text_size(TYPE_META)
                                            .child(task_card.agent.clone()),
                                    )
                                    .children(
                                        (!detail.is_empty()).then(|| meta(detail, cx).min_w_0()),
                                    ),
                            )
                            .children(task_card.step.clone().map(|step| {
                                meta(step, cx)
                                    .w_full()
                                    .truncate()
                                    .font_family(cx.theme().mono_font_family.clone())
                            }))
                            .children(task_card.merge.clone().map(|merge| meta(merge, cx)))
                            .children(answer)
                            .children(quick),
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
                        if task_card.can_retry && !archived {
                            let space = menu_space.clone();
                            menu = menu.item(menu_item(&menu_view, "Retry", move |this, _, cx| {
                                this.retry_task(space.clone(), id, cx);
                            }));
                        }
                        if let Some(url) = task_card.review.clone() {
                            menu =
                                menu.item(menu_item(&menu_view, "Open review", move |_, _, cx| {
                                    cx.open_url(&url)
                                }));
                        }
                        for agent in task_card.reassign.iter().filter(|_| !archived) {
                            let (space, agent) = (menu_space.clone(), agent.clone());
                            menu = menu.item(menu_item(
                                &menu_view,
                                format!("Reassign to {agent}"),
                                move |this, _, cx| {
                                    this.reassign_task(space.clone(), id, agent.clone(), cx);
                                },
                            ));
                        }
                        menu
                    })
            });
            let column = v_flex()
                .min_w_0()
                .gap(SM)
                .p(SM)
                .bg(cx.theme().muted)
                .rounded(ROW_RADIUS)
                .drag_over::<BoardDrag>(|style, _, _, cx| {
                    style
                        .bg(cx.theme().accent)
                        .border_1()
                        .border_color(cx.theme().ring)
                })
                .on_drop(cx.listener(move |this, drag: &BoardDrag, window, cx| {
                    this.drop_card(drag, column_title, window, cx);
                }))
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
                );
            (blank, column)
        });
        // Adjacent empty columns share one grid cell, so the board fits beside the side panel.
        let mut cells = Vec::new();
        let mut blanks = Vec::new();
        for (blank, column) in columns {
            if blank {
                blanks.push(column);
                continue;
            }
            if !blanks.is_empty() {
                cells.push(v_flex().min_w_0().gap(SM).children(blanks.drain(..)));
            }
            cells.push(column);
        }
        if !blanks.is_empty() {
            cells.push(v_flex().min_w_0().gap(SM).children(blanks));
        }
        let column_count = cells.len();
        let board_min = BOARD_COLUMN_MIN * column_count + SM * column_count.saturating_sub(1);
        let view_mode = self.state.project_tasks_view();
        let mode_switcher = h_flex().justify_between().items_center().gap(SM).child(
            ButtonGroup::new("project-tasks-view-mode")
                .child(
                    button("project-tasks-mode-board")
                        .small()
                        .outline()
                        .icon(AppIcon::Columns3)
                        .label("Board")
                        .selected(view_mode == ProjectTasksViewMode::Board)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state
                                .set_project_tasks_view(ProjectTasksViewMode::Board);
                            cx.notify();
                        })),
                )
                .child(
                    button("project-tasks-mode-list")
                        .small()
                        .outline()
                        .icon(AppIcon::FileText)
                        .label("List")
                        .selected(view_mode == ProjectTasksViewMode::List)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state
                                .set_project_tasks_view(ProjectTasksViewMode::List);
                            cx.notify();
                        })),
                )
                .child(
                    button("project-tasks-mode-graph")
                        .small()
                        .outline()
                        .icon(AppIcon::GitFork)
                        .label("Graph")
                        .selected(view_mode == ProjectTasksViewMode::Graph)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state
                                .set_project_tasks_view(ProjectTasksViewMode::Graph);
                            cx.notify();
                        })),
                ),
        );

        let project_tasks = self
            .state
            .project(space, project)
            .map(|view| view.tasks.as_slice())
            .unwrap_or_default();

        let board = match self.state.project_gap(space, project, Part::Tasks) {
            Some(gap) => div()
                .flex_1()
                .min_w_0()
                .child(gap_line(&gap, "tasks", space, cx))
                .into_any_element(),
            None if view_mode == ProjectTasksViewMode::Graph => {
                let dag = build_task_dag(project_tasks, when);
                self.task_dag_view("project-task-dag", space, dag, cx)
            }
            None if view_mode == ProjectTasksViewMode::List => {
                let mut ordered: Vec<_> = project_tasks.iter().collect();
                ordered.sort_by_key(|s| {
                    std::cmp::Reverse((s.completed_at.unwrap_or(s.task.created_at), s.task.id))
                });
                let mono = cx.theme().mono_font_family.clone();
                let mut rows = v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap(XS)
                    .overflow_y_scrollbar()
                    .id("project-tasks-list");
                for summary in ordered {
                    let task = &summary.task;
                    let screen = Screen::Task {
                        space: space.to_owned(),
                        task: task.id,
                    };
                    let status = status_label(task.status, task.waiting_reason.as_deref());
                    let status_kind = Status::task(status);
                    let quiet = task.status == threadlane_core::model::TaskStatus::Done;
                    let time = work_elapsed(summary, when).unwrap_or_else(|| {
                        when.moment(summary.completed_at.unwrap_or(task.created_at))
                    });
                    rows = rows.child(
                        button(SharedString::from(format!("project-list-task-{}", task.id)))
                            .ghost()
                            .w_full()
                            .min_h(CONTROL_HEIGHT)
                            .rounded(ROW_RADIUS)
                            .px(SM)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap(SM)
                                    .items_center()
                                    .child(
                                        h_flex()
                                            .w(TYPE_META)
                                            .flex_none()
                                            .justify_center()
                                            .children(
                                                (!quiet).then(|| status_dot(status_kind, cx)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w(rems(4.5))
                                            .flex_none()
                                            .text_size(TYPE_META)
                                            .font_family(mono.clone())
                                            .font_features(TABULAR.clone())
                                            .text_color(muted)
                                            .child(task.id.to_string()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(TYPE_BODY)
                                            .text_color(if quiet {
                                                muted
                                            } else {
                                                cx.theme().foreground
                                            })
                                            .child(task.title.clone()),
                                    )
                                    .child(
                                        meta(summary.agent.clone(), cx)
                                            .w(rems(7.))
                                            .flex_none()
                                            .truncate(),
                                    )
                                    .child(
                                        meta(time, cx)
                                            .w(rems(5.5))
                                            .flex_none()
                                            .font_features(TABULAR.clone())
                                            .text_right(),
                                    )
                                    .child(
                                        h_flex().w(rems(8.5)).flex_none().justify_end().children(
                                            (!quiet).then(|| status_badge(status, status_kind, cx)),
                                        ),
                                    ),
                            ),
                    );
                }
                rows.into_any_element()
            }
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
                                .children(cells),
                        ),
                )
                .scrollbar(&self.board_scroll, ScrollbarAxis::Both)
                .into_any_element(),
        };

        let decisions: Vec<_> = self
            .state
            .visible_decisions()
            .filter(|_| !archived)
            .map(|queued| self.decision(queued, when, cx))
            .collect();
        v_flex()
            .flex_1()
            .w_full()
            .h_full()
            .min_h_0()
            .min_w_0()
            .gap(MD)
            .child(mode_switcher)
            .when(!decisions.is_empty(), |column| {
                // Capped and scrollable so decisions never push the board off the page.
                column.child(
                    decision_scroller(
                        "board-decisions",
                        v_flex()
                            .gap(MD)
                            .child(section("Open decisions"))
                            .children(decisions),
                        &self.decision_scroll,
                    )
                    .flex_none()
                    .max_h(relative(0.45)),
                )
            })
            .children(is_empty.then(|| empty(EmptyRegion::Board, cx)))
            .child(board)
            .into_any_element()
    }

    /// Repositories and machines the project runs with.
    fn setup_tab(&self, space: &str, project: &str, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let archived = self.state.is_archived(space, project);
        let view = self.state.project(space, project);
        let mut side = v_flex().gap(SM);
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
        let mut machines = v_flex().gap(SM).child(section("Machines"));
        if let Some(gap) = self
            .state
            .space(space)
            .and_then(|known| known.gap(Part::Machines))
        {
            machines = machines.child(gap_line(&gap, "machines", space, cx));
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
            machines = machines.child(
                h_flex()
                    .gap(XS)
                    .text_size(TYPE_BODY)
                    .child(
                        link(
                            format!("setup-machine-{}", machine.id),
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

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .overflow_y_scrollbar()
            .id("setup")
            .gap(XL)
            .child(side)
            .child(machines)
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
        details_in_sheet: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        // Always keep this control and the history status in a single child before the rows.
        let mut history = v_flex().gap(XS).items_start();
        let mut has_header = false;
        if let Some(before) = self.state.earlier(space, project) {
            has_header = true;
            let status = self.state.earlier_status(space, project);
            let label = match status {
                Some(EarlierStatus::Loading) => "Loading earlier messages…",
                Some(EarlierStatus::Failed(error)) => {
                    history = history.child(meta(
                        format!("Could not load earlier messages: {error}"),
                        cx,
                    ));
                    "Retry"
                }
                None => "Show earlier",
            };
            let (space, project) = (space.to_owned(), project.to_owned());
            history = history.child(
                button("earlier")
                    .small()
                    .tab_stop(true)
                    .label(label)
                    .disabled(status == Some(&EarlierStatus::Loading))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.earlier(space.clone(), project.clone(), before, cx);
                    })),
            );
        }
        if let Some(gap) = self.state.project_gap(space, project, Part::Conversation) {
            has_header = true;
            history = history.child(gap_line(&gap, "conversation", space, cx));
        }
        let mut lines = v_flex().gap(MD).children(has_header.then_some(history));
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
                            .label(format!("Open {space} space chat"))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                            )
                    }));
            }
        }
        let recorded = self.state.recorded(space, project, when);
        let newest = newest_message(&conversation);
        let (run_ink, run_tint) = Status::Running.colors(cx.theme());
        let (fail_ink, fail_tint) = Status::Failed.colors(cx.theme());
        for (index, line) in conversation.iter().enumerate() {
            let limit = message_limit(newest, index);
            let said = |id: String,
                        who: &str,
                        icon: Option<AppIcon>,
                        time: &str,
                        text: &str,
                        reply: bool| {
                let reply = reply && main_chat;
                let id = format!("conversation-{space}-{project}-{id}");
                let menu_text = text.to_owned();
                let menu_view = cx.entity().downgrade();
                v_flex()
                    .id(SharedString::from(format!("menu-{id}")))
                    .w_full()
                    .gap(XS)
                    .child(
                        h_flex()
                            .w_full()
                            .gap(SM)
                            .items_center()
                            .children(icon.map(|ic| {
                                h_flex()
                                    .size(px(22.))
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(run_tint)
                                    .child(Icon::new(ic).xsmall().text_color(run_ink))
                            }))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .min_w_0()
                                    .child(who.to_owned()),
                            )
                            .child(div().flex_1())
                            .child(
                                meta(time.to_owned(), cx)
                                    .font_features(TABULAR.clone())
                                    .text_right(),
                            ),
                    )
                    .children((!text.is_empty()).then(|| {
                        self.prose(id, space, Some(project), text.to_owned(), cx)
                            .limit(limit)
                    }))
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
                    div()
                        .w_full()
                        .px(MD)
                        .py(SM)
                        .rounded(ROW_RADIUS)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().group_box)
                        .child(
                            said(
                                format!("you-{id}"),
                                "You",
                                None,
                                time,
                                text,
                                !text.is_empty(),
                            )
                            .children(crate::components::chat_images::previews(images, cx)),
                        ),
                ),
                Line::SpaceLead { id, text, time } => {
                    let toggle_key = format!("handoff-space-{space}-{project}-{id}");
                    let expanded = self.state.message_expanded(&toggle_key);
                    let preview = handoff_preview(text);
                    let key_for_click = toggle_key.clone();
                    lines.child(
                        v_flex()
                            .w_full()
                            .gap(XS)
                            .px(MD)
                            .py(SM)
                            .rounded(ROW_RADIUS)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().group_box)
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap(SM)
                                    .items_center()
                                    .child(Icon::new(AppIcon::GitFork).xsmall().text_color(muted))
                                    .child(
                                        div()
                                            .text_size(TYPE_META)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Space lead ask"),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        button(SharedString::from(format!("toggle-{toggle_key}")))
                                            .ghost()
                                            .xsmall()
                                            .icon(if expanded {
                                                AppIcon::ChevronDown
                                            } else {
                                                AppIcon::ChevronRight
                                            })
                                            .label(if expanded { "Hide" } else { "Show" })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.state.toggle_message(&key_for_click);
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        meta(time.clone(), cx)
                                            .font_features(TABULAR.clone())
                                            .text_right(),
                                    ),
                            )
                            .child(if expanded {
                                self.prose(
                                    format!("conversation-{space}-{project}-space-lead-{id}"),
                                    space,
                                    Some(project),
                                    text.clone(),
                                    cx,
                                )
                                .into_any_element()
                            } else {
                                meta(preview, cx).truncate().into_any_element()
                            }),
                    )
                }
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
                    let toggle_key = format!("handoff-reply-{space}-{project}-{id}");
                    let expanded = self.state.message_expanded(&toggle_key);
                    let preview = handoff_preview(text);
                    let key_for_click = toggle_key.clone();
                    lines.child(
                        v_flex()
                            .w_full()
                            .gap(XS)
                            .px(MD)
                            .py(SM)
                            .rounded(ROW_RADIUS)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().group_box)
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap(SM)
                                    .items_center()
                                    .child(Icon::new(AppIcon::GitFork).xsmall().text_color(run_ink))
                                    .child(
                                        div()
                                            .text_size(TYPE_META)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(format!("{from} lead")),
                                    )
                                    .child(meta("handoff", cx))
                                    .child(
                                        button(SharedString::from(format!("open-reply-{id}")))
                                            .xsmall()
                                            .outline()
                                            .tab_stop(true)
                                            .label(format!("Open {from}"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.go(screen.clone(), cx)
                                            })),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        button(SharedString::from(format!("toggle-{toggle_key}")))
                                            .ghost()
                                            .xsmall()
                                            .icon(if expanded {
                                                AppIcon::ChevronDown
                                            } else {
                                                AppIcon::ChevronRight
                                            })
                                            .label(if expanded { "Hide" } else { "Show" })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.state.toggle_message(&key_for_click);
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        meta(time.clone(), cx)
                                            .font_features(TABULAR.clone())
                                            .text_right(),
                                    ),
                            )
                            .child(if expanded {
                                self.prose(
                                    format!("conversation-{space}-{project}-reply-{id}"),
                                    space,
                                    Some(from.as_str()),
                                    text.clone(),
                                    cx,
                                )
                                .into_any_element()
                            } else {
                                meta(preview, cx).truncate().into_any_element()
                            }),
                    )
                }
                Line::Lead { seq, text, time } => {
                    let speaker = if main_chat { "Space Lead" } else { "Lead" };
                    lines.child(div().w_full().py(XS).child(said(
                        format!("lead-{seq}"),
                        speaker,
                        Some(AppIcon::Bot),
                        time,
                        text,
                        true,
                    )))
                }
                Line::Notice { seq, text, time } => lines.child(
                    h_flex()
                        .w_full()
                        .gap(SM)
                        .items_start()
                        .px(SM)
                        .py(XS)
                        .child(status_dot(Status::Idle, cx))
                        .child(
                            div().flex_1().min_w_0().child(
                                self.prose(
                                    format!("conversation-{space}-{project}-notice-{seq}"),
                                    space,
                                    Some(project),
                                    text.clone(),
                                    cx,
                                )
                                .limit(limit),
                            ),
                        )
                        .child(
                            meta(time.clone(), cx)
                                .flex_none()
                                .font_features(TABULAR.clone()),
                        ),
                ),
                Line::Sending {
                    id,
                    text,
                    time,
                    images,
                } => lines.child(
                    div()
                        .w_full()
                        .px(MD)
                        .py(SM)
                        .rounded(ROW_RADIUS)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().group_box)
                        .child(
                            said(
                                format!("sending-{id}"),
                                "You · Sending…",
                                None,
                                time,
                                text,
                                false,
                            )
                            .children(crate::components::chat_images::previews(images, cx)),
                        ),
                ),
                Line::Stopped { time } => lines.child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .items_center()
                        .px(SM)
                        .py(XS)
                        .child(meta("Lead run stopped", cx))
                        .child(meta(time.clone(), cx).font_features(TABULAR.clone())),
                ),
                Line::DeliveryFailure { id, text, time } => lines.child(
                    v_flex()
                        .w_full()
                        .gap(XS)
                        .px(MD)
                        .py(SM)
                        .rounded(ROW_RADIUS)
                        .border_1()
                        .border_color(fail_ink)
                        .bg(fail_tint)
                        .child(said(
                            format!("failed-{id}"),
                            "Delivery failed",
                            None,
                            time,
                            text,
                            false,
                        )),
                ),
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
                                    Some(project),
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
                            None,
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

        let details = (!main_chat && details_in_sheet).then(|| {
            let (space, project) = (space.to_owned(), project.to_owned());
            button("conversation-details")
                .small()
                .outline()
                .self_start()
                .icon(AppIcon::Columns3)
                .label("Conversation details")
                .on_click(cx.listener(move |_, _, window, cx| {
                    let view = cx.entity().downgrade();
                    let (space, project) = (space.clone(), project.clone());
                    window.open_sheet(cx, move |sheet, window, cx| {
                        let width = RULES_WIDTH
                            .to_pixels(window.rem_size())
                            .min(window.viewport_size().width);
                        let content = view.update(cx, |this, cx| {
                            this.conversation_details(&space, &project, &this.when(), true, cx)
                        });
                        let view = view.clone();
                        sheet
                            .title("Conversation details")
                            .size(width)
                            .resizable(false)
                            .children(content.ok())
                            .on_close(move |_, window, cx| {
                                // Resizing during the sheet can remove its opener.
                                let _gone = view.update(cx, |this, cx| {
                                    if !this.keyboard_focus.contains_focused(window, cx) {
                                        this.keyboard_focus.focus(window, cx);
                                    }
                                });
                            })
                    });
                }))
        });
        let side = (!main_chat && !details_in_sheet).then(|| {
            v_flex()
                .id("conversation-sidebar")
                .w(SIDEBAR_WIDTH)
                .h_full()
                .flex_none()
                .pr(SCROLLBAR_GUTTER)
                .overflow_y_scrollbar()
                .child(self.conversation_details(space, project, when, false, cx))
        });
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
                    .children(details)
                    .child(self.conversation_scroller(
                        space,
                        project,
                        lines,
                        &conversation,
                        has_header,
                        cx,
                    ))
                    .children((!self.state.is_archived(space, project)).then(|| {
                        let page = (space.to_owned(), project.to_owned());
                        let picker = self.to_picker(
                            self.state.chat_target(space, project),
                            move |state, space, project| {
                                state.set_chat_target((&page.0, &page.1), (space, project));
                            },
                            cx,
                        );
                        self.render_composer(
                            threadlane_desktop::Draft::Lead,
                            "Message the lead",
                            Some(picker),
                            cx,
                        )
                        .flex_none()
                        .w_auto()
                        .mr(SCROLLBAR_GUTTER)
                    })),
            )
            .children(side)
            .into_any_element()
    }

    fn open_conversation_detail(
        &mut self,
        screen: Screen,
        in_sheet: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if in_sheet {
            window.close_sheet(cx);
            self.keyboard_focus.focus(window, cx);
        }
        self.go(screen, cx);
    }

    fn conversation_details(
        &self,
        space: &str,
        project: &str,
        when: &When,
        in_sheet: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut side = v_flex().gap(SM);
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
        let total_tasks = tasks.len();
        let mut sorted_tasks: Vec<_> = tasks.iter().collect();
        sorted_tasks.sort_by(|a, b| {
            let active = |status: threadlane_core::model::TaskStatus| match status {
                threadlane_core::model::TaskStatus::WaitingOnYou
                | threadlane_core::model::TaskStatus::Proposed => 0u8,
                threadlane_core::model::TaskStatus::Failed => 1,
                threadlane_core::model::TaskStatus::Running
                | threadlane_core::model::TaskStatus::InReview => 2,
                threadlane_core::model::TaskStatus::Queued
                | threadlane_core::model::TaskStatus::Sleeping => 3,
                threadlane_core::model::TaskStatus::Done
                | threadlane_core::model::TaskStatus::Cancelled => 4,
            };
            active(a.task.status)
                .cmp(&active(b.task.status))
                .then_with(|| {
                    b.completed_at
                        .unwrap_or(b.task.created_at)
                        .cmp(&a.completed_at.unwrap_or(a.task.created_at))
                })
                .then_with(|| b.task.id.cmp(&a.task.id))
        });
        const SIDEBAR_TASKS_SHOWN: usize = 10;
        // Capped with its own scroll so open decisions stay in view below.
        let mut task_list = v_flex().gap(SM).pr(SCROLLBAR_GUTTER);
        for summary in sorted_tasks.into_iter().take(SIDEBAR_TASKS_SHOWN) {
            let screen = Screen::Task {
                space: space.to_owned(),
                task: summary.task.id,
            };
            let status = status_label(summary.task.status, summary.task.waiting_reason.as_deref());
            task_list = task_list
                .child(
                    link(
                        format!("from-{}", summary.task.id),
                        format!("{} {}", summary.task.id, summary.task.title),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_conversation_detail(screen.clone(), in_sheet, window, cx);
                    })),
                )
                .child(
                    h_flex()
                        .gap(XS)
                        .justify_between()
                        .w_full()
                        .child(status_badge(status, Status::task(status), cx))
                        .children(threadlane_desktop::work_started(summary).map(|since| {
                            crate::components::Elapsed::new(
                                format!("elapsed-{space}-{}", summary.task.id),
                                since,
                            )
                        })),
                );
        }
        side = side.child(
            task_list
                .id("conversation-sidebar-tasks")
                .flex_none()
                .max_h(SIDEBAR_SECTION_MAX_HEIGHT)
                .overflow_y_scrollbar(),
        );
        if total_tasks > SIDEBAR_TASKS_SHOWN {
            let all_screen = Screen::Project {
                space: space.to_owned(),
                project: project.to_owned(),
                tab: Tab::Tasks,
            };
            side = side.child(
                button("sidebar-all-tasks")
                    .small()
                    .ghost()
                    .self_start()
                    .label(format!("View all {total_tasks} tasks"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_conversation_detail(all_screen.clone(), in_sheet, window, cx);
                    })),
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
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_conversation_detail(memory.clone(), in_sheet, window, cx);
                })),
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
                                false,
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
                                Some(project),
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
                let versions = self.state.memory_version_rows(space, project, when);
                if let Some(gap) = self.state.memory_versions_gap(space, project) {
                    detail = detail.child(gap_line(&gap, "memory versions", space, cx));
                } else if versions.is_empty() {
                    detail = detail.child(empty(EmptyRegion::MemoryVersions, cx));
                }
                detail = detail.children(versions.into_iter().map(|row| {
                    let (in_space, named) = (space.to_owned(), project.to_owned());
                    let label = if row.current {
                        format!("{} · current", row.label)
                    } else {
                        row.label
                    };
                    button(format!(
                        "memory-version-{space}-{project}-{path}-{}",
                        row.version
                    ))
                    .ghost()
                    .label(label.clone())
                    .accessibility_label(format!("Inspect {label}"))
                    .tab_stop(true)
                    .selected(row.selected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state
                            .inspect_memory_version(&in_space, &named, row.version);
                        cx.notify();
                    }))
                }));
                if let Some(version) = self.state.memory_inspected(space, project) {
                    detail =
                        detail.child(section(format!("Version {} · read-only", version.version)));
                    if version.deleted {
                        detail = detail.child(meta(
                            "This version records a deletion; it has no file contents.",
                            cx,
                        ));
                    } else {
                        detail = detail.child(self.prose(
                            format!(
                                "memory-history-{space}-{project}-{path}-{}",
                                version.version
                            ),
                            space,
                            Some(project),
                            version.content.clone(),
                            cx,
                        ));
                    }
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
            .child(meta("Reviews this project’s conversations, decisions, tasks and memory. You review any proposed GitHub report before publication.", cx));
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
            .children(card.changes.into_iter().map(|change| {
                let (space, project) = (space.to_owned(), project.to_owned());
                let path = change.path;
                let version = change.version;
                let action = if change.deleted { "Deleted" } else { "Wrote" };
                let label = if version > 1 {
                    format!("{action} {path} · v{} → v{version}", version - 1)
                } else {
                    format!("{action} {path} · v{version}")
                };
                button(format!("dream-change-{path}-{version}"))
                    .ghost()
                    .label(label.clone())
                    .accessibility_label(format!("Inspect history: {label}"))
                    .tab_stop(true)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_memory(&space, &project, &path, cx);
                        this.state.preview_dream_version(&space, &project, version);
                    }))
            }))
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

    /// The project's mode and each assigned agent's on/off, own mode, and
    /// permissions.
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
            .min_w_0()
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .overflow_y_scrollbar()
            .id("project-team")
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
                        v_flex().min_w_0().child(section("Project mode")).child(
                            div()
                                .text_size(TYPE_BODY)
                                .text_color(muted)
                                .child(card.note.clone()),
                        ),
                    )
                    .child(ceiling.disabled(archived)),
            )
            .children(locked_note(&card.choices, muted));
        let gap = self.state.project_gap(space, project, Part::Bindings);
        let agents = self.state.agent_rows(space, project);
        let team = Screen::Agents(Some(space.to_owned()));
        let add = (!archived && gap.is_none())
            .then(|| self.add_project_agent(space, project, &agents, cx))
            .flatten();
        rows = rows.child(
            h_flex()
                .justify_between()
                .flex_wrap()
                .gap(SM)
                .child(
                    section("Assigned agents")
                        .children(gap.is_none().then(|| count(agents.len(), cx)).flatten()),
                )
                .child(
                    h_flex()
                        .gap(SM)
                        .flex_wrap()
                        .child(
                            button("project-space-team")
                                .small()
                                .ghost()
                                .label("Space team")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.go(team.clone(), cx)),
                                ),
                        )
                        .children(add),
                ),
        );
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
            let own_control = mode_control(format!("mode-{agent}"), &row.choices).on_click(
                cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                    if let Some(mode) = unlocked(&choices, clicked) {
                        this.set_mode(
                            in_space.clone(),
                            in_project.clone(),
                            named.clone(),
                            Some(mode),
                            cx,
                        );
                    }
                }),
            );
            let (in_space, in_project, named) =
                (space.to_owned(), project.to_owned(), agent.clone());
            // On follows the project; Off is the agent's own.
            let on = Switch::new(SharedString::from(format!("on-{agent}")))
                .label("On")
                .accessibility_label(format!("{agent} on in this project"))
                .checked(row.own != Some(Mode::Off))
                .disabled(archived)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    let mode = (!*checked).then_some(Mode::Off);
                    this.set_mode(
                        in_space.clone(),
                        in_project.clone(),
                        named.clone(),
                        mode,
                        cx,
                    );
                }));
            let (in_space, in_project, named) =
                (space.to_owned(), project.to_owned(), agent.clone());
            let own: Option<AnyElement> = match row.own {
                Some(Mode::Off) => None,
                Some(mode) => Some(
                    h_flex()
                        .gap(SM)
                        .child(meta(format!("Own mode · {}", mode_name(mode)), cx))
                        .child(
                            button(SharedString::from(format!("follow-{agent}")))
                                .small()
                                .ghost()
                                .label("Follow project")
                                .disabled(archived)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_mode(
                                        in_space.clone(),
                                        in_project.clone(),
                                        named.clone(),
                                        None,
                                        cx,
                                    );
                                })),
                        )
                        .into_any_element(),
                ),
                None => Some(
                    h_flex()
                        .gap(SM)
                        .child(meta("Follows project", cx))
                        .child(
                            button(SharedString::from(format!("own-mode-{agent}")))
                                .small()
                                .ghost()
                                .label("Set own mode…")
                                .selected(row.own_open)
                                .disabled(archived)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.toggle_own_mode(&in_space, &in_project, &named);
                                    cx.notify();
                                })),
                        )
                        .into_any_element(),
                ),
            };
            let (toggle_space, toggle_project, toggle_agent) =
                (space.to_owned(), project.to_owned(), agent.clone());
            let expand = button(SharedString::from(format!("expand-{agent}")))
                .small()
                .ghost()
                .icon(if row.open {
                    AppIcon::ChevronDown
                } else {
                    AppIcon::ChevronRight
                })
                .label("Permissions")
                .selected(row.open)
                .accessibility_label(format!(
                    "{} permissions for {agent}",
                    if row.open { "Hide" } else { "Show" }
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state
                        .toggle_agent(&toggle_space, &toggle_project, &toggle_agent);
                    cx.notify();
                }));
            let role = if row.is_lead {
                Some("Project lead".to_owned())
            } else {
                (!agent.eq_ignore_ascii_case(&row.role)).then_some(row.role)
            };
            let above_ceiling = row
                .choices
                .iter()
                .any(|choice| choice.selected && choice.locked.is_some());
            rows = rows.child(
                v_flex()
                    .gap(SM)
                    .py(SM)
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap(SM)
                            .justify_between()
                            .flex_wrap()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_32()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(agent.clone()),
                                    )
                                    .children(role.map(|role| meta(role, cx))),
                            )
                            .children(own)
                            .child(on)
                            .child(expand),
                    )
                    .children(row.own_open.then(|| own_control.disabled(archived)))
                    .children(
                        above_ceiling
                            .then(|| locked_note(&row.choices, muted))
                            .flatten(),
                    )
                    .children(
                        row.open
                            .then(|| self.may_do(space, project, &agent, cx))
                            .flatten(),
                    ),
            );
        }
        rows.into_any_element()
    }

    fn add_project_agent(
        &self,
        space: &str,
        project: &str,
        assigned: &[threadlane_desktop::AgentRow],
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let available: Vec<_> = self
            .state
            .space(space)?
            .agents
            .iter()
            .filter(|agent| !assigned.iter().any(|row| row.agent == agent.name))
            .map(|agent| agent.name.clone())
            .collect();
        if available.is_empty() {
            return None;
        }
        let view = cx.entity().downgrade();
        let (space, project) = (space.to_owned(), project.to_owned());
        Some(
            button("project-add-agent")
                .small()
                .icon(AppIcon::UserPlus)
                .label("Add agent")
                .dropdown_menu(move |mut menu, _, _| {
                    // A new agent follows the project's mode.
                    for agent in &available {
                        let (space, project, agent, view) =
                            (space.clone(), project.clone(), agent.clone(), view.clone());
                        menu = menu.item(PopupMenuItem::new(agent.clone()).on_click(
                            move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.set_mode(
                                        space.clone(),
                                        project.clone(),
                                        agent.clone(),
                                        None,
                                        cx,
                                    );
                                });
                            },
                        ));
                    }
                    menu
                })
                .into_any_element(),
        )
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
    fn show_new_task_form(
        &mut self,
        space: &str,
        project: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.open_task_form(space, project, true);
        self.task_form_return_focus = window.focused(cx);
        let target = self.screen.clone();
        let view = cx.entity().downgrade();
        let (space, project) = (space.to_owned(), project.to_owned());
        window.on_next_frame(move |window, cx| {
            let _gone = view.update(cx, |this, cx| {
                if this.screen == target
                    && this.state.task_form(&space, &project).is_some()
                    && !this.state.is_archived(&space, &project)
                {
                    this.task_goal.focus_handle(cx).focus(window, cx);
                }
            });
        });
        cx.notify();
    }

    /// Quick create (`C`, or "New task…" in the palette): the new task
    /// form of the project you are in, or of the space's chat project.
    pub(crate) fn quick_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let here = match &self.screen {
            Screen::Project { space, project, .. } => Some((space.clone(), project.clone())),
            Screen::Task { space, task } => self
                .state
                .task(space, *task)
                .map(|detail| (space.clone(), detail.summary.project.clone())),
            _ => None,
        };
        let target = self
            .state
            .quick_create_target(here.as_ref().map(|(s, p)| (s.as_str(), p.as_str())));
        let Some((space, project)) = target else {
            self.state.report(threadlane_desktop::Activity::Failed(
                "Pair a space before creating a task.".to_owned(),
            ));
            cx.notify();
            return;
        };
        self.go(
            Screen::Project {
                space: space.clone(),
                project: project.clone(),
                tab: Tab::Tasks,
            },
            cx,
        );
        self.show_new_task_form(&space, &project, window, cx);
    }

    fn hide_new_task_form(
        &mut self,
        space: &str,
        project: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restore_new_task_focus(window, cx);
        self.state.open_task_form(space, project, false);
        cx.notify();
    }

    pub(crate) fn restore_new_task_focus(&mut self, window: &mut Window, cx: &mut App) {
        let return_to = self.task_form_return_focus.take();
        if self.task_form_focus.contains_focused(window, cx) {
            return_to
                .filter(|focus| self.keyboard_focus.contains(focus, window))
                .unwrap_or_else(|| self.keyboard_focus.clone())
                .focus(window, cx);
        }
    }

    fn task_form(&self, space: &str, project: &str, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let Some(form) = self.state.task_form(space, project) else {
            return div().into_any_element();
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
                .child("Turn on an agent in the Team tab to assign this task.")
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
        let mut projects = h_flex().gap(XS).flex_wrap();
        for name in self
            .state
            .space(space)
            .map(|known| {
                known
                    .active_projects()
                    .map(|p| p.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        {
            let (in_space, other) = (space.to_owned(), name.clone());
            projects = projects.child(
                button(SharedString::from(format!("task-project-{name}")))
                    .small()
                    .label(name.clone())
                    .selected(name == project)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.go(
                            Screen::Project {
                                space: in_space.clone(),
                                project: other.clone(),
                                tab: Tab::Tasks,
                            },
                            cx,
                        );
                        this.show_new_task_form(&in_space, &other, window, cx);
                    })),
            );
        }
        let (in_space, named) = (space.to_owned(), project.to_owned());
        let (close_space, close_project) = (space.to_owned(), project.to_owned());
        v_flex()
            .id("new-task-form")
            .track_focus(&self.task_form_focus)
            .gap(SM)
            .p(MD)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(ROW_RADIUS)
            .child(section("New task"))
            .child(h_flex().gap(SM).child(label("Project")).child(projects))
            .child(
                v_flex()
                    .gap(XS)
                    .child(meta("Prompt", cx))
                    .child(field(&self.task_goal).aria_label("Prompt: what the task should do")),
            )
            .child(
                v_flex()
                    .gap(XS)
                    .child(meta("Title (optional, from the prompt's first line)", cx))
                    .child(field(&self.task_title).aria_label("Title")),
            )
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
                h_flex().gap(SM).child(label("When done")).child(
                    button("task-draft-pr")
                        .small()
                        .label("Open a draft pull request")
                        .selected(form.draft_pr)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let form = this.state.task_form_mut();
                            form.draft_pr = !form.draft_pr;
                            cx.notify();
                        })),
                ),
            )
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
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.create_task(in_space.clone(), named.clone(), window, cx);
                            })),
                    )
                    .child(
                        button("task-close")
                            .icon(AppIcon::X)
                            .label("Cancel")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.hide_new_task_form(&close_space, &close_project, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Select a checkout or enter its clone details manually.
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
                    this.reset_repository_form();
                    this.state.open_repository_form(&in_space, &named, true);
                    cx.notify();
                }))
                .into_any_element();
        }
        let (close_space, close_project) = (space.to_owned(), project.to_owned());
        let (pick_space, pick_project) = (space.to_owned(), project.to_owned());
        v_flex()
            .gap(SM)
            .child(
                button("repository-folder")
                    .small()
                    .self_start()
                    .icon(AppIcon::FolderOpen)
                    .tab_stop(true)
                    .label("Choose repository folder…")
                    .disabled(self.repository_picking.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_repository_folder(
                            pick_space.clone(),
                            pick_project.clone(),
                            window,
                            cx,
                        );
                    })),
            )
            .child(
                div()
                    .text_size(TYPE_META)
                    .text_color(cx.theme().muted_foreground)
                    .child(self.repository_hint.clone().unwrap_or_else(|| {
                        "Choose a Git checkout or enter its details below.".into()
                    })),
            )
            .children(
                [
                    ("Name in project", &self.repository_name),
                    ("Clone URL or path on hub", &self.repository_remote),
                    ("Default branch", &self.repository_branch),
                ]
                .into_iter()
                .map(|(title, input)| {
                    v_flex()
                        .gap(XS)
                        .child(meta(title, cx))
                        .child(field(input).disabled(self.repository_picking.is_some()))
                }),
            )
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
                            .disabled(self.repository_picking.is_some())
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
                                this.reset_repository_form();
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

/// Strips internal relay boilerplate ("The lead of <p> replied to your ask (event N): ")
/// and collapses whitespace into a clean single-line preview for collapsed handoff cards.
fn handoff_preview(text: &str) -> String {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("The lead of ")
        .and_then(|rest| rest.split_once("): "))
        .map_or(trimmed, |(_, tail)| tail.trim_start());
    let first_line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(trimmed);
    first_line.to_owned()
}

#[cfg(test)]
mod chat_details_tests {
    use super::{chat_details_in_sheet, px};

    #[test]
    fn narrow_chat_uses_drawer_without_changing_global_sidebar() {
        assert!(chat_details_in_sheet(px(640.), px(16.), false));
        assert!(chat_details_in_sheet(px(760.), px(16.), false));
        assert!(!chat_details_in_sheet(px(760.), px(16.), true));
        assert!(!chat_details_in_sheet(px(1024.), px(16.), false));
        assert!(chat_details_in_sheet(px(1024.), px(24.), false));
    }
}

/// A board card on its way to another column.
#[derive(Clone)]
pub(crate) struct BoardDrag {
    space: String,
    id: threadlane_core::ids::TaskId,
    title: String,
    from: &'static str,
}

impl Render for BoardDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(BOARD_COLUMN_MIN)
            .p(SM)
            .rounded(ROW_RADIUS)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_md()
            .text_size(TYPE_BODY)
            .truncate()
            .child(self.title.clone())
    }
}

impl Desktop {
    /// The card's quick actions, shown while the pointer is over it; its
    /// context menu offers the same ones from the keyboard.
    fn card_actions(
        &self,
        space: &str,
        card: &threadlane_desktop::BoardCard,
        group: &SharedString,
        cx: &Context<Self>,
    ) -> Div {
        let id = card.id;
        let small = |key: &str, label: &str| {
            button(SharedString::from(format!("card-{key}-{id}")))
                .small()
                .ghost()
                .tab_stop(false)
                .label(label.to_owned())
        };
        let mut row = h_flex()
            .gap(XS)
            .flex_wrap()
            .opacity(0.)
            .group_hover(group.clone(), |style| style.opacity(1.));
        if let Some(url) = card.review.clone() {
            row = row.child(
                small("review", "Open review")
                    .icon(AppIcon::ExternalLink)
                    .accessibility_label(format!("Open the pull request of {id}"))
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        if card.can_retry {
            let space = space.to_owned();
            row = row.child(
                small("retry", "Retry")
                    .accessibility_label(format!("Retry {id}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.retry_task(space.clone(), id, cx);
                    })),
            );
        }
        if card.can_cancel {
            let space = space.to_owned();
            row = row.child(
                small("cancel", "Cancel…")
                    .accessibility_label(format!("Cancel {id}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_action(Consequence::CancelTask(space.clone(), id), window, cx);
                    })),
            );
        }
        if !card.reassign.is_empty() {
            let (space, agents, view) = (
                space.to_owned(),
                card.reassign.clone(),
                cx.entity().downgrade(),
            );
            row = row.child(
                small("reassign", "Reassign")
                    .dropdown_caret(true)
                    .accessibility_label(format!("Give {id} to another agent"))
                    .dropdown_menu(move |mut menu, _, _| {
                        for agent in &agents {
                            let (space, agent, view) = (space.clone(), agent.clone(), view.clone());
                            menu = menu.item(PopupMenuItem::new(agent.clone()).on_click(
                                move |_, _, cx| {
                                    let _gone = view.update(cx, |this, cx| {
                                        this.reassign_task(space.clone(), id, agent.clone(), cx);
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            );
        }
        row
    }

    /// Makes the status change a card dropped on column `to` stands for, or
    /// says why the hub has none.
    fn drop_card(
        &mut self,
        drag: &BoardDrag,
        to: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match threadlane_desktop::board_move(drag.from, to) {
            Ok(threadlane_desktop::BoardMove::Stay) => {}
            Ok(threadlane_desktop::BoardMove::Retry) => {
                self.retry_task(drag.space.clone(), drag.id, cx);
            }
            Ok(threadlane_desktop::BoardMove::Cancel) => self.confirm_action(
                Consequence::CancelTask(drag.space.clone(), drag.id),
                window,
                cx,
            ),
            Err(why) => {
                self.state.report(threadlane_desktop::Activity::Failed(why));
                cx.notify();
            }
        }
    }
}
