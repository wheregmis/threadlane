//! Quick Switcher / Command Palette (`Cmd+P`), built on GPUI Kit's `Command`.

use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{Icon, WindowExt as _};
use gpui_kit::*;
use threadlane_desktop::AppState;

use crate::icons::AppIcon;
use crate::menus::{
    DarkAppearance, LightAppearance, OpenShortcutsMenu, Reload, SystemAppearance, ToggleSidebar,
};
use crate::tokens::FORM_WIDTH;
use crate::{Desktop, Screen, Tab, WorkPage};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    Go(Screen),
    OpenSkills(String),
    SwitchSpace(String),
    Reload,
    ToggleSidebar,
    ShowShortcuts,
    SetAppearance(threadlane_desktop::Appearance),
}

pub(crate) struct PaletteEntry {
    pub(crate) label: String,
    pub(crate) keywords: Vec<String>,
    pub(crate) icon: AppIcon,
    pub(crate) action: PaletteAction,
}

pub(crate) struct PaletteSection {
    pub(crate) heading: &'static str,
    pub(crate) entries: Vec<PaletteEntry>,
}

/// Builds the grouped entries for the Quick Switcher from the current `AppState`.
pub(crate) fn build_sections(state: &AppState) -> Vec<PaletteSection> {
    let multi_space = state.spaces().len() > 1;
    let mut sections = Vec::new();

    // 1. Navigate
    let mut nav = vec![
        PaletteEntry {
            label: "Inbox".into(),
            keywords: vec!["home".into(), "decisions".into(), "needs you".into()],
            icon: AppIcon::Inbox,
            action: PaletteAction::Go(Screen::Home),
        },
        PaletteEntry {
            label: "Machines".into(),
            keywords: vec!["runners".into(), "limits".into(), "accounts".into()],
            icon: AppIcon::Monitor,
            action: PaletteAction::Go(Screen::Machines),
        },
        PaletteEntry {
            label: "App settings".into(),
            keywords: vec!["preferences".into(), "notifications".into()],
            icon: AppIcon::Settings,
            action: PaletteAction::Go(Screen::Settings(None)),
        },
        PaletteEntry {
            label: "Add space…".into(),
            keywords: vec!["pair".into(), "hub".into(), "join".into()],
            icon: AppIcon::Plus,
            action: PaletteAction::Go(Screen::AddSpace),
        },
    ];
    if let Some(space) = state.selected_space() {
        let name = space.name.clone();
        if let Some(chat) = space.main_chat_project() {
            nav.push(PaletteEntry {
                label: if multi_space {
                    format!("Chat ({name})")
                } else {
                    "Chat".into()
                },
                keywords: vec![name.clone(), "space lead".into(), "conversation".into()],
                icon: AppIcon::MessagesSquare,
                action: PaletteAction::Go(Screen::Project {
                    space: name.clone(),
                    project: chat.to_owned(),
                    tab: Tab::Conversation,
                }),
            });
        }
        nav.push(PaletteEntry {
            label: if multi_space {
                format!("Tasks ({name})")
            } else {
                "Tasks".into()
            },
            keywords: vec![name.clone(), "board".into(), "graph".into()],
            icon: AppIcon::Columns3,
            action: PaletteAction::Go(Screen::SpaceTasks {
                space: name.clone(),
            }),
        });
        nav.push(PaletteEntry {
            label: if multi_space {
                format!("Team ({name})")
            } else {
                "Team".into()
            },
            keywords: vec![name.clone(), "agents".into(), "topology".into()],
            icon: AppIcon::Bot,
            action: PaletteAction::Go(Screen::Agents(Some(name.clone()))),
        });
        nav.push(PaletteEntry {
            label: if multi_space {
                format!("Skills ({name})")
            } else {
                "Skills".into()
            },
            keywords: vec![
                name.clone(),
                "playbooks".into(),
                "skills".into(),
                "templates".into(),
            ],
            icon: AppIcon::BookOpen,
            action: PaletteAction::OpenSkills(name.clone()),
        });
        nav.push(PaletteEntry {
            label: if multi_space {
                format!("Routines ({name})")
            } else {
                "Routines".into()
            },
            keywords: vec![name.clone(), "timers".into(), "schedule".into()],
            icon: AppIcon::CalendarClock,
            action: PaletteAction::Go(Screen::Routines(Some(name.clone()))),
        });
        nav.push(PaletteEntry {
            label: if multi_space {
                format!("Usage ({name})")
            } else {
                "Usage".into()
            },
            keywords: vec![
                name.clone(),
                "tokens".into(),
                "cost".into(),
                "metrics".into(),
            ],
            icon: AppIcon::ChartColumn,
            action: PaletteAction::Go(Screen::Work {
                space: name.clone(),
                page: WorkPage::Usage(30),
            }),
        });
        nav.push(PaletteEntry {
            label: format!("Space settings ({name})"),
            keywords: vec![name.clone(), "ceiling".into(), "pacing".into()],
            icon: AppIcon::Settings,
            action: PaletteAction::Go(Screen::Settings(Some(name.clone()))),
        });
        nav.push(PaletteEntry {
            label: format!("New project in {name}…"),
            keywords: vec![name.clone(), "create project".into()],
            icon: AppIcon::FolderPlus,
            action: PaletteAction::Go(Screen::NewProject { space: name }),
        });
    }
    sections.push(PaletteSection {
        heading: "Navigate",
        entries: nav,
    });

    // 2. Projects across all paired spaces
    let mut projects = Vec::new();
    for space in state.spaces() {
        for project in space.active_projects().filter(|p| !p.is_general) {
            let label = if multi_space {
                format!("{} / {}", space.name, project.name)
            } else {
                project.name.clone()
            };
            projects.push(PaletteEntry {
                label,
                keywords: vec![space.name.clone(), project.name.clone(), "project".into()],
                icon: AppIcon::Folder,
                action: PaletteAction::Go(Screen::Project {
                    space: space.name.clone(),
                    project: project.name.clone(),
                    tab: Tab::Conversation,
                }),
            });
        }
    }
    if !projects.is_empty() {
        sections.push(PaletteSection {
            heading: "Projects",
            entries: projects,
        });
    }

    // 3. Tasks across all paired spaces
    let mut tasks = Vec::new();
    for space in state.spaces() {
        for summary in &space.tasks {
            let id = summary.task.id.to_string();
            let label = if multi_space {
                format!(
                    "{id} {} ({} / {})",
                    summary.task.title, space.name, summary.project
                )
            } else {
                format!("{id} {} ({})", summary.task.title, summary.project)
            };
            tasks.push(PaletteEntry {
                label,
                keywords: vec![
                    id,
                    space.name.clone(),
                    summary.project.clone(),
                    summary.agent.clone(),
                    "task".into(),
                ],
                icon: AppIcon::FileText,
                action: PaletteAction::Go(Screen::Task {
                    space: space.name.clone(),
                    task: summary.task.id,
                }),
            });
        }
    }
    if !tasks.is_empty() {
        sections.push(PaletteSection {
            heading: "Tasks",
            entries: tasks,
        });
    }

    // 4. Spaces (only when switching is meaningful)
    if multi_space {
        let selected = state.selected_space().map(|s| s.name.as_str());
        let spaces = state
            .spaces()
            .iter()
            .filter(|s| Some(s.name.as_str()) != selected)
            .map(|space| PaletteEntry {
                label: format!("Switch to space {}", space.name),
                keywords: vec![space.name.clone(), "space".into(), "switch".into()],
                icon: AppIcon::GitFork,
                action: PaletteAction::SwitchSpace(space.name.clone()),
            })
            .collect::<Vec<_>>();
        if !spaces.is_empty() {
            sections.push(PaletteSection {
                heading: "Spaces",
                entries: spaces,
            });
        }
    }

    // 5. Actions
    sections.push(PaletteSection {
        heading: "Actions",
        entries: vec![
            PaletteEntry {
                label: "Reload".into(),
                keywords: vec!["refresh".into(), "sync".into()],
                icon: AppIcon::Play,
                action: PaletteAction::Reload,
            },
            PaletteEntry {
                label: "Toggle sidebar".into(),
                keywords: vec!["collapse".into(), "expand".into(), "navigation".into()],
                icon: AppIcon::PanelLeft,
                action: PaletteAction::ToggleSidebar,
            },
            PaletteEntry {
                label: "Keyboard shortcuts".into(),
                keywords: vec!["keys".into(), "help".into()],
                icon: AppIcon::KeyRound,
                action: PaletteAction::ShowShortcuts,
            },
            PaletteEntry {
                label: "Appearance: System".into(),
                keywords: vec!["theme".into(), "auto".into()],
                icon: AppIcon::Monitor,
                action: PaletteAction::SetAppearance(threadlane_desktop::Appearance::System),
            },
            PaletteEntry {
                label: "Appearance: Light".into(),
                keywords: vec!["theme".into(), "day".into()],
                icon: AppIcon::Monitor,
                action: PaletteAction::SetAppearance(threadlane_desktop::Appearance::Light),
            },
            PaletteEntry {
                label: "Appearance: Dark".into(),
                keywords: vec!["theme".into(), "night".into()],
                icon: AppIcon::Monitor,
                action: PaletteAction::SetAppearance(threadlane_desktop::Appearance::Dark),
            },
        ],
    });

    sections
}

impl Desktop {
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let sections = build_sections(&self.state);
        let actions: Vec<Vec<PaletteAction>> = sections
            .iter()
            .map(|section| section.entries.iter().map(|e| e.action.clone()).collect())
            .collect();
        let command_state = cx.new(|cx| CommandState::new(window, cx));
        let focus = command_state.focus_handle(cx);
        let view = cx.entity().downgrade();

        let groups: Vec<CommandGroup> = sections
            .into_iter()
            .map(|section| {
                let mut group = CommandGroup::new().label(section.heading);
                for entry in section.entries {
                    group = group.item(
                        CommandItem::new()
                            .label(entry.label)
                            .icon(Icon::new(entry.icon))
                            .keywords(entry.keywords),
                    );
                }
                group
            })
            .collect();

        window.open_dialog(cx, move |dialog, _, _| {
            let actions = actions.clone();
            let view = view.clone();
            let mut palette = Command::new(&command_state)
                .bordered(false)
                .placeholder("Jump to a project, task, space, or command…")
                .max_h(rems(22.))
                .on_confirm(move |path, window, cx| {
                    let chosen = actions
                        .get(path.section)
                        .and_then(|section| section.get(path.row))
                        .cloned();
                    window.close_dialog(cx);
                    let Some(action) = chosen else { return };
                    match action {
                        PaletteAction::Go(screen) => {
                            let _ = view.update(cx, |this, cx| this.go(screen, cx));
                        }
                        PaletteAction::OpenSkills(space) => {
                            let _ = view.update(cx, |this, cx| this.open_skill(space, None, cx));
                        }
                        PaletteAction::SwitchSpace(space) => {
                            let _ = view.update(cx, |this, cx| {
                                this.state.select_space(&space);
                                match crate::screens::shell::switch_destination(
                                    &this.state,
                                    &this.screen,
                                    &space,
                                ) {
                                    Some(screen) => this.go(screen, cx),
                                    None => cx.notify(),
                                }
                            });
                        }
                        PaletteAction::Reload => {
                            window.dispatch_action(Box::new(Reload), cx);
                        }
                        PaletteAction::ToggleSidebar => {
                            window.dispatch_action(Box::new(ToggleSidebar), cx);
                        }
                        PaletteAction::ShowShortcuts => {
                            window.dispatch_action(Box::new(OpenShortcutsMenu), cx);
                        }
                        PaletteAction::SetAppearance(appearance) => {
                            let action: Box<dyn Action> = match appearance {
                                threadlane_desktop::Appearance::System => {
                                    Box::new(SystemAppearance)
                                }
                                threadlane_desktop::Appearance::Light => Box::new(LightAppearance),
                                threadlane_desktop::Appearance::Dark => Box::new(DarkAppearance),
                            };
                            window.dispatch_action(action, cx);
                        }
                    }
                });
            for group in groups.clone() {
                palette = palette.group(group);
            }
            dialog
                .width(FORM_WIDTH)
                .close_button(false)
                .p_0()
                .child(palette)
        });

        window.on_next_frame(move |window, cx| {
            focus.focus(window, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use threadlane_core::client::ProjectSummary;
    use threadlane_core::ids::ProjectId;
    use threadlane_desktop::AppState;

    use super::{PaletteAction, build_sections};
    use crate::{Screen, Tab};

    #[test]
    fn palette_includes_projects_and_space_switch_for_multiple_spaces() {
        let mut state = AppState::paired(["dev", "ops"]);
        state.project_created(
            "dev",
            ProjectSummary {
                id: ProjectId(1),
                name: "api".into(),
                is_general: false,
                paused: false,
                mode_ceiling: None,
                archived: false,
            },
        );
        state.project_created(
            "ops",
            ProjectSummary {
                id: ProjectId(2),
                name: "infra".into(),
                is_general: false,
                paused: false,
                mode_ceiling: None,
                archived: false,
            },
        );

        let sections = build_sections(&state);
        let headings: Vec<&str> = sections.iter().map(|s| s.heading).collect();
        assert!(headings.contains(&"Navigate"));
        assert!(headings.contains(&"Projects"));
        assert!(headings.contains(&"Spaces"));
        assert!(headings.contains(&"Actions"));

        let projects = sections.iter().find(|s| s.heading == "Projects").unwrap();
        assert!(projects.entries.iter().any(|e| {
            e.label == "dev / api"
                && e.action
                    == PaletteAction::Go(Screen::Project {
                        space: "dev".into(),
                        project: "api".into(),
                        tab: Tab::Conversation,
                    })
        }));

        let spaces = sections.iter().find(|s| s.heading == "Spaces").unwrap();
        assert_eq!(spaces.entries.len(), 1);
        assert_eq!(
            spaces.entries[0].action,
            PaletteAction::SwitchSpace("ops".into())
        );
    }
}
