//! Live video mirror for native computer use: a small non-activating popup
//! that shows the target as it changes plus where the agent's input lands.
//! It displays; it never drives anything itself.
//!
//! Frames arrive in-process from `threadlane_session::computer_live`: the
//! macOS poller publishes bounded BGRA frames at up to 20fps while this view
//! holds a subscription, and each one is painted straight from a
//! `RenderImage` — no JPEG round trip and no file polling on the hot path.
//! Input overlays (click ripples, scroll direction, the pointer) ride the
//! same feed so the popup reads like a screen recording, not a slideshow.
//!
//! `<previews>/latest.json` stays the cold fallback: it names the last
//! capture on disk for when no live frame exists (fresh launch, Linux) and
//! carries the last action line. Captures exclude our own windows, so the
//! mirror cannot recurse into itself.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, IconName, Sizable};
use image::{Frame, RgbaImage};
use threadlane_session::computer_live::{
    self, LiveFrame, LiveOverlay, LiveOverlayKind, LiveStatus,
};

use crate::state::AppState;

/// How long a pointer overlay stays visible.
const OVERLAY_MS: u128 = 1_100;
/// How long the caption chip shows the last action.
const CAPTION_MS: u128 = 2_600;
/// Frame arrivals within this window feed the fps readout.
const FPS_WINDOW_MS: u128 = 2_000;
/// Paints a retired frame outlives before its atlas tile is released, so a
/// command buffer still sampling it never sees the tile reused.
const RETIRE_PAINTS: u8 = 3;
/// Retired frames beyond this are released immediately, paints or not, so a
/// hidden window never hoards textures.
const RETIRE_CAP: usize = 8;
/// A poller capture older than this while running means the feed stalled.
const STALE_MS: u128 = 3_000;

/// The frame currently on screen and the pixels it was built from.
struct LivePicture {
    image: Arc<RenderImage>,
    frame: Arc<LiveFrame>,
}

/// A frame no longer shown, waiting out in-flight GPU work before release.
struct Retired {
    image: Arc<RenderImage>,
    paints_left: u8,
}

pub struct MirrorView {
    focus_handle: FocusHandle,
    model: Entity<AppState>,
    previews_dir: PathBuf,
    live: Option<LivePicture>,
    retired: Arc<Mutex<Vec<Retired>>>,
    overlays: Vec<Arc<LiveOverlay>>,
    status: LiveStatus,
    frame_times: VecDeque<u128>,
    /// Fallback still from disk, for when no live frame exists.
    still: Option<Arc<Image>>,
    still_path: Option<PathBuf>,
    loaded_path: Option<PathBuf>,
    loaded_mtime_ms: u64,
    action: String,
    /// When the last in-process overlay landed; sidecar text yields to it.
    last_overlay_ms: u128,
    error: Option<String>,
    last_ts: u64,
    /// Header as last repainted by the timer, see `header_key`.
    last_header_key: String,
    _tasks: Vec<Task<()>>,
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
        // Live frames: holding this receiver is what switches the poller to
        // its video tier, and dropping the task on close switches it back.
        let frames = cx.spawn(async move |this, cx| {
            let mut receiver = computer_live::subscribe_frames();
            // A fresh subscription has already "seen" the current frame, and
            // a static picture is never republished, so take it up front
            // instead of waiting for pixels that may not change.
            let mut pending = receiver.borrow_and_update().clone();
            loop {
                let frame = match pending.take() {
                    Some(frame) => Some(frame),
                    None => {
                        if receiver.changed().await.is_err() {
                            break;
                        }
                        receiver.borrow_and_update().clone()
                    }
                };
                let Some(frame) = frame else {
                    continue;
                };
                let alive = this
                    .update(cx, |this, cx| {
                        this.apply_frame(frame, cx);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        let overlays = cx.spawn(async move |this, cx| {
            let mut receiver = computer_live::subscribe_overlays();
            loop {
                let overlay = match receiver.recv().await {
                    Ok(overlay) => overlay,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                let alive = this
                    .update(cx, |this, cx| {
                        this.apply_overlay(overlay);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        // Poller status every tick: the pointer keeps moving over a picture
        // that has not changed, and the header learns about stalls at once.
        let status = cx.spawn(async move |this, cx| {
            let mut receiver = computer_live::subscribe_status();
            loop {
                if receiver.changed().await.is_err() {
                    break;
                }
                let status = receiver.borrow_and_update().clone();
                let alive = this
                    .update(cx, |this, cx| {
                        if this.apply_status(status) {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        // Cold path: the on-disk sidecar fallback, at a slideshow cadence.
        let sidecar = cx.spawn(async move |this, cx| loop {
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
        });

        Self {
            focus_handle: cx.focus_handle(),
            model,
            previews_dir,
            live: None,
            retired: Arc::new(Mutex::new(Vec::new())),
            overlays: Vec::new(),
            status: computer_live::status(),
            frame_times: VecDeque::new(),
            still: None,
            still_path: None,
            loaded_path: None,
            loaded_mtime_ms: 0,
            action: "Waiting for computer activity…".into(),
            last_overlay_ms: 0,
            error: None,
            last_ts: 0,
            last_header_key: String::new(),
            _tasks: vec![frames, overlays, status, sidecar],
        }
    }

    /// Take a poller status; true when something visible changed. Every tick
    /// bumps `last_capture_ms`, which the header does not show directly, so
    /// only real changes redraw.
    fn apply_status(&mut self, status: LiveStatus) -> bool {
        let visible = |status: &LiveStatus| {
            (
                status.running,
                status.target,
                status.last_error.clone(),
                status.pointer,
            )
        };
        let changed = visible(&status) != visible(&self.status)
            || self.feed_state_for(&status) != self.feed_state_for(&self.status);
        self.status = status;
        changed
    }

    /// Swap in a new live frame. The previous frame's atlas tile is retired
    /// rather than dropped: the GPU may still be drawing it.
    fn apply_frame(&mut self, frame: Arc<LiveFrame>, cx: &mut Context<Self>) {
        let Some(image) = render_image(&frame) else {
            self.error = Some(format!(
                "Live frame {}x{} has {} bytes; expected {}.",
                frame.width,
                frame.height,
                frame.bgra.len(),
                frame.width as usize * frame.height as usize * 4
            ));
            return;
        };
        self.error = None;
        let now = now_ms();
        self.frame_times.push_back(now);
        while self
            .frame_times
            .front()
            .is_some_and(|ts| now.saturating_sub(*ts) > FPS_WINDOW_MS)
        {
            self.frame_times.pop_front();
        }
        if let Some(previous) = self.live.replace(LivePicture { image, frame }) {
            let mut retired = self
                .retired
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            retired.push(Retired {
                image: previous.image,
                paints_left: RETIRE_PAINTS,
            });
            while retired.len() > RETIRE_CAP {
                let oldest = retired.remove(0);
                cx.drop_image(oldest.image, None);
            }
        }
    }

    fn apply_overlay(&mut self, overlay: Arc<LiveOverlay>) {
        self.action = overlay.label.clone();
        self.last_overlay_ms = overlay.ts_ms;
        self.overlays.push(overlay);
        let now = now_ms();
        self.overlays
            .retain(|overlay| now.saturating_sub(overlay.ts_ms) < CAPTION_MS.max(OVERLAY_MS));
    }

    /// Frames per second over the recent window, once at least two landed
    /// in it. Measured against `now`, not the last arrival: unchanged
    /// pixels are never republished, so the readout must decay to nothing
    /// on its own when the picture settles.
    fn fps(&self, now: u128) -> Option<f32> {
        let recent: Vec<u128> = self
            .frame_times
            .iter()
            .copied()
            .filter(|ts| now.saturating_sub(*ts) <= FPS_WINDOW_MS)
            .collect();
        let (first, last) = (recent.first()?, recent.last()?);
        let span_ms = last.saturating_sub(*first);
        (recent.len() >= 2 && span_ms > 0)
            .then(|| (recent.len() - 1) as f32 * 1_000.0 / span_ms as f32)
    }

    /// What the header would say right now; the sidecar timer compares
    /// this between ticks so time-driven states (a stalled feed, a decayed
    /// fps readout, an expired caption) get their repaint without waiting
    /// for the very events whose absence defines them.
    fn header_key(&self) -> String {
        let now = now_ms();
        let caption_live = self
            .overlays
            .last()
            .is_some_and(|overlay| now.saturating_sub(overlay.ts_ms) < CAPTION_MS);
        format!(
            "{:?}|{:?}|{caption_live}",
            self.feed_state(),
            self.fps(now).map(|fps| fps.round() as u32)
        )
    }

    /// Re-read poller status and the sidecar; true when the view changed.
    /// Never fails silently: a missing sidecar falls back to the newest
    /// capture on disk, and an unreadable image surfaces as an error line
    /// instead of a blank window.
    fn poll(&mut self) -> bool {
        let mut changed = false;
        let header = self.header_key();
        if header != self.last_header_key {
            self.last_header_key = header;
            changed = true;
        }
        if let Ok(bytes) = std::fs::read(self.previews_dir.join("latest.json")) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                let ts = value
                    .get("ts_ms")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                if ts > self.last_ts {
                    self.last_ts = ts;
                    // In-process overlays carry the action while frames flow;
                    // the sidecar only speaks for captures nobody narrated.
                    if let Some(action) = value.get("action").and_then(|value| value.as_str()) {
                        let narrated = self.last_overlay_ms > 0
                            && now_ms().saturating_sub(self.last_overlay_ms) < CAPTION_MS;
                        if !narrated && self.action != action {
                            self.action = action.to_string();
                            changed = true;
                        }
                    }
                    let path = value
                        .get("path")
                        .and_then(|value| value.as_str())
                        .map(PathBuf::from);
                    if path != self.still_path {
                        self.still_path = path;
                        changed = true;
                    }
                }
            }
        }
        if self.live.is_some() {
            // Live frames win; the still is only decoded when there is
            // nothing better to show.
            return changed;
        }
        let mut candidate = self.still_path.clone();
        if candidate.as_ref().is_none_or(|path| !path.is_file()) {
            let scanned = newest_capture(&self.previews_dir);
            if scanned != self.still_path {
                self.still_path = scanned.clone();
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
                    self.still = Some(Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes)));
                    self.error = None;
                }
                None => {
                    self.still = None;
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

    /// Header state: what the feed is doing right now, in one glance.
    fn feed_state(&self) -> FeedState {
        self.feed_state_for(&self.status)
    }

    fn feed_state_for(&self, status: &LiveStatus) -> FeedState {
        let now = now_ms();
        if let Some(error) = status.last_error.as_deref() {
            return FeedState::Error(error.to_string());
        }
        if status.running {
            let stalled = now.saturating_sub(status.last_capture_ms) > STALE_MS;
            if stalled {
                FeedState::Stalled
            } else {
                FeedState::Live { fps: self.fps(now) }
            }
        } else if self.live.is_some() {
            FeedState::Paused
        } else {
            FeedState::Idle
        }
    }
}

#[derive(Debug, PartialEq)]
enum FeedState {
    /// Frames are flowing (or the picture is simply still).
    Live {
        fps: Option<f32>,
    },
    /// The poller reports running but has not captured lately.
    Stalled,
    /// The poller stopped; the last frame stays up until the next action.
    Paused,
    /// Nothing live yet: showing the last capture on disk, if any.
    Idle,
    Error(String),
}

/// Upload-ready image for a live frame: gpui wants tightly packed BGRA rows,
/// which is exactly what the poller publishes, so this is one copy.
fn render_image(frame: &LiveFrame) -> Option<Arc<RenderImage>> {
    // `from_raw` accepts any buffer at least this long, and the atlas
    // uploads with a tight pitch, so a padded buffer would shear silently.
    if frame.bgra.len() != frame.width as usize * frame.height as usize * 4 {
        return None;
    }
    let buffer = RgbaImage::from_raw(frame.width, frame.height, frame.bgra.clone())?;
    Some(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
}

/// Where a `frame_width × frame_height` picture lands inside `bounds` with
/// contain fit, centred. Pure so the math is testable.
fn fit_contain(bounds: Bounds<Pixels>, frame_width: f32, frame_height: f32) -> Bounds<Pixels> {
    if frame_width <= 0.0 || frame_height <= 0.0 {
        return Bounds {
            origin: bounds.origin,
            size: size(px(0.0), px(0.0)),
        };
    }
    let scale_x = bounds.size.width / px(frame_width);
    let scale_y = bounds.size.height / px(frame_height);
    let scale = scale_x.min(scale_y);
    let draw = size(px(frame_width * scale), px(frame_height * scale));
    let origin = point(
        bounds.origin.x + (bounds.size.width - draw.width) / 2.0,
        bounds.origin.y + (bounds.size.height - draw.height) / 2.0,
    );
    Bounds { origin, size: draw }
}

/// Screen position of a frame-pixel coordinate inside the fitted picture.
fn place(image_bounds: Bounds<Pixels>, frame_width: f32, pixel: (f32, f32)) -> Point<Pixels> {
    let scale = if frame_width > 0.0 {
        image_bounds.size.width / px(frame_width)
    } else {
        1.0
    };
    point(
        image_bounds.origin.x + px(pixel.0 * scale),
        image_bounds.origin.y + px(pixel.1 * scale),
    )
}

fn circle(center: Point<Pixels>, radius: Pixels) -> Bounds<Pixels> {
    Bounds {
        origin: point(center.x - radius, center.y - radius),
        size: size(radius * 2.0, radius * 2.0),
    }
}

/// One pointer overlay to draw: kind, frame-pixel position, age fraction.
struct Marker {
    kind: LiveOverlayKind,
    pixel: (f32, f32),
    /// 0 when fresh, 1 when expired.
    progress: f32,
    delta: Option<(f64, f64)>,
}

fn markers(frame: &LiveFrame, overlays: &[Arc<LiveOverlay>], now: u128) -> Vec<Marker> {
    overlays
        .iter()
        .filter_map(|overlay| {
            let age = now.saturating_sub(overlay.ts_ms);
            if age >= OVERLAY_MS {
                return None;
            }
            let (x, y) = overlay.point?;
            let pixel = frame.project(x, y)?;
            Some(Marker {
                kind: overlay.kind,
                pixel,
                progress: age as f32 / OVERLAY_MS as f32,
                delta: overlay.delta,
            })
        })
        .collect()
}

fn paint_marker(marker: &Marker, at: Point<Pixels>, scale: f32, window: &mut Window) {
    let fade = 1.0 - marker.progress;
    let ink = hsla(0.13, 0.95, 0.55, 1.0);
    let rim = hsla(0.0, 0.0, 0.05, 1.0);
    match marker.kind {
        LiveOverlayKind::Click | LiveOverlayKind::DoubleClick => {
            let rings = if marker.kind == LiveOverlayKind::DoubleClick {
                2
            } else {
                1
            };
            for ring in 0..rings {
                let t = (marker.progress - ring as f32 * 0.18).clamp(0.0, 1.0);
                let radius = px(6.0 + 26.0 * t);
                window.paint_quad(
                    outline(circle(at, radius), ink.opacity(1.0 - t), BorderStyle::Solid)
                        .corner_radii(Corners::all(radius))
                        .border_widths(Edges::all(px(2.0))),
                );
            }
            let dot = px(4.0);
            window.paint_quad(
                fill(circle(at, dot), ink.opacity(fade))
                    .corner_radii(Corners::all(dot))
                    .border_widths(Edges::all(px(1.0)))
                    .border_color(rim.opacity(fade)),
            );
        }
        LiveOverlayKind::Move => {
            let arm = px(9.0);
            let thickness = px(2.0);
            for (w, h) in [(arm * 2.0, thickness), (thickness, arm * 2.0)] {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(at.x - w / 2.0, at.y - h / 2.0),
                        size: size(w, h),
                    },
                    ink.opacity(fade),
                ));
            }
        }
        LiveOverlayKind::Scroll => {
            let (dx, dy) = marker.delta.unwrap_or((0.0, 0.0));
            // Wheel deltas are points; show direction with a bounded arm.
            let length = px((dx.abs().max(dy.abs()) as f32 * scale).clamp(12.0, 44.0));
            let thickness = px(4.0);
            let vertical = dy.abs() >= dx.abs();
            let sign = if vertical { dy.signum() } else { dx.signum() } as f32;
            let bounds = if vertical {
                Bounds {
                    origin: point(
                        at.x - thickness / 2.0,
                        if sign >= 0.0 { at.y } else { at.y - length },
                    ),
                    size: size(thickness, length),
                }
            } else {
                Bounds {
                    origin: point(
                        if sign >= 0.0 { at.x } else { at.x - length },
                        at.y - thickness / 2.0,
                    ),
                    size: size(length, thickness),
                }
            };
            window
                .paint_quad(fill(bounds, ink.opacity(fade)).corner_radii(Corners::all(thickness)));
            let radius = px(7.0);
            window.paint_quad(
                outline(circle(at, radius), ink.opacity(fade), BorderStyle::Solid)
                    .corner_radii(Corners::all(radius))
                    .border_widths(Edges::all(px(2.0))),
            );
        }
        LiveOverlayKind::Type | LiveOverlayKind::Press | LiveOverlayKind::Screenshot => {}
    }
}

fn paint_cursor(at: Point<Pixels>, window: &mut Window) {
    let radius = px(5.0);
    window.paint_quad(
        fill(circle(at, radius), hsla(0.0, 0.0, 1.0, 0.95))
            .corner_radii(Corners::all(radius))
            .border_widths(Edges::all(px(1.5)))
            .border_color(hsla(0.0, 0.0, 0.05, 0.9)),
    );
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// Newest `computer-*.jpg` screenshot in a previews dir, if any.
fn newest_capture(dir: &std::path::Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jpg")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("computer-"))
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

impl Focusable for MirrorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MirrorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let now = now_ms();
        self.overlays
            .retain(|overlay| now.saturating_sub(overlay.ts_ms) < CAPTION_MS.max(OVERLAY_MS));
        let state = self.feed_state();
        let (dot, badge, badge_color) = match &state {
            FeedState::Live { fps: Some(fps) } => {
                (theme.success, format!("LIVE · {fps:.0} fps"), theme.success)
            }
            FeedState::Live { fps: None } => (theme.success, "LIVE".to_string(), theme.success),
            FeedState::Stalled => (theme.warning, "STALLED".to_string(), theme.warning),
            FeedState::Paused => (
                theme.muted_foreground,
                "PAUSED".to_string(),
                theme.muted_foreground,
            ),
            FeedState::Idle => (
                theme.muted_foreground,
                "LAST CAPTURE".to_string(),
                theme.muted_foreground,
            ),
            FeedState::Error(_) => (theme.danger, "NO SIGNAL".to_string(), theme.danger),
        };
        let subtitle = match &state {
            FeedState::Error(error) => error.clone(),
            _ => self
                .status
                .target
                .map(|target| target.label())
                .filter(|_| self.last_overlay_ms == 0 && self.action.starts_with("Waiting"))
                .map(|target| format!("Watching {target}"))
                .unwrap_or_else(|| self.action.clone()),
        };
        let caption = self
            .overlays
            .last()
            .filter(|overlay| now.saturating_sub(overlay.ts_ms) < CAPTION_MS)
            .map(|overlay| overlay.label.clone());

        let picture: AnyElement = match (&self.live, self.still.clone(), self.error.clone()) {
            (Some(live), _, _) => {
                let image = live.image.clone();
                let frame = live.frame.clone();
                let markers = markers(&frame, &self.overlays, now);
                let retired = self.retired.clone();
                let backdrop = theme.background;
                let pointer = self.status.pointer;
                let flash = self
                    .overlays
                    .iter()
                    .rev()
                    .find(|overlay| overlay.kind == LiveOverlayKind::Screenshot)
                    .map(|overlay| now.saturating_sub(overlay.ts_ms))
                    .filter(|age| *age < 450)
                    .map(|age| 1.0 - age as f32 / 450.0);
                let animating = !markers.is_empty() || flash.is_some() || caption.is_some();
                canvas(
                    move |_, _, _| (),
                    move |bounds, _, window, _cx| {
                        window.paint_quad(fill(bounds, backdrop));
                        let image_bounds =
                            fit_contain(bounds, frame.width as f32, frame.height as f32);
                        let _ = window.paint_image(
                            image_bounds,
                            image_bounds,
                            Corners::default(),
                            image.clone(),
                            0,
                            false,
                        );
                        let scale = if frame.width > 0 {
                            image_bounds.size.width / px(frame.width as f32)
                        } else {
                            1.0
                        };
                        for marker in &markers {
                            let at = place(image_bounds, frame.width as f32, marker.pixel);
                            paint_marker(marker, at, scale, window);
                        }
                        if let Some(pixel) = pointer.and_then(|(x, y)| frame.project(x, y)) {
                            paint_cursor(place(image_bounds, frame.width as f32, pixel), window);
                        }
                        if let Some(strength) = flash {
                            window.paint_quad(
                                outline(
                                    image_bounds,
                                    hsla(0.0, 0.0, 1.0, 0.9 * strength),
                                    BorderStyle::Solid,
                                )
                                .border_widths(Edges::all(px(3.0))),
                            );
                        }
                        // Release frames the GPU has had time to finish with.
                        let done: Vec<Arc<RenderImage>> = {
                            let mut retired = retired
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            for entry in retired.iter_mut() {
                                entry.paints_left = entry.paints_left.saturating_sub(1);
                            }
                            let (done, keep): (Vec<Retired>, Vec<Retired>) =
                                retired.drain(..).partition(|entry| entry.paints_left == 0);
                            *retired = keep;
                            done.into_iter().map(|entry| entry.image).collect()
                        };
                        for image in done {
                            let _ = window.drop_image(image);
                        }
                        if animating {
                            window.request_animation_frame();
                        }
                    },
                )
                .size_full()
                .into_any_element()
            }
            (None, Some(still), _) => div()
                .flex_1()
                .min_h_0()
                .size_full()
                .child(img(still).size_full().object_fit(ObjectFit::Contain))
                .into_any_element(),
            (None, None, Some(error)) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_xs()
                .text_color(theme.danger)
                .child(error)
                .into_any_element(),
            (None, None, None) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("No capture yet — the live feed starts with the first computer action.")
                .into_any_element(),
        };

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
                    .child(div().size(px(8.0)).rounded_full().bg(dot))
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(badge_color)
                            .child(badge),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(subtitle),
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(picture)
                    .when_some(caption, |this, caption| {
                        this.child(
                            div()
                                .absolute()
                                .bottom_2()
                                .left_2()
                                .max_w_full()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(hsla(0.0, 0.0, 0.0, 0.62))
                                .text_xs()
                                .text_color(hsla(0.0, 0.0, 1.0, 0.95))
                                .truncate()
                                .child(caption),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_session::computer_live::StreamTarget;

    fn frame(width: u32, height: u32) -> LiveFrame {
        LiveFrame {
            seq: 1,
            ts_ms: 0,
            target: StreamTarget::Display,
            width,
            height,
            bgra: vec![0; (width * height * 4) as usize],
            origin_points: (0.0, 0.0),
            points_width: width as f64,
        }
    }

    #[test]
    fn newest_capture_picks_latest_jpg() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("computer-1.jpg");
        let new = dir.path().join("computer-2.jpg");
        std::fs::write(&old, b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::write(&new, b"new").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"nope").unwrap();
        // A leftover preview from pre-video builds is not a screenshot.
        std::fs::write(dir.path().join("latest-frame.jpg"), b"legacy").unwrap();
        assert_eq!(newest_capture(dir.path()), Some(new));
        assert!(file_mtime_ms(&old).is_some());
    }

    #[test]
    fn newest_capture_empty_without_images() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("latest.json"), b"{}").unwrap();
        assert_eq!(newest_capture(dir.path()), None);
    }

    #[test]
    fn contain_fit_letterboxes_and_centres() {
        let bounds = Bounds {
            origin: point(px(10.0), px(20.0)),
            size: size(px(400.0), px(300.0)),
        };
        // Wide frame: width-limited, vertically centred.
        let wide = fit_contain(bounds, 1600.0, 800.0);
        assert_eq!(wide.size, size(px(400.0), px(200.0)));
        assert_eq!(wide.origin, point(px(10.0), px(70.0)));
        // Tall frame: height-limited, horizontally centred.
        let tall = fit_contain(bounds, 300.0, 600.0);
        assert_eq!(tall.size, size(px(150.0), px(300.0)));
        assert_eq!(tall.origin, point(px(135.0), px(20.0)));
        // Degenerate frame: nothing to draw, no panic.
        assert_eq!(fit_contain(bounds, 0.0, 10.0).size, size(px(0.0), px(0.0)));
    }

    #[test]
    fn markers_map_points_through_the_frame() {
        let frame = frame(200, 100);
        let fresh = Arc::new(LiveOverlay {
            seq: 1,
            ts_ms: 1_000,
            kind: LiveOverlayKind::Click,
            point: Some((50.0, 25.0)),
            delta: None,
            label: "Click".into(),
        });
        let outside = Arc::new(LiveOverlay {
            seq: 2,
            ts_ms: 1_000,
            kind: LiveOverlayKind::Click,
            point: Some((500.0, 25.0)),
            delta: None,
            label: "Click".into(),
        });
        let expired = Arc::new(LiveOverlay {
            seq: 3,
            ts_ms: 0,
            kind: LiveOverlayKind::Move,
            point: Some((1.0, 1.0)),
            delta: None,
            label: "Move".into(),
        });
        let keyboard = Arc::new(LiveOverlay {
            seq: 4,
            ts_ms: 1_000,
            kind: LiveOverlayKind::Type,
            point: None,
            delta: None,
            label: "Type".into(),
        });
        let found = markers(
            &frame,
            &[fresh, outside, expired, keyboard],
            1_000 + OVERLAY_MS / 2,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pixel, (50.0, 25.0));
        assert!((found[0].progress - 0.5).abs() < 0.01);
        assert_eq!(found[0].kind, LiveOverlayKind::Click);

        let image_bounds = Bounds {
            origin: point(px(100.0), px(50.0)),
            size: size(px(400.0), px(200.0)),
        };
        assert_eq!(
            place(image_bounds, 200.0, (50.0, 25.0)),
            point(px(200.0), px(100.0))
        );
    }

    #[test]
    fn render_image_requires_tightly_packed_frames() {
        let good = frame(4, 2);
        let image = render_image(&good).expect("packed frame uploads");
        assert_eq!(image.size(0), size(DevicePixels(4), DevicePixels(2)));
        let mut short = frame(4, 2);
        short.bgra.pop();
        assert!(render_image(&short).is_none());
        // Row padding would upload sheared; refuse it too.
        let mut padded = frame(4, 2);
        padded.bgra.extend_from_slice(&[0; 8]);
        assert!(render_image(&padded).is_none());
    }

    #[test]
    fn circle_bounds_centre_on_the_point() {
        let bounds = circle(point(px(10.0), px(10.0)), px(4.0));
        assert_eq!(bounds.origin, point(px(6.0), px(6.0)));
        assert_eq!(bounds.size, size(px(8.0), px(8.0)));
    }
}
