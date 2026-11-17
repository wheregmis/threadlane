//! Controlled conversation find presentation; hosts own scanning and selection.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Disableable, IconName, Sizable};
use std::rc::Rc;

actions!(
    threadlane_chat_find,
    [
        FindInConversation,
        CloseConversationFind,
        NextConversationMatch,
        PreviousConversationMatch
    ]
);

pub fn init_conversation_find(cx: &mut App) {
    let shortcut = if cfg!(target_os = "macos") {
        "cmd-f"
    } else {
        "ctrl-f"
    };
    #[cfg(target_family = "wasm")]
    cx.bind_keys([KeyBinding::new(
        "cmd-f",
        FindInConversation,
        Some("Conversation"),
    )]);
    cx.bind_keys([
        KeyBinding::new(shortcut, FindInConversation, Some("Conversation")),
        KeyBinding::new(
            "escape",
            CloseConversationFind,
            Some("ConversationFindActive"),
        ),
        KeyBinding::new(
            "enter",
            NextConversationMatch,
            Some("ConversationFind > Input"),
        ),
        KeyBinding::new(
            "shift-enter",
            PreviousConversationMatch,
            Some("ConversationFind > Input"),
        ),
    ]);
}

/// Shared header command; the host decides which conversation is searchable.
pub fn conversation_find_button() -> Button {
    let hint = if cfg!(target_os = "macos") {
        "Find in conversation (⌘F)"
    } else if cfg!(target_family = "wasm") {
        "Find in conversation (Ctrl+F / ⌘F)"
    } else {
        "Find in conversation (Ctrl+F)"
    };
    Button::new("conversation-find-open")
        .debug_selector(|| "conversation-find-open".into())
        .icon(IconName::Search)
        .ghost()
        .small()
        .accessibility_label(hint)
        .tooltip(hint)
}

pub fn conversation_find_status(
    query: &str,
    pending: bool,
    count: usize,
    selected: Option<usize>,
) -> String {
    if query.trim().is_empty() {
        "Type to find a message".into()
    } else if pending && count == 0 {
        "Searching…".into()
    } else if count == 0 {
        "No matching messages".into()
    } else if let Some(index) = selected.filter(|index| *index < count) {
        format!("{} of {count} matching messages", index + 1)
    } else {
        format!("{count} matching messages · Choose Previous or Next")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationFindAction {
    Previous,
    Next,
    Close,
}

pub struct ConversationFindStrip {
    input: Entity<InputState>,
    status: SharedString,
    can_navigate: bool,
    leading_inset: Option<Pixels>,
    excerpt: Option<SharedString>,
}
impl ConversationFindStrip {
    pub fn new(
        input: &Entity<InputState>,
        status: impl Into<SharedString>,
        can_navigate: bool,
    ) -> Self {
        Self {
            input: input.clone(),
            status: status.into(),
            can_navigate,
            leading_inset: None,
            excerpt: None,
        }
    }
    pub fn leading_inset(mut self, inset: Pixels) -> Self {
        self.leading_inset = Some(inset);
        self
    }
    pub fn excerpt(mut self, excerpt: Option<SharedString>) -> Self {
        self.excerpt = excerpt;
        self
    }
    pub fn render(
        self,
        on_request: impl Fn(&ConversationFindAction, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> Stateful<Div> {
        let callback = Rc::new(on_request);
        let action = |id: &'static str,
                      label: &'static str,
                      hint: &'static str,
                      request: ConversationFindAction,
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
                        callback(&request, window, cx);
                    }
                })
        };
        div()
            .id("conversation-find-strip")
            .debug_selector(|| "conversation-find-strip".into())
            .key_context("ConversationFind")
            .flex_none()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .when_some(self.leading_inset, |el, inset| el.pl(inset))
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
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
                                .aria_label("Find in conversation"),
                        ),
                    )
                    .child(action(
                        "conversation-find-previous",
                        "Previous",
                        "Previous matching message (Shift+Enter)",
                        ConversationFindAction::Previous,
                        self.can_navigate,
                    ))
                    .child(action(
                        "conversation-find-next",
                        "Next",
                        "Next matching message (Enter)",
                        ConversationFindAction::Next,
                        self.can_navigate,
                    ))
                    .child(action(
                        "conversation-find-close",
                        "Close",
                        "Close find in conversation (Escape)",
                        ConversationFindAction::Close,
                        true,
                    )),
            )
            .child(
                div()
                    .id("conversation-find-status")
                    .role(Role::Status)
                    .aria_label(self.status.clone())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status),
            )
            .children(self.excerpt.map(|excerpt| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Selected message: {excerpt}"))
            }))
    }
}

/// Row geometry and selection treatment shared by every conversation host.
pub fn conversation_transcript_row(selected_label: Option<&'static str>, cx: &App) -> Div {
    div()
        .w_full()
        .max_w(rems(threadlane_ui_theme::CHAT_CONTENT_MAX_WIDTH))
        .mx_auto()
        .when_some(selected_label, |el, label| {
            el.border_1()
                .border_color(cx.theme().primary)
                .child(div().text_sm().child(label))
        })
}
