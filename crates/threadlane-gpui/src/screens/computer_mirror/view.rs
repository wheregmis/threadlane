//! ChatGPT-style live mirror for native computer use: a small
//! non-activating popup showing the latest screenshot plus the current
//! action. It displays; it never drives anything itself.
//!
//! State flows through `<previews>/latest.json` (written by the session
//! computer tools, polled here), so this view stays decoupled from the
//! agent runtime. Captures exclude our own windows, so the mirror cannot
//! recurse into itself.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, IconName, Sizable};

use crate::state::AppState;

pub struct MirrorView {
    focus_handle: FocusHandle,
    model: Entity<AppState>,
    previews_dir: PathBuf,
    image: Option<Arc<Image>>,
    image_path: Option<PathBuf>,
    action: String,
    last_ts: u64,
}

impl MirrorView {
    pub(crate) fn build(
        model: Entity<AppState>,
        previews_dir: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| Self::new(model, previews_dir, window, cx))
    }

    fn new(
        model: Entity<AppState>,
        previews_dir: PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let alive = this
                .update(cx, |this, cx| {
                    if this.poll() {
                        cx.notify();
                    }
                })
                .is_ok();
            if !alive {
                break;
            }
        })
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            model,
            previews_dir,
            image: None,
            image_path: None,
            action: "Waiting for computer activity…".into(),
            last_ts: 0,
        }
    }

    /// Re-read the sidecar; true when the view changed.
    fn poll(&mut self) -> bool {
        let sidecar = self.previews_dir.join("latest.json");
        let bytes = match std::fs::read(&sidecar) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return false,
        };
        let ts = value.get("ts_ms").and_then(|value| value.as_u64()).unwrap_or(0);
        if ts <= self.last_ts {
            return false;
        }
        self.last_ts = ts;
        let mut changed = false;
        if let Some(action) = value.get("action").and_then(|value| value.as_str()) {
            if self.action != action {
                self.action = action.to_string();
                changed = true;
            }
        }
        let path = value
            .get("path")
            .and_then(|value| value.as_str())
            .map(PathBuf::from);
        if path != self.image_path {
            self.image_path = path.clone();
            self.image = path
                .as_ref()
                .and_then(|path| std::fs::read(path).ok())
                .filter(|bytes| !bytes.is_empty())
                .map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes)));
            changed = true;
        }
        changed
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model.update(cx, |state, _| state.mirror_open = false);
        window.remove_window();
    }
}

impl Focusable for MirrorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MirrorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .h(px(30.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .size(px(8.0))
                            .rounded_full()
                            .bg(theme.success),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(self.action.clone()),
                    )
                    .child(
                        Button::new("mirror-close")
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip("Close computer mirror")
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.close(window, cx);
                            })),
                    ),
            )
            .child(match self.image.clone() {
                Some(image) => div()
                    .flex_1()
                    .min_h_0()
                    .child(img(image).size_full().object_fit(ObjectFit::Contain))
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("No capture yet — screenshots appear here.")
                    .into_any_element(),
            })
    }
}
