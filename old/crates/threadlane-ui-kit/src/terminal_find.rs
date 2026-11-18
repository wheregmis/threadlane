//! Controlled terminal search presentation. Hosts retain queries and search workers.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Disableable, Sizable};
use std::rc::Rc;

actions!(
    threadlane_terminal,
    [
        FindInTerminalOutput,
        CloseTerminalFind,
        NextTerminalMatch,
        PreviousTerminalMatch
    ]
);

pub fn init_terminal_find(cx: &mut App) {
    let shortcut = if cfg!(target_os = "macos") {
        "cmd-f"
    } else {
        "ctrl-shift-f"
    };
    cx.bind_keys([
        KeyBinding::new(shortcut, FindInTerminalOutput, Some("Terminal")),
        KeyBinding::new("escape", CloseTerminalFind, Some("TerminalFind")),
        KeyBinding::new("enter", NextTerminalMatch, Some("TerminalFind > Input")),
        KeyBinding::new(
            "shift-enter",
            PreviousTerminalMatch,
            Some("TerminalFind > Input"),
        ),
    ]);
}

#[derive(Clone, Debug)]
pub enum TerminalFindStatus {
    Empty,
    Searching,
    Failed,
    Unavailable,
    Results(TerminalFindResults),
}

#[derive(Clone, Debug)]
pub struct TerminalFindResults {
    total: usize,
    retained: usize,
    selected: Option<usize>,
}

impl TerminalFindStatus {
    pub fn results(total: usize, retained: usize, selected: Option<usize>) -> Self {
        let retained = retained.min(total);
        Self::Results(TerminalFindResults {
            total,
            retained,
            selected: selected.filter(|index| *index < retained),
        })
    }

    pub fn label(&self) -> String {
        match self {
            Self::Unavailable => "Find unavailable in full-screen terminal applications".into(),
            Self::Failed => "Couldn't search terminal output".into(),
            Self::Empty => "Type to find output".into(),
            Self::Searching => "Searching…".into(),
            Self::Results(results) if results.retained == 0 => {
                "No matching lines in retained output".into()
            }
            Self::Results(results) => match results.selected {
                Some(index) => format!(
                    "{} of {} matching lines",
                    results.total - results.retained + index + 1,
                    results.total
                ),
                None => format!("{} matching lines · Choose Previous or Next", results.total),
            },
        }
    }

    pub fn can_navigate(&self) -> bool {
        matches!(self, Self::Results(results) if results.retained > 0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalFindAction {
    Previous,
    Next,
    Retry,
    JumpToLive,
    Close,
}

pub struct TerminalFindStrip {
    input: Entity<InputState>,
    status: TerminalFindStatus,
    excerpt: Option<SharedString>,
    scrolled: bool,
}

impl TerminalFindStrip {
    pub fn new(input: &Entity<InputState>, status: TerminalFindStatus) -> Self {
        Self {
            input: input.clone(),
            status,
            excerpt: None,
            scrolled: false,
        }
    }
    pub fn excerpt(mut self, excerpt: Option<SharedString>) -> Self {
        self.excerpt = excerpt;
        self
    }
    pub fn scrolled(mut self, scrolled: bool) -> Self {
        self.scrolled = scrolled;
        self
    }

    pub fn render(
        self,
        on_request: impl Fn(TerminalFindAction, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let status = self.status.label();
        let nav_enabled = self.status.can_navigate();
        let callback = Rc::new(on_request);
        let action = |id: &'static str,
                      label: &'static str,
                      hint: &'static str,
                      request: TerminalFindAction,
                      enabled: bool| {
            let callback = callback.clone();
            Button::new(id)
                .debug_selector(move || id.into())
                .label(label)
                .small()
                .ghost()
                .accessibility_label(hint)
                .tooltip(hint)
                .disabled(!enabled)
                .on_click(move |_, window, cx| {
                    if enabled {
                        callback(request, window, cx);
                    }
                })
        };
        div()
            .id("terminal-find-strip")
            .key_context("TerminalFind")
            .flex_none()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1p5()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div().flex_1().min_w(rems(10.)).child(
                            Input::new(&self.input)
                                .small()
                                .aria_label("Find in terminal output"),
                        ),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .border_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Match case"),
                    )
                    .child(action(
                        "terminal-find-previous",
                        "Previous",
                        "Previous matching line (Shift+Enter)",
                        TerminalFindAction::Previous,
                        nav_enabled,
                    ))
                    .child(action(
                        "terminal-find-next",
                        "Next",
                        "Next matching line (Enter)",
                        TerminalFindAction::Next,
                        nav_enabled,
                    ))
                    .children(matches!(self.status, TerminalFindStatus::Failed).then(|| {
                        action(
                            "terminal-find-retry",
                            "Retry",
                            "Retry terminal output search",
                            TerminalFindAction::Retry,
                            true,
                        )
                    }))
                    .children(self.scrolled.then(|| {
                        action(
                            "terminal-find-jump-to-live",
                            "Jump to live output",
                            "Jump to live output",
                            TerminalFindAction::JumpToLive,
                            true,
                        )
                    }))
                    .child(action(
                        "terminal-find-close",
                        "Close",
                        "Close find in terminal output (Escape)",
                        TerminalFindAction::Close,
                        true,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("terminal-find-status")
                            .role(Role::Status)
                            .aria_label(status.clone())
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Retained output · current terminal"),
                    ),
            )
            .children(self.excerpt.map(|excerpt| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("Match: {excerpt}"))
            }))
    }
}

pub fn next_terminal_find_match(
    selected: Option<usize>,
    count: usize,
    previous: bool,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (selected, previous) {
        (Some(index), true) => (index + count - 1) % count,
        (Some(index), false) => (index + 1) % count,
        (None, true) => 0,
        (None, false) => count - 1,
    })
}
