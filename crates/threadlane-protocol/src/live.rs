//! In-process live feed for native computer use.
//!
//! The GPUI mirror wants video, not a slideshow. The macOS poller in
//! `threadlane-session::computer_stream` publishes bounded opaque BGRA frames
//! here at up to 20fps while a mirror is subscribed, along with input
//! overlays (where a click landed, what was typed) and poller status for the
//! mirror header. Everything is process-global because the poller is: one
//! machine, one live feed, many project sessions.
//!
//! This module lives in `threadlane-protocol` (moved from
//! `threadlane-session::computer_live`) because it is the contract between
//! the session producer and the GPUI consumer: it has no session, runtime,
//! or GPUI dependencies, only `tokio::sync` channels. `threadlane-session`
//! `threadlane-session` re-exports it as `computer_live` for backward
//! compatibility; new code should import from here directly. Nothing here
//! reaches the model: pixels enter context only through explicit
//! `computer_screenshot` calls.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, watch};

/// Bound on live frame width: enough for a mirror popup, small enough to
/// scale, publish, and upload twenty times a second.
pub const LIVE_FRAME_MAX_WIDTH: u32 = 1_280;

/// What the poller is pointed at. Screenshots reuse frames only when the
/// target matches; otherwise they fall back to one-shot capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamTarget {
    Window(u32),
    Display,
}

impl StreamTarget {
    pub fn label(&self) -> String {
        match self {
            StreamTarget::Window(id) => format!("window {id}"),
            StreamTarget::Display => "the main display".to_string(),
        }
    }
}

/// One mirror frame: opaque BGRA pixels plus the geometry needed to map
/// screen-space points (window bounds, input events, the pointer) onto it.
#[derive(Debug)]
pub struct LiveFrame {
    pub seq: u64,
    pub ts_ms: u128,
    pub target: StreamTarget,
    pub width: u32,
    pub height: u32,
    /// `width × height × 4` bytes in B-G-R-A order, effectively opaque (the
    /// capture is drawn over an opaque backdrop; CoreGraphics rounding can
    /// leave a stray 254): the layout gpui uploads without conversion.
    pub bgra: Vec<u8>,
    /// Top-left of the captured region in display points (screen space).
    pub origin_points: (f64, f64),
    /// Captured region width in display points.
    pub points_width: f64,
}

impl LiveFrame {
    /// Frame pixels per display point.
    pub fn pixels_per_point(&self) -> f64 {
        if self.points_width <= 0.0 {
            1.0
        } else {
            f64::from(self.width) / self.points_width
        }
    }

    /// Map a screen-space point onto frame pixels; `None` when it lands
    /// outside the captured region.
    pub fn project(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let scale = self.pixels_per_point();
        let px = (x - self.origin_points.0) * scale;
        let py = (y - self.origin_points.1) * scale;
        (px >= 0.0 && py >= 0.0 && px <= f64::from(self.width) && py <= f64::from(self.height))
            .then_some((px as f32, py as f32))
    }
}

/// What an input action looked like, for drawing on top of the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveOverlayKind {
    Click,
    DoubleClick,
    Move,
    Scroll,
    Type,
    Press,
    Screenshot,
}

#[derive(Clone, Debug)]
pub struct LiveOverlay {
    pub seq: u64,
    pub ts_ms: u128,
    pub kind: LiveOverlayKind,
    /// Screen-space display points for pointer actions.
    pub point: Option<(f64, f64)>,
    /// Scroll delta in points (dx, dy) for scroll actions.
    pub delta: Option<(f64, f64)>,
    /// The approval title: what the user already agreed to.
    pub label: String,
}

/// Poller health for the mirror header — whether frames are flowing, of
/// what, at what cadence, and why not — plus the pointer, refreshed every
/// tick so the cursor overlay keeps moving over a picture that has not.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveStatus {
    pub running: bool,
    pub target: Option<StreamTarget>,
    pub last_capture_ms: u128,
    pub interval_ms: u64,
    pub last_error: Option<String>,
    /// Pointer position in display points (screen space) at the last tick.
    pub pointer: Option<(f64, f64)>,
}

fn frames() -> &'static watch::Sender<Option<Arc<LiveFrame>>> {
    static FRAMES: OnceLock<watch::Sender<Option<Arc<LiveFrame>>>> = OnceLock::new();
    FRAMES.get_or_init(|| {
        // Drop the initial receiver so `receiver_count` counts only mirrors.
        let (sender, receiver) = watch::channel(None);
        drop(receiver);
        sender
    })
}

fn overlays() -> &'static broadcast::Sender<Arc<LiveOverlay>> {
    static OVERLAYS: OnceLock<broadcast::Sender<Arc<LiveOverlay>>> = OnceLock::new();
    OVERLAYS.get_or_init(|| broadcast::channel(64).0)
}

fn status_slot() -> &'static watch::Sender<LiveStatus> {
    static STATUS: OnceLock<watch::Sender<LiveStatus>> = OnceLock::new();
    STATUS.get_or_init(|| {
        let (sender, receiver) = watch::channel(LiveStatus::default());
        drop(receiver);
        sender
    })
}

/// Latest-frame subscription for a mirror. Holding the receiver is what
/// turns the poller's video tier on: it only pays for 20fps while
/// [`watcher_count`] is non-zero, so drop it when the mirror closes.
pub fn subscribe_frames() -> watch::Receiver<Option<Arc<LiveFrame>>> {
    frames().subscribe()
}

/// Input overlays in emission order. A lagging receiver skips old overlays
/// rather than blocking the tools.
pub fn subscribe_overlays() -> broadcast::Receiver<Arc<LiveOverlay>> {
    overlays().subscribe()
}

pub fn subscribe_status() -> watch::Receiver<LiveStatus> {
    status_slot().subscribe()
}

pub fn latest_frame() -> Option<Arc<LiveFrame>> {
    frames().borrow().clone()
}

pub fn status() -> LiveStatus {
    status_slot().borrow().clone()
}

/// Mirrors currently subscribed to frames.
pub fn watcher_count() -> usize {
    frames().receiver_count()
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn next_seq() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Publish a frame to every subscribed mirror, returning its sequence
/// number. Stale frames are replaced, never queued: a slow mirror sees the
/// newest picture, not a backlog.
pub fn publish_frame(mut frame: LiveFrame) -> u64 {
    frame.seq = next_seq();
    let seq = frame.seq;
    frames().send_replace(Some(Arc::new(frame)));
    seq
}

pub fn publish_overlay(
    kind: LiveOverlayKind,
    point: Option<(f64, f64)>,
    delta: Option<(f64, f64)>,
    label: impl Into<String>,
) -> Arc<LiveOverlay> {
    let overlay = Arc::new(LiveOverlay {
        seq: next_seq(),
        ts_ms: now_ms(),
        kind,
        point,
        delta,
        label: label.into(),
    });
    // No mirror listening is fine: overlays are decoration, not state.
    let _ = overlays().send(overlay.clone());
    overlay
}

/// Record poller status; unchanged status wakes nobody.
pub fn set_status(status: LiveStatus) {
    status_slot().send_if_modified(|current| {
        if *current == status {
            false
        } else {
            *current = status;
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, origin: (f64, f64), points_width: f64) -> LiveFrame {
        LiveFrame {
            seq: 0,
            ts_ms: 0,
            target: StreamTarget::Display,
            width,
            height,
            bgra: vec![0; (width * height * 4) as usize],
            origin_points: origin,
            points_width,
        }
    }

    #[test]
    fn stream_target_labels() {
        assert_eq!(StreamTarget::Window(42).label(), "window 42");
        assert_eq!(StreamTarget::Display.label(), "the main display");
        assert_eq!(StreamTarget::Window(7), StreamTarget::Window(7));
        assert_ne!(StreamTarget::Window(7), StreamTarget::Display);
    }

    #[test]
    fn project_maps_screen_points_onto_frame_pixels() {
        // A 1440-point display served at 720px wide: half a pixel per point.
        let display = frame(720, 450, (0.0, 0.0), 1440.0);
        assert_eq!(display.pixels_per_point(), 0.5);
        assert_eq!(display.project(200.0, 100.0), Some((100.0, 50.0)));
        assert_eq!(display.project(-1.0, 0.0), None);
        assert_eq!(display.project(0.0, 901.0), None);

        // A window at (100, 50) that is 400 points wide served at 800px.
        let window = frame(800, 600, (100.0, 50.0), 400.0);
        assert_eq!(window.pixels_per_point(), 2.0);
        assert_eq!(window.project(100.0, 50.0), Some((0.0, 0.0)));
        assert_eq!(window.project(300.0, 200.0), Some((400.0, 300.0)));
        assert_eq!(window.project(99.0, 50.0), None);
    }

    #[test]
    fn degenerate_points_width_falls_back_to_unit_scale() {
        let odd = frame(10, 10, (0.0, 0.0), 0.0);
        assert_eq!(odd.pixels_per_point(), 1.0);
    }

    #[tokio::test]
    async fn frames_reach_subscribers_and_count_watchers() {
        let before = watcher_count();
        let mut receiver = subscribe_frames();
        assert_eq!(watcher_count(), before + 1);
        receiver.mark_unchanged();

        let seq = publish_frame(frame(2, 2, (0.0, 0.0), 2.0));
        assert!(seq > 0);
        receiver.changed().await.expect("sender lives forever");
        let latest = receiver
            .borrow_and_update()
            .clone()
            .expect("frame published");
        assert_eq!(latest.seq, seq);
        assert_eq!(latest_frame().map(|frame| frame.seq >= seq), Some(true));

        drop(receiver);
        assert_eq!(watcher_count(), before);
    }

    #[tokio::test]
    async fn overlays_broadcast_in_order() {
        let mut receiver = subscribe_overlays();
        let first = publish_overlay(LiveOverlayKind::Click, Some((1.0, 2.0)), None, "Click");
        let second = publish_overlay(LiveOverlayKind::Scroll, None, Some((0.0, -40.0)), "Scroll");
        assert!(second.seq > first.seq);
        let got = receiver.recv().await.expect("first overlay");
        assert_eq!(got.kind, LiveOverlayKind::Click);
        assert_eq!(got.point, Some((1.0, 2.0)));
        let got = receiver.recv().await.expect("second overlay");
        assert_eq!(got.kind, LiveOverlayKind::Scroll);
        assert_eq!(got.delta, Some((0.0, -40.0)));
        assert_eq!(got.label, "Scroll");
    }

    #[test]
    fn status_updates_only_on_change() {
        let mut receiver = subscribe_status();
        receiver.mark_unchanged();
        let running = LiveStatus {
            running: true,
            target: Some(StreamTarget::Display),
            last_capture_ms: 5,
            interval_ms: 33,
            last_error: None,
            pointer: Some((1.0, 2.0)),
        };
        set_status(running.clone());
        assert!(receiver.has_changed().unwrap());
        assert_eq!(*receiver.borrow_and_update(), running);
        set_status(running.clone());
        assert!(!receiver.has_changed().unwrap());
        assert_eq!(status(), running);
    }
}
