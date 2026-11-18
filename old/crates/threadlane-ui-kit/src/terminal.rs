//! Terminal chrome shared by native and web. Hosts retain PTYs, selection and close guards.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalTabAction {
    Select,
    Close,
    CloseOthers,
    Restart,
    NewTab,
}

/// A shell tab with separate selection and close controls. IDs belong to shell identity.
pub struct TerminalTab {
    id: SharedString,
    label: SharedString,
    hint: SharedString,
    selected: bool,
    close_armed: bool,
    project_actions: bool,
    close_others: bool,
}

impl TerminalTab {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        hint: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            hint: hint.into(),
            selected: false,
            close_armed: false,
            project_actions: true,
            close_others: false,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn close_armed(mut self, armed: bool) -> Self {
        self.close_armed = armed;
        self
    }
    pub fn project_actions(mut self, enabled: bool) -> Self {
        self.project_actions = enabled;
        self
    }
    pub fn close_others(mut self, enabled: bool) -> Self {
        self.close_others = enabled;
        self
    }

    pub fn render(
        self,
        on_request: impl Fn(TerminalTabAction, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let callback = Rc::new(on_request);
        let select = callback.clone();
        let close = callback.clone();
        let menu = callback.clone();
        let close_hint = if self.close_armed {
            format!("{} holds output — activate again to close it", self.hint)
        } else {
            format!("Close {}", self.hint)
        };
        div()
            .id(self.id.clone())
            .role(Role::Group)
            .flex_none()
            .flex()
            .items_center()
            .gap_0p5()
            .child(
                Button::new(SharedString::from(format!("{}-select", self.id)))
                    .debug_selector({
                        let id = self.id.clone();
                        move || format!("{id}-select").into()
                    })
                    .label(self.label)
                    .icon(IconName::SquareTerminal)
                    .ghost()
                    .xsmall()
                    .selected(self.selected)
                    .accessibility_label(format!(
                        "{}{}",
                        self.hint,
                        if self.selected { ", selected" } else { "" }
                    ))
                    .tooltip(self.hint)
                    .on_click(move |_, window, cx| select(TerminalTabAction::Select, window, cx))
                    .context_menu(move |popup, window, cx| {
                        // The pinned context-menu wrapper otherwise focuses during
                        // prepaint, after the terminal has published its focused node.
                        popup.focus_handle(cx).focus(window, cx);
                        terminal_tab_menu(popup, self.project_actions, self.close_others, {
                            let menu = menu.clone();
                            move |action, window, cx| menu(action, window, cx)
                        })
                    }),
            )
            .child(
                Button::new(SharedString::from(format!("{}-close", self.id)))
                    .debug_selector({
                        let id = self.id.clone();
                        move || format!("{id}-close").into()
                    })
                    .ghost()
                    .xsmall()
                    .disabled(!self.project_actions)
                    .accessibility_label(close_hint.clone())
                    .tooltip(close_hint)
                    .when(self.close_armed, |button| button.label("Sure?").danger())
                    .when(!self.close_armed, |button| button.icon(IconName::Close))
                    .on_click(move |_, window, cx| {
                        if self.project_actions {
                            close(TerminalTabAction::Close, window, cx);
                        }
                    }),
            )
    }
}

pub fn terminal_tab_menu(
    menu: PopupMenu,
    project_actions: bool,
    close_others: bool,
    on_request: impl Fn(TerminalTabAction, &mut Window, &mut App) + 'static,
) -> PopupMenu {
    let callback = Rc::new(on_request);
    [
        ("Close shell", TerminalTabAction::Close, project_actions),
        (
            "Close other tabs",
            TerminalTabAction::CloseOthers,
            project_actions && close_others,
        ),
        ("Restart shell", TerminalTabAction::Restart, true),
        (
            "New terminal tab",
            TerminalTabAction::NewTab,
            project_actions,
        ),
    ]
    .into_iter()
    .fold(menu, |menu, (label, action, enabled)| {
        let callback = callback.clone();
        menu.item(
            PopupMenuItem::new(label)
                .disabled(!enabled)
                .on_click(move |_, window, cx| {
                    if enabled {
                        callback(action, window, cx);
                    }
                }),
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalAction {
    NewTab,
    Clear,
    Restart,
    Find,
    OpenLinks,
    AddSelectionToChat,
    Hide,
    ToggleMaximize,
    RecreateWorktree,
    UseProjectFolder,
}

pub fn terminal_surface(cx: &App) -> Div {
    div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(cx.theme().background)
        .border_t_1()
        .border_color(cx.theme().border)
}

fn terminal_bar(cx: &App) -> Div {
    div()
        .w_full()
        .min_h(rems(2.125))
        .flex_none()
        .flex()
        .items_center()
        .px_2()
        .gap_2()
        .bg(cx.theme().title_bar)
        .border_b_1()
        .border_color(cx.theme().title_bar_border)
}

fn action_button(id: &'static str, hint: impl Into<SharedString>) -> Button {
    let hint = hint.into();
    Button::new(id)
        .debug_selector(move || id.into())
        .ghost()
        .small()
        .accessibility_label(hint.clone())
        .tooltip(hint)
}

/// Controlled toolbar. Only the tab strip scrolls; actions stay reachable at narrow widths.
pub struct TerminalToolbar {
    project: SharedString,
    project_hint: Option<SharedString>,
    worktree: bool,
    tabs: Vec<AnyElement>,
    new_tab: bool,
    selection_enabled: bool,
    selection_hint: SharedString,
    find_hint: SharedString,
    hide_hint: SharedString,
    maximized: Option<bool>,
    window_controls_inset: Option<Pixels>,
}

impl TerminalToolbar {
    pub fn new(project: impl Into<SharedString>) -> Self {
        Self {
            project: project.into(),
            project_hint: None,
            worktree: false,
            tabs: Vec::new(),
            new_tab: true,
            selection_enabled: false,
            selection_hint: "Select terminal output first".into(),
            find_hint: "Find in terminal output".into(),
            hide_hint: "Hide terminal".into(),
            maximized: None,
            window_controls_inset: None,
        }
    }
    /// Full shell working directory for the project chip's tooltip and
    /// accessibility label, when the chip label is a shortened basename.
    pub fn path_hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.project_hint = Some(hint.into());
        self
    }
    /// Marks the visible shell as running inside an isolated worktree.
    pub fn worktree(mut self, worktree: bool) -> Self {
        self.worktree = worktree;
        self
    }
    pub fn tabs(mut self, tabs: Vec<AnyElement>) -> Self {
        self.tabs = tabs;
        self
    }
    pub fn new_tab(mut self, enabled: bool) -> Self {
        self.new_tab = enabled;
        self
    }
    /// Hosts opt in when the terminal can fill the workspace without ending its shell.
    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = Some(maximized);
        self
    }
    /// Reserve native window chrome when this toolbar occupies the title row.
    pub fn window_controls_inset(mut self, inset: Option<Pixels>) -> Self {
        self.window_controls_inset = inset;
        self
    }
    pub fn selection(mut self, enabled: bool, hint: impl Into<SharedString>) -> Self {
        self.selection_enabled = enabled;
        self.selection_hint = hint.into();
        self
    }
    pub fn shortcuts(
        mut self,
        find: impl Into<SharedString>,
        hide: impl Into<SharedString>,
    ) -> Self {
        self.find_hint = find.into();
        self.hide_hint = hide.into();
        self
    }
    pub fn render(
        self,
        on_request: impl Fn(TerminalAction, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> Div {
        let callback = Rc::new(on_request);
        let new_tab = callback.clone();
        let hide = callback.clone();
        let maximize = callback.clone();
        let theme = cx.theme().colors;
        let project_hint = self
            .project_hint
            .clone()
            .unwrap_or_else(|| self.project.clone());
        let header = terminal_bar(cx)
            .when_some(self.window_controls_inset, |bar, inset| {
                bar.pl(inset)
                    .min_h(threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE)
            })
            .child(
                div()
                    .id("terminal-project")
                    .role(Role::Group)
                    .aria_label(format!("Terminal working directory: {project_hint}"))
                    .flex_none()
                    .max_w(rems(12.0))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(theme.secondary)
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(project_hint.clone())
                            .build(window, cx)
                    })
                    .child(
                        Icon::new(IconName::SquareTerminal)
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.project),
                    ),
            )
            .children(self.worktree.then(|| {
                div()
                    .id("terminal-worktree-badge")
                    .debug_selector(|| "terminal-worktree-badge".into())
                    .flex_none()
                    .px_1p5()
                    .py(rems(0.125))
                    .rounded_full()
                    .bg(theme.muted.opacity(0.35))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(
                            "This shell runs inside the session's isolated worktree",
                        )
                        .build(window, cx)
                    })
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Worktree"),
                    )
            }))
            .child(div().w(px(1.0)).h_4().bg(theme.border))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .overflow_x_scrollbar()
                    .children(self.tabs),
            )
            .children(self.new_tab.then(|| {
                action_button("terminal-new-tab", "New terminal tab")
                    .icon(IconName::Plus)
                    .on_click(move |_, window, cx| new_tab(TerminalAction::NewTab, window, cx))
            }))
            .children(self.maximized.map(|maximized| {
                action_button(
                    "terminal-maximize-btn",
                    if maximized {
                        "Restore terminal size"
                    } else {
                        "Maximize terminal"
                    },
                )
                .icon(if maximized {
                    IconName::Minimize
                } else {
                    IconName::Maximize
                })
                .selected(maximized)
                .on_click(move |_, window, cx| maximize(TerminalAction::ToggleMaximize, window, cx))
            }))
            .child(
                action_button("terminal-close-panel-btn", self.hide_hint)
                    .icon(IconName::Close)
                    .on_click(move |_, window, cx| hide(TerminalAction::Hide, window, cx)),
            );
        let buttons = [
            (
                "terminal-clear-btn",
                "Clear terminal",
                // Not undo/redo arrows: these actions erase output and restart the shell.
                Icon::default().path("icons/eraser.svg"),
                None,
                TerminalAction::Clear,
            ),
            (
                "terminal-restart-btn",
                "Restart shell",
                Icon::default().path("icons/rotate-cw.svg"),
                None,
                TerminalAction::Restart,
            ),
            (
                "terminal-find-btn",
                self.find_hint.as_ref(),
                Icon::new(IconName::Search),
                Some("Find"),
                TerminalAction::Find,
            ),
            (
                "terminal-open-link",
                "Open link… — Links in visible output",
                Icon::new(IconName::ExternalLink),
                Some("Links…"),
                TerminalAction::OpenLinks,
            ),
        ]
        .into_iter()
        .map(|(id, hint, icon, label, action)| {
            let callback = callback.clone();
            action_button(id, hint.to_owned())
                .icon(icon)
                .when_some(label, |button, label| button.label(label))
                .on_click(move |_, window, cx| callback(action, window, cx))
        })
        .collect::<Vec<_>>();
        let selection_hint = format!("Add selection to chat — {}", self.selection_hint);
        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .child(header)
            .child(
                terminal_bar(cx)
                    .flex_wrap()
                    .py_1()
                    .gap_1()
                    .children(buttons)
                    .child(
                        action_button("terminal-add-selection-to-chat", selection_hint)
                            .icon(Icon::default().path("icons/square-pen.svg"))
                            .label("Add to chat")
                            .disabled(!self.selection_enabled)
                            .on_click(move |_, window, cx| {
                                if self.selection_enabled {
                                    callback(TerminalAction::AddSelectionToChat, window, cx);
                                }
                            }),
                    ),
            )
    }
}

pub fn terminal_unavailable(
    on_request: impl Fn(TerminalAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let callback = Rc::new(on_request);
    let hide = callback.clone();
    let recreate = callback.clone();
    terminal_surface(cx)
        .child(
            terminal_bar(cx)
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Terminal"),
                )
                .child(div().flex_1())
                .child(
                    action_button("terminal-unavailable-close", "Hide terminal")
                        .icon(IconName::Close)
                        .on_click(move |_, window, cx| hide(TerminalAction::Hide, window, cx)),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_4()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Worktree unavailable"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("This session's worktree is not checked out"),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap_2()
                        .child(
                            Button::new("terminal-recreate-worktree")
                                .debug_selector(|| "terminal-recreate-worktree".into())
                                .label("Recreate worktree")
                                .small()
                                .on_click(move |_, window, cx| {
                                    recreate(TerminalAction::RecreateWorktree, window, cx)
                                }),
                        )
                        .child(
                            Button::new("terminal-use-project-folder")
                                .debug_selector(|| "terminal-use-project-folder".into())
                                .label("Use project folder")
                                .small()
                                .ghost()
                                .on_click(move |_, window, cx| {
                                    callback(TerminalAction::UseProjectFolder, window, cx)
                                }),
                        ),
                ),
        )
}
