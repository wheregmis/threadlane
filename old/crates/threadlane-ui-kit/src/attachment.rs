//! Staged attachment presentation. Hosts retain upload data, decoding and preview lifecycle.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

pub fn staged_image_chip(
    name: impl Into<SharedString>,
    preview: Button,
    remove: Button,
    cx: &App,
) -> Div {
    let name = name.into();
    let theme = cx.theme().colors;
    div()
        .debug_selector(|| "staged-image-chip".into())
        .flex()
        .items_center()
        .gap_1p5()
        .px_2p5()
        .py_1()
        .min_w_0()
        .max_w_full()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_xs()
        .text_color(theme.foreground)
        .child(Icon::new(IconName::File).xsmall().text_color(theme.primary))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .max_w(rems(10.0))
                .truncate()
                .child(name),
        )
        .child(preview)
        .child(remove)
}

/// The host passes the same ID to its keyed focus handle when returning from a preview.
pub fn staged_image_preview_button(
    id: impl Into<ElementId>,
    name: &str,
    position: usize,
    total: usize,
) -> Button {
    let label = format!("Preview {name}, image {position} of {total}");
    Button::new(id)
        .debug_selector(|| "staged-image-preview".into())
        .label("Preview…")
        .xsmall()
        .ghost()
        .accessibility_label(label.clone())
        .tooltip(label)
}

pub fn staged_image_remove_button(id: impl Into<ElementId>, name: &str) -> Button {
    let label = format!("Remove {name}");
    Button::new(id)
        .debug_selector(|| "staged-image-remove".into())
        .icon(IconName::Close)
        .accessibility_label(label.clone())
        .xsmall()
        .ghost()
        .tooltip(label)
}

pub fn composer_attachment_group() -> Div {
    div().flex().flex_wrap().gap_2().min_w_0()
}
