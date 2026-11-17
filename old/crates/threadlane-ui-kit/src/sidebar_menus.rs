//! Controlled sidebar commands. Hosts retain session services and durable guards.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarSessionAction {
    Open,
    RegenerateTitle,
    Fork,
    TogglePin,
    Snooze(u64),
    Unsnooze,
    RetrySnooze,
    OpenTerminal,
    CopyId,
    CopyProjectPath,
    CopySessionFile,
    ExportLog,
    ExportTrajectory,
    Archive,
    Remove,
}

#[derive(Clone, Copy)]
pub enum SidebarSessionMenuScope {
    Quick,
    Full,
}

/// A host-resolved return time. The duration is dispatched at activation;
/// the menu never creates a deadline or writes a snooze record.
#[derive(Clone)]
pub struct SidebarSnoozeChoice {
    label: String,
    duration_secs: u64,
    return_label: String,
}
impl SidebarSnoozeChoice {
    pub fn new(label: impl Into<String>, duration_secs: u64) -> Self {
        Self {
            label: label.into(),
            duration_secs,
            return_label: String::new(),
        }
    }
    pub fn with_return_label(mut self, label: impl Into<String>) -> Self {
        self.return_label = label.into();
        self
    }
    pub fn return_label(&self) -> &str {
        &self.return_label
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn duration_secs(&self) -> u64 {
        self.duration_secs
    }
}

/// A confirmed or pending presentation state, shared by rows and menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarSnoozeStatus {
    Saving,
    SaveFailed,
    Snoozed(String),
}
impl SidebarSnoozeStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Saving => "Saving snooze…".into(),
            Self::SaveFailed => "Couldn't save snooze".into(),
            Self::Snoozed(until) => format!("Snoozed until {until}"),
        }
    }
    pub fn pending(&self) -> bool {
        !matches!(self, Self::Snoozed(_))
    }
}

#[derive(Clone)]
pub enum SidebarSnoozeMenu {
    Available(Vec<SidebarSnoozeChoice>),
    Unavailable(String),
    Status(SidebarSnoozeStatus),
}

pub struct SidebarSessionMenuState {
    pinned: bool,
    title_generating: bool,
    title_loading: bool,
    terminal_available: bool,
    snooze: SidebarSnoozeMenu,
}
impl SidebarSessionMenuState {
    pub fn new(snooze: SidebarSnoozeMenu) -> Self {
        Self {
            pinned: false,
            title_generating: false,
            title_loading: false,
            terminal_available: true,
            snooze,
        }
    }
    pub fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }
    pub fn title_generating(mut self, generating: bool) -> Self {
        self.title_generating = generating;
        self
    }
    pub fn title_loading(mut self, loading: bool) -> Self {
        self.title_loading = loading;
        self
    }
    pub fn terminal_available(mut self, available: bool) -> Self {
        self.terminal_available = available;
        self
    }
}

/// The compact row menu and full object menu share commands, wording, enabled
/// states and snooze submenus. Services and confirmations belong to the host.
pub fn sidebar_session_menu(
    menu: PopupMenu,
    state: SidebarSessionMenuState,
    scope: SidebarSessionMenuScope,
    on_action: impl Fn(SidebarSessionAction, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    // Transfer focus before prepaint, including when opened from a focused input.
    menu.focus_handle(cx).focus(window, cx);
    let request = std::rc::Rc::new(on_action);
    let item_request = request.clone();
    let item = move |label: &str, action, disabled| {
        let request = item_request.clone();
        PopupMenuItem::new(label.to_owned())
            .disabled(disabled)
            .on_click(move |_, window, cx| request(action, window, cx))
    };
    let full = matches!(scope, SidebarSessionMenuScope::Full);
    let mut menu = menu
        .when(full, |menu| {
            menu.item(item(
                if state.title_generating {
                    "Generating title…"
                } else {
                    "Regenerate title"
                },
                SidebarSessionAction::RegenerateTitle,
                state.title_generating || state.title_loading,
            ))
        })
        .item(item("Open session", SidebarSessionAction::Open, false))
        .when(full, |menu| {
            menu.item(item("Fork session", SidebarSessionAction::Fork, false))
        })
        .item(item(
            if state.pinned {
                "Unpin session"
            } else {
                "Pin session"
            },
            SidebarSessionAction::TogglePin,
            false,
        ));
    menu = match state.snooze {
        SidebarSnoozeMenu::Available(choices) => {
            let request = request.clone();
            menu.submenu("Snooze session…", window, cx, move |menu, _, _| {
                choices.iter().fold(menu, |menu, choice| {
                    let request = request.clone();
                    let duration = choice.duration_secs;
                    menu.item(
                        PopupMenuItem::new(if choice.return_label.is_empty() {
                            choice.label.clone()
                        } else {
                            format!("{} — back at {}", choice.label, choice.return_label)
                        })
                        .on_click(move |_, window, cx| {
                            request(SidebarSessionAction::Snooze(duration), window, cx);
                        }),
                    )
                })
            })
        }
        SidebarSnoozeMenu::Unavailable(reason) => {
            menu.item(PopupMenuItem::new(format!("Snooze session… — {reason}")).disabled(true))
        }
        SidebarSnoozeMenu::Status(status) => {
            let failed = matches!(status, SidebarSnoozeStatus::SaveFailed);
            menu.item(PopupMenuItem::new(status.label()).disabled(true))
                .when(failed, |menu| {
                    menu.item(item(
                        "Retry saving snooze",
                        SidebarSessionAction::RetrySnooze,
                        false,
                    ))
                })
                .item(item(
                    "Unsnooze session",
                    SidebarSessionAction::Unsnooze,
                    false,
                ))
        }
    };
    if full {
        let copy_item = item.clone();
        let export_item = item.clone();
        menu = menu
            .separator()
            .item(item(
                if state.terminal_available {
                    "Open terminal here"
                } else {
                    "Open terminal here — worktree unavailable"
                },
                SidebarSessionAction::OpenTerminal,
                !state.terminal_available,
            ))
            .submenu("Copy…", window, cx, move |menu, _, _| {
                menu.item(copy_item("Session ID", SidebarSessionAction::CopyId, false))
                    .item(copy_item(
                        "Project root path",
                        SidebarSessionAction::CopyProjectPath,
                        false,
                    ))
                    .item(copy_item(
                        "Session file path",
                        SidebarSessionAction::CopySessionFile,
                        false,
                    ))
            })
            .submenu("Export…", window, cx, move |menu, _, _| {
                menu.item(export_item(
                    "Session log…",
                    SidebarSessionAction::ExportLog,
                    false,
                ))
                .item(export_item(
                    "Trajectory…",
                    SidebarSessionAction::ExportTrajectory,
                    false,
                ))
            });
    }
    menu.separator()
        .item(item(
            "Archive session",
            SidebarSessionAction::Archive,
            false,
        ))
        .separator()
        .item(item("Remove session", SidebarSessionAction::Remove, false))
}

pub fn session_pin_button(id: &str, pinned: bool, active: bool, cx: &App) -> Button {
    let id = format!("pin-session-{id}");
    let selector = id.clone();
    Button::new(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .icon(Icon::default().path("icons/pin.svg"))
        .ghost()
        .xsmall()
        .compact()
        .tab_stop(false)
        .accessibility_label(if pinned {
            "Unpin session"
        } else {
            "Pin session to top"
        })
        .tooltip(if pinned {
            "Unpin session"
        } else {
            "Pin session to top"
        })
        .text_color(if pinned {
            cx.theme().primary
        } else {
            cx.theme().muted_foreground
        })
        .opacity(if pinned || active { 1.0 } else { 0.0 })
        .group_hover("session-card", |style| style.opacity(1.0))
        .focus_visible(|style| style.opacity(1.0))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

pub fn session_archive_button(id: &str, active: bool) -> Button {
    let id = format!("settle-session-{id}");
    let selector = id.clone();
    Button::new(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .icon(Icon::default().path("icons/archive.svg"))
        .ghost()
        .xsmall()
        .accessibility_label("Archive session")
        .tooltip("Archive session")
        .opacity(if active { 1.0 } else { 0.0 })
        .group_hover("session-card", |style| style.opacity(1.0))
        .focus_visible(|style| style.opacity(1.0))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

pub fn session_actions_button(id: &str, active: bool) -> Button {
    let id = format!("session-actions-{id}");
    let selector = id.clone();
    // The popover receives mouse-down and stops it before the session row.
    Button::new(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .icon(IconName::Ellipsis)
        .ghost()
        .xsmall()
        .compact()
        .accessibility_label("Session actions")
        .tooltip("Session actions")
        .opacity(if active { 1.0 } else { 0.0 })
        .group_hover("session-card", |style| style.opacity(1.0))
        .focus_visible(|style| style.opacity(1.0))
}

pub fn sidebar_project_menu(
    menu: PopupMenu,
    projects: &[(String, PathBuf, usize)],
    selected: Option<&Path>,
    on_select: impl Fn(Option<PathBuf>, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    menu.focus_handle(cx).focus(window, cx);
    let request = std::rc::Rc::new(on_select);
    let all = request.clone();
    projects.iter().fold(
        menu.item(
            crate::sidebar_project_filter_item(
                "All projects",
                projects.iter().map(|(_, _, count)| count).sum(),
                selected.is_none(),
            )
            .on_click(move |_, window, cx| all(None, window, cx)),
        ),
        |menu, (label, path, count)| {
            let request = request.clone();
            let path = path.clone();
            menu.item(
                crate::sidebar_project_filter_item(label, *count, selected == Some(path.as_path()))
                    .on_click(move |_, window, cx| request(Some(path.clone()), window, cx)),
            )
        },
    )
}
