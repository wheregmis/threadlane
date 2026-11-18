//! Composer queue presentation. Hosts own message identity, mutations and acknowledgements.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};
use threadlane_ui_theme::CHAT_CONTENT_MAX_WIDTH;

/// A bounded queue above the composer; hosts omit it when the queue is empty.
pub fn queued_message_panel(
    id: impl Into<SharedString>,
    count: usize,
    rows: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    div()
        .debug_selector(|| "queued-messages-panel".into())
        .w_full()
        .min_w_0()
        .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
        .mx_auto()
        .mb_2()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
        .child(
            div()
                .px_3()
                .py_2()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .text_color(theme.foreground)
                        .child(format!("Queued · {count}")),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("Sends after this turn"),
                ),
        )
        .child(
            div()
                .id(id.into())
                .role(Role::List)
                .aria_label("Queued messages")
                .max_h(rems(10.0))
                .overflow_y_scrollbar()
                .children(rows),
        )
}

/// Pending removals retain their text and replace actions until the host confirms removal.
pub fn queued_message_row(
    id: &str,
    text: impl Into<SharedString>,
    removing: bool,
    actions: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    let text = text.into();
    div()
        .id(SharedString::from(format!("queued-message-{id}")))
        // A group keeps each action exposed to native accessibility clients.
        .role(Role::Group)
        .aria_label(text.clone())
        .debug_selector(|| "queued-message-row".into())
        .w_full()
        .min_w_0()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .debug_selector(|| "queued-message-body".into())
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(theme.foreground)
                .child(text),
        )
        .when(removing, |row| {
            row.child(
                div()
                    .debug_selector(|| "queued-message-removing".into())
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Removing…"),
            )
        })
        .when(!removing, |row| {
            row.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(actions),
            )
        })
}

pub fn queued_steer_button(id: &str, enabled: bool, hint: impl Into<SharedString>) -> Button {
    let hint = hint.into();
    Button::new(format!("queued-steer-{id}"))
        .debug_selector(|| "queued-steer".into())
        .icon(IconName::ArrowRight)
        .accessibility_label(if enabled {
            "Steer current turn with this message".into()
        } else {
            hint.clone()
        })
        .xsmall()
        .ghost()
        .disabled(!enabled)
        .tooltip(hint)
}

pub fn queued_edit_button(id: &str) -> Button {
    Button::new(format!("queued-edit-{id}"))
        .debug_selector(|| "queued-edit".into())
        .icon(Icon::default().path("icons/square-pen.svg"))
        .accessibility_label("Edit queued message in the composer")
        .xsmall()
        .ghost()
        .tooltip("Edit message in the composer")
}

pub fn queued_remove_button(id: &str) -> Button {
    Button::new(format!("queued-remove-{id}"))
        .debug_selector(|| "queued-remove".into())
        .icon(IconName::Close)
        .accessibility_label("Remove queued message")
        .xsmall()
        .ghost()
        .tooltip("Remove from queue")
}

/// A staged follow-up before the host commits it to the queue or current turn.
pub fn pending_message_row(
    text: impl Into<SharedString>,
    actions: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    let text = text.into();
    div()
        .id("pending-message")
        .role(Role::Group)
        .aria_label(format!("Pending message: {text}"))
        .debug_selector(|| "pending-preview-row".into())
        .w_full()
        .min_w_0()
        .max_w(rems(CHAT_CONTENT_MAX_WIDTH))
        .mx_auto()
        .mb_2()
        .min_h(rems(3.25))
        .px_3()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .child(
            Tag::new()
                .child("Pending")
                .with_variant(TagVariant::Secondary)
                .small(),
        )
        .child(
            div()
                .debug_selector(|| "pending-message-body".into())
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(theme.foreground)
                .child(text),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_3()
                .children(actions),
        )
}

pub fn pending_queue_button() -> Button {
    Button::new("queue-pending-message")
        .debug_selector(|| "pending-queue".into())
        .icon(IconName::Plus)
        .accessibility_label("Queue pending message after the current response")
        .xsmall()
        .secondary()
        .tooltip("Queue after the current response")
}

pub fn pending_steer_button(enabled: bool, hint: impl Into<SharedString>) -> Button {
    let hint = hint.into();
    Button::new("steer-pending-message")
        .debug_selector(|| "pending-steer".into())
        .icon(IconName::ArrowRight)
        .accessibility_label(if enabled {
            "Steer with pending message".into()
        } else {
            hint.clone()
        })
        .xsmall()
        .primary()
        .disabled(!enabled)
        .tooltip(hint)
}

pub fn pending_edit_button() -> Button {
    Button::new("dismiss-pending-message")
        .debug_selector(|| "pending-dismiss".into())
        .icon(IconName::Undo2)
        .accessibility_label("Edit pending message in the composer")
        .xsmall()
        .ghost()
        .tooltip("Edit message in the composer")
}
