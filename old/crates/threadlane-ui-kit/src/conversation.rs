//! Message actions and transcript chrome shared by desktop and captured previews.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};

pub const MESSAGE_COPY_FEEDBACK_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

pub fn message_actions(align_end: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .when(align_end, |el| el.justify_end())
}
pub(crate) fn copy_feedback_button(selector: String, copied: bool, cx: &App) -> Button {
    Button::new(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .icon(if copied {
            IconName::Check
        } else {
            IconName::Copy
        })
        .xsmall()
        .ghost()
        .when(copied, |button| button.text_color(cx.theme().success))
}
pub fn message_copy_button(message_id: &str, copied: bool, cx: &App) -> Button {
    copy_feedback_button(format!("message-copy-{message_id}"), copied, cx)
        .tooltip(if copied {
            "Copied!"
        } else {
            "Copy message to clipboard"
        })
        .accessibility_label(if copied {
            "Message copied"
        } else {
            "Copy message"
        })
}
/// Quote controls carry one of two states: enabled, or disabled with an
/// explanation readable without color (`reason` doubles as tooltip and
/// accessibility label).
pub fn message_quote_button(message_id: &str, enabled: bool, reason: SharedString) -> Button {
    let selector = format!("message-quote-{message_id}");
    Button::new(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .icon(Icon::default().path("icons/quote.svg"))
        .xsmall()
        .ghost()
        .disabled(!enabled)
        .tooltip(if enabled {
            "Quote the selected text into the composer"
        } else {
            reason.as_ref()
        })
        .accessibility_label(if enabled {
            "Quote selection"
        } else {
            reason.as_ref()
        })
}
/// Context-menu entry mirroring `message_quote_button`; disabled entries keep
/// a readable reason in the label so the menu explains itself.
pub fn message_quote_menu_item(
    enabled: bool,
    disabled_label: Option<SharedString>,
    on_quote: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> PopupMenuItem {
    let label = match (enabled, disabled_label) {
        (true, _) | (_, None) => SharedString::from("Quote selection"),
        (false, Some(reason)) => reason,
    };
    PopupMenuItem::new(label)
        .disabled(!enabled)
        .on_click(on_quote)
}
pub fn message_edit_button(message_id: &str) -> Button {
    let selector = format!("message-edit-{message_id}");
    Button::new(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .icon(Icon::default().path("icons/square-pen.svg"))
        .xsmall()
        .ghost()
        .tooltip("Load this message into the composer to edit and resend")
        .accessibility_label("Load this message into the composer to edit and resend")
}
pub fn message_context_menu(
    menu: PopupMenu,
    on_copy: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> PopupMenu {
    message_context_menu_with_quote(menu, on_copy, None)
}
/// Copy plus an optional host-supplied quote item (see
/// `message_quote_menu_item`), kept ahead of Copy.
pub fn message_context_menu_with_quote(
    menu: PopupMenu,
    on_copy: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    quote: Option<PopupMenuItem>,
) -> PopupMenu {
    menu.when_some(quote, |menu, item| menu.item(item))
        .item(PopupMenuItem::new("Copy Message").on_click(on_copy))
}

pub fn format_run_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    let remaining = seconds % 60;
    if minutes < 60 {
        return format!("{minutes}m {remaining:02}s");
    }
    format!("{}h {:02}m", minutes / 60, minutes % 60)
}
pub fn last_run_duration(seconds: u64, cx: &App) -> Stateful<Div> {
    let label = format!("Last run · {}", format_run_elapsed(seconds));
    div()
        .id("last-run-duration")
        .debug_selector(|| "last-run-duration".into())
        .role(Role::Status)
        .aria_label(label.clone())
        .px_4()
        .py_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

pub fn conversation_transcript_viewport(
    rail: impl IntoElement,
    transcript: List,
    scroll: &ListState,
    show_environment: bool,
    on_latest: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id("chat-transcript-container")
        .relative()
        .flex()
        .gap_2()
        .w_full()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .child(rail)
        .child(
            div()
                .debug_selector(|| "chat-transcript-viewport".into())
                .flex_1()
                .min_w_0()
                .h_full()
                .child(transcript.when(show_environment, |list| list.px_8())),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .child(gpui_component::scroll::Scrollbar::vertical(scroll)),
        )
        .when(!scroll.is_following_tail(), |el| {
            el.child(
                div().absolute().bottom_3().right_4().child(
                    Button::new("jump-to-latest")
                        .debug_selector(|| "jump-to-latest".into())
                        .icon(IconName::ArrowDown)
                        .size_8()
                        .secondary()
                        .rounded_full()
                        .shadow_md()
                        .accessibility_label("Jump to latest message")
                        .tooltip("Scroll to the latest message")
                        .on_click(on_latest),
                ),
            )
        })
}
