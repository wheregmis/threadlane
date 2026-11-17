//! Shared result chrome for commands, files, searches and diffs.
use gpui::{prelude::*, *};
use gpui_component::scroll::{ScrollableElement, ScrollbarAxis};
use gpui_component::theme::ThemeColor;
use gpui_component::InteractiveElementExt;

const RESULT_OUTPUT_MAX_HEIGHT: f32 = 6.0;

pub fn result_surface(theme: &ThemeColor) -> Div {
    div()
        .w_full()
        .min_w_0()
        .rounded_lg()
        .border_1()
        .border_color(theme.border.opacity(0.5))
        .bg(theme.title_bar)
        .overflow_hidden()
}

pub fn result_header(theme: &ThemeColor) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .px_3()
        .py_1p5()
        .bg(theme.background.opacity(0.35))
        .border_b_1()
        .border_color(theme.border.opacity(0.3))
}

/// The output scrolls inside this viewport without moving the conversation.
pub fn result_viewport(id: String) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .max_h(rems(RESULT_OUTPUT_MAX_HEIGHT))
        .flex_none()
        .flex()
        .flex_col()
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
}

/// Content-sized output with a shared height cap. Keep scrolling inside
/// `result_viewport` so wheel input cannot escape into the conversation.
pub fn result_scroll_body(id: String, content: impl IntoElement) -> impl IntoElement {
    ResultScrollBody {
        id: SharedString::from(id).into(),
        content: content.into_any_element(),
    }
}

#[derive(IntoElement)]
struct ResultScrollBody {
    id: ElementId,
    content: AnyElement,
}

impl RenderOnce for ResultScrollBody {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll = window
            .use_keyed_state(self.id.clone(), cx, |_, _| ScrollHandle::default())
            .read(cx)
            .clone();
        // The height cap belongs to the scroll area, while the content keeps
        // its natural height so GPUI can measure the complete scroll extent.
        div()
            .id(self.id)
            .relative()
            .flex()
            .items_start()
            .max_h(rems(RESULT_OUTPUT_MAX_HEIGHT))
            .whitespace_nowrap()
            .overflow_scroll()
            .lock_scroll_axis()
            .track_scroll(&scroll)
            .child(div().flex_none().min_w_full().child(self.content))
            .scrollbar(&scroll, ScrollbarAxis::Both)
    }
}

/// Clipboard lacks an accessibility-label seam in the pinned kit. Reuse our
/// semantic Button and copy feedback instead; retain only element-local state.
pub(crate) fn result_copy_button(id: &str, text: String) -> impl IntoElement {
    ResultCopyButton {
        id: format!("tool-output-copy-{id}").into(),
        text: text.into(),
    }
}

#[derive(IntoElement)]
struct ResultCopyButton {
    id: SharedString,
    text: SharedString,
}

#[derive(Default)]
struct ResultCopyState {
    copied: Option<SharedString>,
    reset: Option<Task<()>>,
}

impl RenderOnce for ResultCopyButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(
            ElementId::NamedChild(ElementId::from(self.id.clone()).into(), "feedback".into()),
            cx,
            |_, _| ResultCopyState::default(),
        );
        let copied = state.read(cx).copied.as_ref() == Some(&self.text);
        let label = if copied {
            "Output copied"
        } else {
            "Copy output to clipboard"
        };
        crate::conversation::copy_feedback_button(self.id.to_string(), copied, cx)
            .accessibility_label(label)
            .tooltip(label)
            .when(copied, |button| {
                let selector = format!("{}-copied", self.id);
                button.debug_selector(move || selector.clone())
            })
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                cx.write_to_clipboard(ClipboardItem::new_string(self.text.to_string()));
                state.update(cx, |feedback, cx| {
                    feedback.copied = Some(self.text.clone());
                    feedback.reset = Some(cx.spawn(async move |feedback, cx| {
                        cx.background_executor()
                            .timer(crate::MESSAGE_COPY_FEEDBACK_WINDOW)
                            .await;
                        let _ = feedback.update(cx, |feedback, cx| {
                            feedback.copied = None;
                            cx.notify();
                        });
                    }));
                    cx.notify();
                });
            })
    }
}
