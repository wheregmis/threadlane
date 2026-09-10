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
    loaded_path: Option<PathBuf>,
    loaded_mtime_ms: u64,
    action: String,
    error: Option<String>,
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
            loaded_path: None,
            loaded_mtime_ms: 0,
            action: "Waiting for computer activity…".into(),
            error: None,
            last_ts: 0,
        }
    }

    /// Re-read the sidecar; true when the view changed. Never fails silently:
    /// a missing sidecar falls back to the newest capture on disk, and an
    /// unreadable image surfaces as an error line instead of a blank window.
    fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Ok(bytes) = std::fs::read(self.previews_dir.join("latest.json")) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                let ts = value
                    .get("ts_ms")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                if ts > self.last_ts {
                    self.last_ts = ts;
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
                        self.image_path = path;
                        changed = true;
                    }
                }
            }
        }
        let mut candidate = self.image_path.clone();
        if candidate.as_ref().is_none_or(|path| !path.is_file()) {
            let scanned = newest_capture(&self.previews_dir);
            if scanned != self.image_path {
                self.image_path = scanned.clone();
                changed = true;
            }
            candidate = scanned;
        }
        let mtime = candidate
            .as_ref()
            .and_then(|path| file_mtime_ms(path))
            .unwrap_or(0);
        if candidate != self.loaded_path || mtime > self.loaded_mtime_ms {
            self.loaded_path = candidate.clone();
            self.loaded_mtime_ms = mtime;
            match candidate
                .as_ref()
                .and_then(|path| std::fs::read(path).ok())
                .filter(|bytes| !bytes.is_empty())
            {
                Some(bytes) => {
                    self.image = Some(Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes)));
                    self.error = None;
                }
                None => {
                    self.image = None;
                    self.error = Some(match candidate.as_ref() {
                        Some(path) => format!("Cannot read {}", path.display()),
                        None => format!("No captures in {}", self.previews_dir.display()),
                    });
                }
            }
            changed = true;
        }
        changed
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model.update(cx, |state, _| state.mirror_open = false);
        window.remove_window();
    }
}

/// Newest `computer-*.jpg` / `latest-frame.jpg` in a previews dir, if any.
fn newest_capture(dir: &std::path::Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jpg")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("computer-") || name == "latest-frame.jpg")
        })
        .max_by_key(|path| file_mtime_ms(path).unwrap_or(0))
}

fn file_mtime_ms(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_capture_picks_latest_jpg() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("computer-1.jpg");
        let new = dir.path().join("latest-frame.jpg");
        std::fs::write(&old, b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::write(&new, b"new").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"nope").unwrap();
        assert_eq!(newest_capture(dir.path()), Some(new));
        assert!(file_mtime_ms(&old).is_some());
    }

    #[test]
    fn newest_capture_empty_without_images() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("latest.json"), b"{}").unwrap();
        assert_eq!(newest_capture(dir.path()), None);
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
                    .child(div().size(px(8.0)).rounded_full().bg(theme.success))
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
            .child(match (self.image.clone(), self.error.clone()) {
                (Some(image), _) => div()
                    .flex_1()
                    .min_h_0()
                    .child(img(image).size_full().object_fit(ObjectFit::Contain))
                    .into_any_element(),
                (None, Some(error)) => div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(error)
                    .into_any_element(),
                (None, None) => div()
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
