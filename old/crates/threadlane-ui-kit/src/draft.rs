//! Saved drafts and prompt recall presentation. Hosts own persistence and navigation.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::dialog::{AlertDialog, DialogButtonProps};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Sizable};

pub fn saved_draft_banner(
    draft: &str,
    restore: Button,
    discard: Button,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    let tip = format!("Saved draft:\n{draft}");
    div()
        .id("saved-draft-banner")
        .role(Role::Group)
        .aria_label(tip.clone())
        .debug_selector(|| "saved-draft-banner".into())
        .w_full()
        .min_w_0()
        .mb_2()
        .px_3()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted.opacity(0.25))
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .flex_1()
                .min_w_0()
                .child(
                    Icon::new(IconName::File)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .id("stashed-draft-preview")
                        .debug_selector(|| "saved-draft-preview".into())
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.foreground)
                        .truncate()
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                        })
                        .child(format!(
                            "Saved draft: \"{}\"",
                            crate::truncate_preview_text(draft, 60)
                        )),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child(restore)
                .child(discard),
        )
}

pub fn restore_saved_draft_button() -> Button {
    Button::new("restore-stashed-draft")
        .debug_selector(|| "restore-stashed-draft".into())
        .label("Restore draft")
        .small()
        .secondary()
        .accessibility_label("Restore saved draft into an empty composer")
}

pub fn discard_saved_draft_button() -> Button {
    Button::new("dismiss-stashed-draft")
        .debug_selector(|| "discard-stashed-draft".into())
        .icon(IconName::Close)
        .ghost()
        .xsmall()
        .accessibility_label("Discard saved draft…")
        .tooltip("Discard saved draft…")
}

/// Hosts attach a guarded confirmation callback; opening this never discards a draft.
pub fn discard_saved_draft_dialog(
    alert: AlertDialog,
    draft: impl Into<SharedString>,
) -> AlertDialog {
    alert.title("Discard saved draft?")
        .description("This removes the saved draft. Your current composer text and attachments will not change.")
        .child(div().debug_selector(|| "saved-draft-discard-preview".into())
            .max_h(rems(8.0)).overflow_y_scrollbar().text_sm().child(draft.into()))
        .button_props(DialogButtonProps::default().ok_text("Discard")
            .ok_variant(ButtonVariant::Danger).show_cancel(true))
}

pub fn save_draft_button(unavailable_reason: Option<&str>) -> Button {
    let hint = unavailable_reason.unwrap_or("Save draft for later");
    Button::new("stash-prompt-btn")
        .debug_selector(|| "stash-prompt-btn".into())
        .icon(IconName::Inbox)
        .ghost()
        .small()
        .rounded_lg()
        .accessibility_label(hint.to_owned())
        .tooltip(hint.to_owned())
        .disabled(unavailable_reason.is_some())
}

pub fn recall_prompt_button(unavailable_reason: Option<&str>) -> Button {
    let hint = unavailable_reason.unwrap_or("Recall previous prompt (Up)");
    Button::new("prompt-recall-btn")
        .debug_selector(|| "prompt-recall-btn".into())
        .icon(IconName::Undo2)
        .ghost()
        .small()
        .rounded_lg()
        .accessibility_label(hint.to_owned())
        .tooltip(hint.to_owned())
        .disabled(unavailable_reason.is_some())
}

pub fn prompt_recall_strip(
    position: usize,
    total: usize,
    older: Button,
    newer: Button,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    div().id("prompt-recall-strip").debug_selector(|| "prompt-recall-strip".into())
        .role(Role::Group).aria_label("Prompt recall")
        .w_full().min_w_0().mb_2().px_3().py_1p5().rounded_lg()
        .border_1().border_color(theme.border).bg(theme.title_bar)
        .flex().flex_wrap().items_center().gap_2()
        .child(Icon::new(IconName::Undo2).xsmall().text_color(theme.muted_foreground))
        .child(div().id("prompt-recall-status").role(Role::Status)
            .aria_label(format!("Earlier prompt {} of {total} recalled into the composer; text only, attachments are not restored", position + 1))
            .flex_1().min_w_0().text_xs().text_color(theme.muted_foreground)
            .child(format!("Earlier prompt · text only · {} of {total}", position + 1)))
        .child(div().flex().flex_none().items_center().gap_2().child(older).child(newer))
}

pub fn recall_older_button(at_oldest: bool) -> Button {
    let hint = if at_oldest {
        "This is the oldest prompt"
    } else {
        "Recall an older prompt (Up)"
    };
    Button::new("prompt-recall-older")
        .debug_selector(|| "prompt-recall-older".into())
        .label("Older")
        .ghost()
        .xsmall()
        .disabled(at_oldest)
        .tooltip(hint)
        .accessibility_label(hint)
}

pub fn recall_newer_button() -> Button {
    Button::new("prompt-recall-newer")
        .debug_selector(|| "prompt-recall-newer".into())
        .label("Newer")
        .ghost()
        .xsmall()
        .tooltip("Recall a newer prompt (Down); clears past the newest")
        .accessibility_label("Recall a newer prompt (Down); clears past the newest")
}
