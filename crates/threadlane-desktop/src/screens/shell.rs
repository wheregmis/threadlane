//! Navigation, space reachability, and the last action.

use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_desktop::{
    Activity, AppState, Connection, ConnectionStatus, Consequence, EmptyRegion, Part, Space, When,
};

use super::{delete_project::delete_project_item, empty, gap_line, menu_item};
use crate::components::{icon_button, meta, row, status_dot};
use crate::icons::AppIcon;
use crate::menus::{GoHome, OpenAddSpace, OpenSettings, SidebarShortcut, shortcut_hint};
use crate::tokens::{BAR_HEIGHT, LG, MD, SIDEBAR_WIDTH, SM, Status, TYPE_EMPHASIS, XS};
use crate::{Desktop, Screen, Tab};

struct NavigationRow {
    id: String,
    icon: AppIcon,
    label: String,
    count: Option<SharedString>,
    /// Running work here: a decoration, so the row never moves.
    running: bool,
    screen: Screen,
}

enum SidebarEntry<'a> {
    Row(NavigationRow),
    Space(&'a Space),
    /// The "Projects" label and its new-project button.
    Projects(&'a Space),
    Label(&'static str),
    App,
    Archived(&'a Space),
}

fn sidebar_entries(state: &AppState) -> Vec<SidebarEntry<'_>> {
    let row = |id: String, icon, label: String, count, running, screen| {
        SidebarEntry::Row(NavigationRow {
            id,
            icon,
            label,
            count,
            running,
            screen,
        })
    };
    let mut entries = vec![row(
        "go-home".into(),
        AppIcon::Inbox,
        "Inbox".into(),
        positive_count(state.queue().len()),
        false,
        Screen::Home,
    )];
    for space in state.spaces() {
        entries.push(SidebarEntry::Space(space));
        if let Some(project) = space.active_projects().find(|project| project.is_general) {
            entries.push(row(
                format!("main-chat-{}", space.name),
                AppIcon::MessagesSquare,
                "Chat".into(),
                positive_count(state.open_in(&space.name, project.id)),
                state.is_working(&space.name, &project.name),
                Screen::Project {
                    space: space.name.clone(),
                    project: project.name.clone(),
                    tab: Tab::Conversation,
                },
            ));
        }
        entries.push(row(
            format!("tasks-{}", space.name),
            AppIcon::Columns3,
            "Tasks".into(),
            None,
            false,
            Screen::SpaceTasks {
                space: space.name.clone(),
            },
        ));
        entries.push(SidebarEntry::Projects(space));
        entries.extend(
            space
                .active_projects()
                .filter(|project| !project.is_general)
                .map(|project| {
                    row(
                        format!("go-{}-{}", space.name, project.name),
                        AppIcon::Folder,
                        project.name.clone(),
                        positive_count(state.open_in(&space.name, project.id)),
                        state.is_working(&space.name, &project.name),
                        Screen::Project {
                            space: space.name.clone(),
                            project: project.name.clone(),
                            tab: Tab::Conversation,
                        },
                    )
                }),
        );
        if space.archived_projects().next().is_some() {
            entries.push(SidebarEntry::Archived(space));
        }
        entries.push(SidebarEntry::Label("Manage"));
        entries.extend(
            [
                (
                    "agents",
                    AppIcon::Bot,
                    "Team",
                    Screen::Agents(Some(space.name.clone())),
                ),
                (
                    "routines",
                    AppIcon::CalendarClock,
                    "Routines",
                    Screen::Routines(Some(space.name.clone())),
                ),
                (
                    "settings",
                    AppIcon::Settings,
                    "Settings",
                    Screen::Settings(Some(space.name.clone())),
                ),
            ]
            .map(|(id, icon, label, screen)| {
                row(
                    format!("{id}-{}", space.name),
                    icon,
                    label.into(),
                    None,
                    false,
                    screen,
                )
            }),
        );
    }
    entries.push(SidebarEntry::App);
    entries.push(row(
        "add-space-row".into(),
        AppIcon::Plus,
        "Add space".into(),
        None,
        false,
        Screen::AddSpace,
    ));
    let machines = state
        .machine_count()
        .filter(|&(_, all)| all != 0)
        .map(|(online, all)| format!("{online} / {all}").into());
    entries.extend(
        [
            (
                "go-machines",
                AppIcon::Monitor,
                "Machines",
                machines,
                Screen::Machines,
            ),
            (
                "go-settings",
                AppIcon::Settings,
                "App settings",
                None,
                Screen::Settings(None),
            ),
        ]
        .map(|(id, icon, label, count, screen)| {
            row(id.into(), icon, label.into(), count, false, screen)
        }),
    );
    entries
}

impl Desktop {
    /// Opens the nth actionable row regardless of the sidebar's visibility.
    pub(crate) fn sidebar_screen(&self, index: usize) -> Option<Screen> {
        sidebar_entries(&self.state)
            .into_iter()
            .filter_map(|entry| match entry {
                SidebarEntry::Row(row) => Some(row.screen),
                _ => None,
            })
            .nth(index)
    }

    pub(crate) fn sidebar(&self, when: &When, cx: &mut Context<Self>) -> impl IntoElement {
        let mut bar = v_flex()
            .w(SIDEBAR_WIDTH)
            .flex_none()
            .h_full()
            .gap(XS)
            .p(SM)
            .pt(LG)
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .overflow_y_scrollbar()
            .id("sidebar")
            .child(
                h_flex()
                    .h(BAR_HEIGHT)
                    .px(MD)
                    .text_size(TYPE_EMPHASIS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Threadlane"),
            );
        let mut group = v_flex().gap(XS);
        let mut index = 0;
        for entry in sidebar_entries(&self.state) {
            match entry {
                SidebarEntry::Row(NavigationRow {
                    id,
                    icon,
                    label,
                    count,
                    running,
                    screen,
                }) => {
                    let active = match (&self.screen, &screen) {
                        (
                            Screen::Project { space, project, .. },
                            Screen::Project {
                                space: row_space,
                                project: row_project,
                                ..
                            },
                        ) => space == row_space && project == row_project,
                        _ => self.screen == screen,
                    };
                    let command = match screen {
                        Screen::Home => shortcut_hint(cx, &GoHome),
                        Screen::Settings(None) => shortcut_hint(cx, &OpenSettings),
                        Screen::AddSpace => shortcut_hint(cx, &OpenAddSpace),
                        _ => None,
                    };
                    let shortcuts: Vec<_> = command
                        .into_iter()
                        .chain(shortcut_hint(cx, &SidebarShortcut(index)))
                        .collect();
                    index += 1;
                    let mut button = row(id, icon, label.clone(), count, running, active, cx);
                    if !shortcuts.is_empty() {
                        button = button.tooltip(format!("{label}  {}", shortcuts.join(" / ")));
                    }
                    let open = screen.clone();
                    let button = button
                        .on_click(cx.listener(move |this, _, _, cx| this.go(open.clone(), cx)));
                    group = if let Screen::Project { space, project, .. } = &screen
                        && self.state.space(space).and_then(Space::main_chat_project)
                            != Some(project)
                    {
                        let paused = self.state.is_paused(space, project);
                        let offers_archive = self.state.offers_archive(space, project);
                        let offers_delete = self.state.offers_delete(space, project);
                        let (space, project) = (space.clone(), project.clone());
                        let view = cx.entity().downgrade();
                        group.child(button.context_menu(move |menu, _, cx| {
                            let open = screen.clone();
                            let (pause_space, pause_name, copy) =
                                (space.clone(), project.clone(), project.clone());
                            let mut menu = menu
                                .item(menu_item(&view, "Open", move |this, _, cx| {
                                    this.go(open.clone(), cx)
                                }))
                                .item(menu_item(
                                    &view,
                                    if paused {
                                        "Resume project"
                                    } else {
                                        "Pause project"
                                    },
                                    move |this, _, cx| {
                                        this.set_paused(
                                            pause_space.clone(),
                                            pause_name.clone(),
                                            !paused,
                                            cx,
                                        );
                                    },
                                ))
                                .item(menu_item(&view, "Copy name", move |this, _, cx| {
                                    this.copy(copy.clone(), cx)
                                }));
                            if offers_archive {
                                let action =
                                    Consequence::ArchiveProject(space.clone(), project.clone());
                                menu = menu.item(menu_item(
                                    &view,
                                    "Archive project…",
                                    move |this, window, cx| {
                                        this.confirm_action(action.clone(), window, cx)
                                    },
                                ));
                            }
                            if offers_delete {
                                menu = menu.item(delete_project_item(
                                    &view,
                                    space.clone(),
                                    project.clone(),
                                    cx,
                                ));
                            }
                            menu
                        }))
                    } else {
                        group.child(button)
                    };
                }
                SidebarEntry::Space(space) => {
                    bar = bar.child(group);
                    bar = bar.child(h_flex().px(MD).pt(MD).min_w_0().children(
                        self.state.connection(&space.name, when).map(|connection| {
                            Self::connection_indicator(&space.name, connection, cx)
                        }),
                    ));
                    group = v_flex()
                        .gap(XS)
                        .ml(MD)
                        .pl(SM)
                        .border_l_1()
                        .border_color(cx.theme().sidebar_border);
                }
                SidebarEntry::Projects(space) => {
                    let screen = Screen::NewProject {
                        space: space.name.clone(),
                    };
                    group = group.child(
                        h_flex()
                            .justify_between()
                            .pl(MD)
                            .pt(SM)
                            .child(meta("Projects", cx))
                            .child(
                                icon_button(
                                    SharedString::from(format!("new-project-{}", space.name)),
                                    AppIcon::Plus,
                                    format!("New project in {}", space.name),
                                )
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                                ),
                            ),
                    );
                    if let Some(gap) = space.gap(Part::Projects) {
                        group = group.child(gap_line(&gap, "projects", &space.name, cx));
                    } else if space.active_projects().next().is_none() {
                        group = group.child(empty(EmptyRegion::Projects, cx));
                    }
                }
                SidebarEntry::Label(label) => {
                    group = group.child(meta(label, cx).px(MD).pt(SM));
                }
                SidebarEntry::App => {
                    bar = bar
                        .child(group)
                        .child(meta("Application", cx).px(MD).pt(MD));
                    group = v_flex().gap(XS);
                }
                SidebarEntry::Archived(space) => {
                    let name = space.name.clone();
                    let open = space.archived_open();
                    group = group.child(
                        row(
                            SharedString::from(format!("archived-{name}")),
                            if open {
                                AppIcon::ChevronDown
                            } else {
                                AppIcon::ChevronRight
                            },
                            "Archived",
                            None,
                            false,
                            false,
                            cx,
                        )
                        .tab_stop(true)
                        .accessibility_label(if open {
                            "Collapse archived projects"
                        } else {
                            "Expand archived projects"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.toggle_archived(&name);
                            cx.notify();
                        })),
                    );
                    if open {
                        for project in space.archived_projects() {
                            let active = matches!(&self.screen, Screen::Project { space: shown, project: name, .. }
                                if shown == &space.name && name == &project.name);
                            let screen = Screen::Project {
                                space: space.name.clone(),
                                project: project.name.clone(),
                                tab: Tab::Conversation,
                            };
                            let view = cx.entity().downgrade();
                            let (at, name) = (space.name.clone(), project.name.clone());
                            group = group.child(
                                div().pl(MD).child(
                                    row(
                                        SharedString::from(format!(
                                            "archived-project-{}-{}",
                                            space.name, project.id
                                        )),
                                        AppIcon::Folder,
                                        project.name.clone(),
                                        None,
                                        false,
                                        active,
                                        cx,
                                    )
                                    .tab_stop(true)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.go(screen.clone(), cx)
                                    }))
                                    .context_menu(
                                        move |menu, _, cx| {
                                            menu.item(delete_project_item(
                                                &view,
                                                at.clone(),
                                                name.clone(),
                                                cx,
                                            ))
                                        },
                                    ),
                                ),
                            );
                        }
                    }
                }
            }
        }
        bar.child(group)
    }

    fn connection_indicator(space: &str, connection: Connection, cx: &App) -> Div {
        let status = match connection.status {
            ConnectionStatus::Live => Status::Done,
            ConnectionStatus::Reconnecting => Status::Waiting,
            ConnectionStatus::Unreachable => Status::Failed,
        };
        let heading = h_flex().gap(XS).min_w_0().child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(space.to_owned()),
        );
        if connection.status == ConnectionStatus::Live {
            heading.child(
                h_flex()
                    .id(SharedString::from(format!("connected-{space}")))
                    .p(XS)
                    .role(Role::Status)
                    .aria_label(format!("{space} connected"))
                    .child(status_dot(status, cx))
                    .tooltip(|window, cx| Tooltip::new("Connected").build(window, cx)),
            )
        } else {
            heading
                .child(status_dot(status, cx))
                .child(meta(connection.label, cx))
        }
    }

    pub(crate) fn page_connections(&self, when: &When, cx: &App) -> Option<Div> {
        let relevant = |name: &str| match &self.screen {
            Screen::Home => self.state.filter().is_none_or(|space| space == name),
            Screen::Project { space, .. }
            | Screen::Task { space, .. }
            | Screen::NewProject { space }
            | Screen::Machine { space, .. } => space == name,
            _ => true,
        };
        let warnings: Vec<_> = self
            .state
            .spaces()
            .iter()
            .filter(|space| relevant(&space.name))
            .filter_map(|space| {
                let warning = self.state.connection_warning(&space.name, when)?;
                Some(Self::connection_indicator(&space.name, warning, cx))
            })
            .collect();
        (!warnings.is_empty()).then(|| h_flex().gap(MD).flex_wrap().children(warnings))
    }

    /// The last action and where it stands, until the next one.
    pub(crate) fn activity(&self, cx: &App) -> Option<Div> {
        let activity = self.state.activity()?;
        if let Activity::Failed(why) = activity {
            return Some(
                crate::components::empty_state(
                    "The last action could not be completed.",
                    Some(why),
                    cx,
                )
                .flex_none(),
            );
        }
        let status = match activity {
            Activity::Working(_) => Status::Running,
            Activity::Done(_) => Status::Done,
            Activity::Failed(_) => Status::Failed,
        };
        Some(
            v_flex()
                .flex_none()
                .gap(SM)
                .child(Separator::horizontal())
                .child(
                    h_flex()
                        .gap(SM)
                        .child(status_dot(status, cx))
                        .child(meta(activity.text().to_owned(), cx)),
                ),
        )
    }
}

fn positive_count(count: usize) -> Option<SharedString> {
    (count > 0).then(|| count.to_string().into())
}

#[cfg(test)]
mod tests {
    use threadlane_core::client::ProjectSummary;
    use threadlane_core::ids::ProjectId;
    use threadlane_desktop::AppState;

    use super::{SidebarEntry, sidebar_entries};
    use crate::{Screen, Tab};

    #[test]
    fn a_space_lists_chat_and_tasks_first_then_projects_then_manage() {
        let mut state = AppState::paired(["dev"]);
        for (id, name, is_general) in [(1, "api", false), (2, "General", true)] {
            state.project_created(
                "dev",
                ProjectSummary {
                    id: ProjectId(id),
                    name: name.to_owned(),
                    is_general,
                    paused: false,
                    mode_ceiling: None,
                    archived: false,
                },
            );
        }
        let labels: Vec<_> = sidebar_entries(&state)
            .into_iter()
            .map(|entry| match entry {
                SidebarEntry::Row(row) => row.label,
                SidebarEntry::Space(space) => format!("[{}]", space.name),
                SidebarEntry::Projects(_) => "PROJECTS +".to_owned(),
                SidebarEntry::Label(label) => label.to_uppercase(),
                SidebarEntry::App => "APPLICATION".to_owned(),
                SidebarEntry::Archived(_) => "Archived".to_owned(),
            })
            .collect();
        assert_eq!(
            labels,
            [
                "Inbox",
                "[dev]",
                "Chat",
                "Tasks",
                "PROJECTS +",
                "api",
                "MANAGE",
                "Team",
                "Routines",
                "Settings",
                "APPLICATION",
                "Add space",
                "Machines",
                "App settings",
            ]
        );
        let Some(SidebarEntry::Row(api)) = sidebar_entries(&state).into_iter().nth(5) else {
            panic!("the api row");
        };
        assert!(matches!(
            api.screen,
            Screen::Project {
                tab: Tab::Conversation,
                ..
            }
        ));
        assert!(!api.running);
    }
}
