//! Controlled terminal output chrome. Hosts own clipboard, PTY and parser operations.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TerminalOutputAction {
    CopySelection,
    FontSize(f32),
    ToggleCompact,
    ToggleBackground,
    Find,
    CopyOutput,
    Paste,
    SelectAll,
    Clear,
    Restart,
}

/// Output commands and checked appearance choices; link commands may precede this menu.
pub struct TerminalOutputMenu {
    font_size: f32,
    compact: bool,
    blend: bool,
    selection: bool,
    paste: bool,
}

impl TerminalOutputMenu {
    pub fn new(font_size: f32) -> Self {
        Self {
            font_size,
            compact: false,
            blend: false,
            selection: false,
            paste: true,
        }
    }
    pub fn compact(mut self, enabled: bool) -> Self {
        self.compact = enabled;
        self
    }
    pub fn blend(mut self, enabled: bool) -> Self {
        self.blend = enabled;
        self
    }
    pub fn selection(mut self, present: bool) -> Self {
        self.selection = present;
        self
    }
    pub fn paste(mut self, enabled: bool) -> Self {
        self.paste = enabled;
        self
    }

    pub fn render(
        self,
        mut menu: PopupMenu,
        on_request: impl Fn(TerminalOutputAction, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut App,
    ) -> PopupMenu {
        // The pinned wrapper otherwise transfers focus during prepaint, after
        // the terminal has published its focused accessibility node.
        menu.focus_handle(cx).focus(window, cx);
        let callback = Rc::new(on_request);
        if self.selection {
            let callback = callback.clone();
            menu = menu.item(PopupMenuItem::new("Copy selection").on_click(
                move |_, window, cx| {
                    callback(TerminalOutputAction::CopySelection, window, cx);
                },
            ));
        }
        for (label, size) in [
            ("Small", 11.),
            ("Medium", super::TERMINAL_FONT_SIZE),
            ("Large", 16.),
        ] {
            let callback = callback.clone();
            menu = menu.item(
                PopupMenuItem::new(format!("Font size: {label}"))
                    .checked(self.font_size == size)
                    .on_click(move |_, window, cx| {
                        callback(TerminalOutputAction::FontSize(size), window, cx)
                    }),
            );
        }
        for (label, checked, action) in [
            (
                "Compact lines",
                self.compact,
                TerminalOutputAction::ToggleCompact,
            ),
            (
                "Blend background",
                self.blend,
                TerminalOutputAction::ToggleBackground,
            ),
        ] {
            let callback = callback.clone();
            menu = menu.item(
                PopupMenuItem::new(label)
                    .checked(checked)
                    .on_click(move |_, window, cx| callback(action, window, cx)),
            );
        }
        menu = menu.separator();
        for (label, action, enabled) in [
            ("Find in terminal output…", TerminalOutputAction::Find, true),
            (
                "Copy terminal output",
                TerminalOutputAction::CopyOutput,
                true,
            ),
            ("Paste", TerminalOutputAction::Paste, self.paste),
            ("Select all", TerminalOutputAction::SelectAll, true),
            ("Clear terminal", TerminalOutputAction::Clear, true),
            ("Restart shell", TerminalOutputAction::Restart, true),
        ] {
            let callback = callback.clone();
            menu = menu.item(PopupMenuItem::new(label).disabled(!enabled).on_click(
                move |_, window, cx| {
                    if enabled {
                        callback(action, window, cx);
                    }
                },
            ));
        }
        menu
    }
}

pub fn terminal_output_surface(id: impl Into<ElementId>, blend: bool, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Terminal)
        .size_full()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .bg(cx.theme().background.opacity(if blend { 0.92 } else { 1. }))
        .rounded_md()
        .border_1()
        .border_color(transparent_black())
        .focus(|style| style.border_color(cx.theme().ring))
}

pub fn terminal_status(
    message: impl Into<SharedString>,
    is_error: bool,
    on_restart: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let color = if is_error {
        cx.theme().danger
    } else {
        cx.theme().warning
    };
    let message = message.into();
    div()
        .id("terminal-status")
        .debug_selector(|| "terminal-status".into())
        .role(if is_error { Role::Alert } else { Role::Status })
        .aria_label(message.clone())
        .flex()
        .flex_none()
        .min_w_0()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .bg(color.opacity(0.1))
        .border_1()
        .border_color(color.opacity(0.3))
        .rounded_md()
        .mt_2()
        .mb_2()
        .mx_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Icon::new(if is_error {
                        IconName::CircleX
                    } else {
                        IconName::Info
                    })
                    .xsmall()
                    .text_color(color),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .child(message),
                ),
        )
        .child(
            Button::new("terminal-restart-banner-btn")
                .debug_selector(|| "terminal-restart-banner-btn".into())
                .label("Restart shell")
                .icon(Icon::default().path("icons/rotate-cw.svg"))
                .ghost()
                .xsmall()
                .on_click(move |_, window, cx| on_restart(window, cx)),
        )
}

pub fn terminal_live_output(
    lines_up: usize,
    on_jump: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    div()
        .absolute()
        .bottom_7()
        .right_3()
        .rounded_full()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().title_bar)
        .shadow_md()
        .child(
            Button::new("terminal-autoscroll-pill")
                .label(format!("Live output · {lines_up} lines up"))
                .accessibility_label(format!("Jump to live terminal output, {lines_up} lines up"))
                .icon(IconName::ChevronDown)
                .tooltip("Jump to live output")
                .xsmall()
                .ghost()
                .on_click(move |_, window, cx| on_jump(window, cx)),
        )
}
