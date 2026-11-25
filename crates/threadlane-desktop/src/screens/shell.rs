//! Navigation, space reachability, and the last action.

use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_desktop::{
    Activity, AppState, Connection, ConnectionStatus, Consequence, EmptyRegion, Part, Space, When,
};

use super::{delete_project::delete_project_item, empty, gap_line, menu_item};
use crate::components::{button, icon_button, meta, row, status_badge, status_dot};
use crate::icons::AppIcon;
use crate::menus::{
    GoHome, NavigateBack, NavigateForward, OpenPalette, OpenSettings, SidebarShortcut,
    ToggleSidebar, shortcut_hint,
};
use crate::tokens::{BADGE_RADIUS, BAR_HEIGHT, LG, MD, ROW_HEIGHT, SM, Status, TYPE_EMPHASIS, XS};
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
    /// The "Projects" label and its new-project button.
    Projects(&'a Space),
    Label(&'static str),
    Archived(&'a Space),
}

/// Inbox, then the selected space's rows: its Chat and Tasks, its
/// projects, and what manages it.
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
    let Some(space) = state.selected_space() else {
        return entries;
    };
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
                "usage",
                AppIcon::ChartColumn,
                "Usage",
                Screen::Work {
                    space: space.name.clone(),
                    page: crate::WorkPage::Usage(30),
                },
            ),
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
    entries
}

/// Selects the space `screen` belongs to, so the sidebar always matches the page.
pub(crate) fn follow_screen(state: &mut AppState, screen: &Screen) {
    if let Some(space) = screen.space() {
        state.select_space(space);
    }
}

/// Where the page goes when the switcher picks `space`: a page of another
/// space would disagree with the sidebar, so it moves to the new space's Chat
/// (or its Tasks). App-wide pages and pages of `space` itself stay.
pub(crate) fn switch_destination(
    state: &AppState,
    current: &Screen,
    space: &str,
) -> Option<Screen> {
    if current.space().is_none_or(|current| current == space) {
        return None;
    }
    let chat = state.space(space).and_then(Space::main_chat_project);
    Some(match chat {
        Some(project) => Screen::Project {
            space: space.to_owned(),
            project: project.to_owned(),
            tab: Tab::Conversation,
        },
        None => Screen::SpaceTasks {
            space: space.to_owned(),
        },
    })
}

/// The app icon's mark, shown beside the name at the top of the sidebar.
fn logo() -> std::sync::Arc<Image> {
    static LOGO: std::sync::LazyLock<std::sync::Arc<Image>> = std::sync::LazyLock::new(|| {
        std::sync::Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../../resources/logo.png").to_vec(),
        ))
    });
    std::sync::Arc::clone(&LOGO)
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

    fn nav_controls(&self, show_expand: bool, cx: &mut Context<Self>) -> Div {
        let with_hint = |label: &str, action: &dyn Action| {
            shortcut_hint(cx, action)
                .map_or_else(|| label.to_owned(), |hint| format!("{label}  {hint}"))
        };
        let back = icon_button(
            "nav-back",
            AppIcon::ArrowLeft,
            with_hint("Back", &NavigateBack),
        )
        .disabled(!self.nav_history.can_go_back())
        .on_click(cx.listener(|this, _, _, cx| this.go_back(cx)));
        let forward = icon_button(
            "nav-forward",
            AppIcon::ArrowRight,
            with_hint("Forward", &NavigateForward),
        )
        .disabled(!self.nav_history.can_go_forward())
        .on_click(cx.listener(|this, _, _, cx| this.go_forward(cx)));
        let search = icon_button(
            "open-palette",
            AppIcon::Search,
            with_hint("Quick switcher…", &OpenPalette),
        )
        .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)));

        let mut strip = h_flex().gap(XS).items_center();
        if show_expand {
            strip = strip.child(
                icon_button(
                    "expand-sidebar",
                    AppIcon::PanelLeftOpen,
                    with_hint("Show sidebar", &ToggleSidebar),
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            );
        }
        strip.child(back).child(forward).child(search)
    }

    /// Quiet navigation strip shown in the main workspace when the sidebar is collapsed.
    pub(crate) fn collapsed_nav_strip(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .flex_none()
            .gap(SM)
            .items_center()
            .child(self.nav_controls(true, cx))
    }

    pub(crate) fn sidebar(&self, when: &When, cx: &mut Context<Self>) -> impl IntoElement {
        let collapse_tip = shortcut_hint(cx, &ToggleSidebar).map_or_else(
            || "Hide sidebar".to_owned(),
            |hint| format!("Hide sidebar  {hint}"),
        );
        let bar = v_flex()
            .w_full()
            .min_w_0()
            .h_full()
            .gap(XS)
            .p(SM)
            .pt(LG)
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                h_flex()
                    .h(BAR_HEIGHT)
                    .pl(MD)
                    .pr(XS)
                    .gap(SM)
                    .justify_between()
                    .child(
                        h_flex()
                            .gap(SM)
                            .min_w_0()
                            .text_size(TYPE_EMPHASIS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(img(logo()).size_5().rounded(BADGE_RADIUS).flex_none())
                            .child(div().min_w_0().truncate().child("Threadlane")),
                    )
                    .child(
                        icon_button("collapse-sidebar", AppIcon::PanelLeftClose, collapse_tip)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                    ),
            )
            .child(
                h_flex()
                    .px(XS)
                    .justify_between()
                    .items_center()
                    .child(self.nav_controls(false, cx)),
            )
            .child(self.space_switcher(when, cx))
            .children(
                self.state
                    .selected_space()
                    .filter(|space| self.state.is_space_paused(&space.name))
                    .map(|_| meta("Paused: no new run starts until you resume it", cx).px(MD)),
            )
            .children(
                self.state
                    .selected_space()
                    .and_then(|space| self.state.connection_warning(&space.name, when))
                    .map(|warning| meta(warning.label, cx).px(MD)),
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
        bar.child(
            div()
                .id("sidebar")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(group),
        )
        .child(self.sidebar_footer(cx))
    }

    /// The selected space, and a menu to switch spaces, open the space's
    /// settings or pair another.
    fn space_switcher(&self, when: &When, cx: &mut Context<Self>) -> AnyElement {
        let Some(selected) = self.state.selected_space() else {
            return row(
                "space-switcher",
                AppIcon::Plus,
                "Add a space",
                None,
                false,
                self.screen == Screen::AddSpace,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| this.go(Screen::AddSpace, cx)))
            .into_any_element();
        };
        let status = match self
            .state
            .connection(&selected.name, when)
            .map(|c| c.status)
        {
            Some(ConnectionStatus::Live) => Status::Done,
            Some(ConnectionStatus::Unreachable) => Status::Failed,
            _ => Status::Waiting,
        };
        let spaces: Vec<(String, bool)> = self
            .state
            .spaces()
            .iter()
            .map(|space| (space.name.clone(), space.name == selected.name))
            .collect();
        let name = selected.name.clone();
        let paused = self.state.is_space_paused(&name);
        let view = cx.entity().downgrade();
        let style = ButtonCustomVariant::new(cx)
            .foreground(cx.theme().sidebar_foreground)
            .hover(cx.theme().muted)
            .active(cx.theme().sidebar_accent);
        button("space-switcher")
            .custom(style)
            .w_full()
            .h(ROW_HEIGHT)
            .tab_stop(true)
            .accessibility_label(if paused {
                format!("Space: {name}, paused — switch spaces")
            } else {
                format!("Space: {name} — switch spaces")
            })
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(SM)
                    .child(status_dot(status, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name.clone()),
                    )
                    .children(paused.then(|| status_badge("Paused", Status::Waiting, cx)))
                    .child(Icon::new(AppIcon::ChevronDown).small()),
            )
            .dropdown_menu(move |mut menu, _, _| {
                for (space, checked) in &spaces {
                    let (view, space) = (view.clone(), space.clone());
                    menu = menu.item(
                        PopupMenuItem::new(space.clone())
                            .checked(*checked)
                            .on_click(move |_, _, cx| {
                                let _gone = view.update(cx, |this, cx| {
                                    this.state.select_space(&space);
                                    match switch_destination(&this.state, &this.screen, &space) {
                                        Some(screen) => this.go(screen, cx),
                                        None => cx.notify(),
                                    }
                                });
                            }),
                    );
                }
                let settings = Screen::Settings(Some(name.clone()));
                let pause = name.clone();
                menu.separator()
                    .item(menu_item(
                        &view,
                        if paused {
                            "Resume space"
                        } else {
                            "Pause space"
                        },
                        move |this, _, cx| this.set_space_paused(pause.clone(), !paused, cx),
                    ))
                    .item(menu_item(&view, "Space settings", move |this, _, cx| {
                        this.go(settings.clone(), cx)
                    }))
                    .item(menu_item(&view, "Add space…", |this, _, cx| {
                        this.go(Screen::AddSpace, cx)
                    }))
            })
            .into_any_element()
    }

    /// Machines and app settings, pinned below the scrolling list.
    fn sidebar_footer(&self, cx: &mut Context<Self>) -> Div {
        let running = self.state.local_running_tasks();
        let machine_label = running.map_or_else(
            || "Machines and task limits".to_owned(),
            |running| format!("Machines · {running} running on this computer"),
        );
        let style = ButtonCustomVariant::new(cx)
            .foreground(cx.theme().sidebar_foreground)
            .hover(cx.theme().muted)
            .active(cx.theme().sidebar_accent);
        let mut settings = icon_button("go-settings", AppIcon::Settings, "App settings".to_owned())
            .selected(self.screen == Screen::Settings(None));
        if let Some(hint) = shortcut_hint(cx, &OpenSettings) {
            settings = settings.tooltip(format!("App settings  {hint}"));
        }
        h_flex()
            .flex_none()
            .gap(XS)
            .pt(SM)
            .border_t_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                div().flex_1().min_w_0().child(
                    button("go-machines")
                        .custom(style)
                        .rounded(cx.theme().radius)
                        .w_full()
                        .h_auto()
                        .min_h(ROW_HEIGHT)
                        .py_1()
                        .selected(matches!(
                            self.screen,
                            Screen::Machines | Screen::Machine { .. }
                        ))
                        .accessibility_label(machine_label.clone())
                        .tooltip(machine_label)
                        .child(
                            h_flex()
                                .w_full()
                                .gap(SM)
                                .child(Icon::new(AppIcon::Monitor).small())
                                .child(
                                    v_flex().items_start().min_w_0().child("Machines").children(
                                        running
                                            .map(|running| meta(format!("{running} running"), cx)),
                                    ),
                                ),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Machines, cx))),
                ),
            )
            .child(
                settings
                    .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Settings(None), cx))),
            )
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

    /// Shows a compact status line only while an operation is actively in flight.
    /// Completed and failed results surface as non-blocking notifications.
    pub(crate) fn activity(&self, cx: &App) -> Option<Div> {
        let Activity::Working(text) = self.state.activity()? else {
            return None;
        };
        Some(
            v_flex()
                .flex_none()
                .gap(SM)
                .child(Separator::horizontal())
                .child(
                    h_flex()
                        .gap(SM)
                        .child(status_dot(Status::Running, cx))
                        .child(meta(text.clone(), cx)),
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

    use super::{SidebarEntry, follow_screen, sidebar_entries, switch_destination};
    use crate::{Screen, Tab};

    fn project(state: &mut AppState, space: &str, id: i64, name: &str, is_general: bool) {
        state.project_created(
            space,
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

    fn labels(state: &AppState) -> Vec<String> {
        sidebar_entries(state)
            .into_iter()
            .map(|entry| match entry {
                SidebarEntry::Row(row) => row.label,
                SidebarEntry::Projects(space) => format!("PROJECTS + [{}]", space.name),
                SidebarEntry::Label(label) => label.to_uppercase(),
                SidebarEntry::Archived(_) => "Archived".to_owned(),
            })
            .collect()
    }

    #[test]
    fn a_space_lists_chat_and_tasks_first_then_projects_then_manage() {
        let mut state = AppState::paired(["dev"]);
        project(&mut state, "dev", 1, "api", false);
        project(&mut state, "dev", 2, "General", true);
        assert_eq!(
            labels(&state),
            [
                "Inbox",
                "Chat",
                "Tasks",
                "PROJECTS + [dev]",
                "api",
                "MANAGE",
                "Usage",
                "Team",
                "Routines",
            ]
        );
        let Some(SidebarEntry::Row(api)) = sidebar_entries(&state).into_iter().nth(4) else {
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

    #[test]
    fn only_the_selected_space_is_listed_and_the_page_selects_its_space() {
        let mut state = AppState::paired(["dev", "ops"]);
        project(&mut state, "dev", 1, "api", false);
        project(&mut state, "ops", 2, "infra", false);
        assert_eq!(
            state.selected_space().map(|space| space.name.as_str()),
            Some("dev")
        );
        assert!(labels(&state).contains(&"api".to_owned()));
        assert!(!labels(&state).contains(&"infra".to_owned()));

        // A task link from the Inbox, a notification or THREADLANE_DESKTOP_OPEN.
        follow_screen(
            &mut state,
            &Screen::Task {
                space: "ops".into(),
                task: threadlane_core::ids::TaskId(1),
            },
        );
        assert_eq!(
            state.selected_space().map(|space| space.name.as_str()),
            Some("ops")
        );
        assert!(labels(&state).contains(&"infra".to_owned()));
        assert!(!labels(&state).contains(&"api".to_owned()));

        // Switching spaces moves a page of the old space to the new space's Chat.
        let dev_tasks = Screen::SpaceTasks {
            space: "dev".into(),
        };
        match switch_destination(&state, &dev_tasks, "ops") {
            Some(Screen::Project { space, tab, .. }) => {
                assert_eq!((space.as_str(), tab), ("ops", Tab::Conversation));
            }
            Some(Screen::SpaceTasks { space }) => assert_eq!(space, "ops"),
            other => panic!("expected a page of ops, got {other:?}"),
        }
        assert_eq!(switch_destination(&state, &Screen::Home, "ops"), None);
        let ops_tasks = Screen::SpaceTasks {
            space: "ops".into(),
        };
        assert_eq!(switch_destination(&state, &ops_tasks, "ops"), None);

        // Inbox and app-wide screens keep the selection; an unknown space is ignored.
        for screen in [
            Screen::Home,
            Screen::Machines,
            Screen::Settings(None),
            Screen::SpaceTasks {
                space: "gone".into(),
            },
        ] {
            follow_screen(&mut state, &screen);
            assert_eq!(
                state.selected_space().map(|space| space.name.as_str()),
                Some("ops")
            );
        }
    }

    #[test]
    fn with_no_space_paired_only_inbox_is_listed() {
        assert_eq!(labels(&AppState::default()), ["Inbox"]);
    }
}
