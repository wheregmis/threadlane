//! Image preview presentation; hosts own decoding, mode and dialog lifetime.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::{Dialog, DialogClose, DialogFooter};
use gpui_component::scroll::ScrollableElement;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme, Selectable, Sizable};
use std::sync::Arc;

/// Shared dialog shell; hosts attach content and their close/focus lifecycle.
pub fn image_preview_dialog(dialog: Dialog, window: &Window) -> Dialog {
    dialog
        .title("Image preview")
        .w(window.rem_size() * 40.0)
        .max_w(window.viewport_size().width - window.rem_size() * 2.0)
        .footer(DialogFooter::new().child(DialogClose::new().trigger(|button| {
            button
                .debug_selector(|| "image-preview-close".into())
                .label("Close")
        })))
}

pub fn image_preview_fit_button(id: impl Into<ElementId>, selected: bool) -> Button {
    Button::new(id)
        .debug_selector(|| "image-preview-fit".into())
        .label("Fit")
        .xsmall()
        .ghost()
        .selected(selected)
        .accessibility_label("Fit image to preview")
}

pub fn image_preview_actual_size_button(
    id: impl Into<ElementId>,
    selected: bool,
    width: u32,
    height: u32,
) -> Button {
    Button::new(id)
        .debug_selector(|| "image-preview-actual-size".into())
        .label("Actual size")
        .xsmall()
        .ghost()
        .selected(selected)
        .accessibility_label(format!(
            "Show image at actual size, {width} by {height} pixels"
        ))
}

/// `None` displays loading, `Err` displays the decode error, and `Ok` displays the image.
pub fn image_preview_content(
    name: impl Into<SharedString>,
    decoded: Option<Result<Arc<RenderImage>, String>>,
    actual_size: bool,
    controls: impl IntoIterator<Item = AnyElement>,
    window: &Window,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    let height =
        (window.viewport_size().height - window.rem_size() * 12.0).max(window.rem_size() * 8.0);
    let dimensions = decoded
        .as_ref()
        .and_then(|image| image.as_ref().ok())
        .map(|image| image.size(0));
    let body = match decoded {
        Some(Ok(image)) if actual_size => {
            let size = image.size(0);
            let scale = window.scale_factor();
            div()
                .debug_selector(|| "image-preview-actual-viewport".into())
                .w_full()
                // Scroll containers need a definite height; max-height alone collapses
                // when GPUI measures the overflow content independently.
                .h(px(size.height.0 as f32 / scale).min(height))
                .min_h_0()
                .overflow_scrollbar()
                .child(
                    div()
                        .w(px(size.width.0 as f32 / scale))
                        .h(px(size.height.0 as f32 / scale))
                        .child(img(image).size_full().object_fit(ObjectFit::None)),
                )
                .into_any_element()
        }
        Some(Ok(image)) => div()
            .w_full()
            .h(height)
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .child(img(image).size_full().object_fit(ObjectFit::Contain))
            .into_any_element(),
        Some(Err(error)) => div()
            .debug_selector(|| "image-preview-error".into())
            .w_full()
            .h(height)
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .text_color(theme.danger)
            .child(error)
            .into_any_element(),
        None => div()
            .debug_selector(|| "image-preview-loading".into())
            .w_full()
            .h(height)
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Spinner::new().small())
            .child("Preparing image preview…")
            .into_any_element(),
    };
    div()
        .debug_selector(|| "image-preview-content".into())
        .flex()
        .flex_col()
        .gap_2()
        .min_w_0()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(name.into()),
                )
                .child(div().flex().items_center().gap_1().children(controls))
                .children(dimensions.map(|size| {
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{} × {} px",
                            size.width.0 as u32, size.height.0 as u32
                        ))
                })),
        )
        .child(body)
}
