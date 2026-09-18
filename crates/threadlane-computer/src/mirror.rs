//! Driver-backed live mirror feed for the GPUI popup.
//!
//! A lazily-started tokio task captures the last-used target about once per
//! second while computer calls are recent (or a mirror is watching) and
//! publishes downscaled BGRA frames on [`threadlane_protocol::live`].
//! Captures go through the pooled driver session to a temp PNG
//! (`screenshot_out_file`, so no base64 crosses stdio), which is decoded and
//! scaled here. Frames never enter model context — only explicit screenshot
//! tools attach pixels — so polls need no per-capture approval (the OS grant
//! covers capture, as with the old in-process poller).
//!
//! Lifecycle mirrors the old poller: the first computer call starts the feed
//! for its target, later calls retarget/refresh it, and the task exits after
//! two minutes without computer calls (ten while a mirror watches). One mutex
//! owns both the target state and the "a task is serving it" flag, so an idle
//! exit and a restart can never interleave into state with no task.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use threadlane_protocol::live::{
    self as computer_live, LiveFrame, LiveStatus, StreamTarget, LIVE_FRAME_MAX_WIDTH,
};

use crate::driver::driver_call;

/// Poll cadence: a driver screenshot round trip costs a few hundred ms, so
/// ~1fps is the honest ceiling (the old CG poller managed 20fps; the driver
/// owns capture now and the model path never needed video rate).
const FEED_INTERVAL_MS: u64 = 1_000;
/// Exit after this long without computer calls when nobody is watching.
const FEED_IDLE_MS: u128 = 120_000;
/// Exit after this long without computer calls even while watched.
const FEED_WATCHED_IDLE_MS: u128 = 600_000;
/// Refresh the cached display geometry this often (displays rarely change).
const GEOMETRY_TTL_MS: u128 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq)]
enum FeedTarget {
    Display,
    Window { window_id: i64 },
}

impl FeedTarget {
    fn stream_target(self) -> StreamTarget {
        match self {
            FeedTarget::Display => StreamTarget::Display,
            FeedTarget::Window { window_id } => StreamTarget::Window(window_id as u32),
        }
    }

    fn label(self) -> String {
        match self {
            FeedTarget::Display => "the main display".to_string(),
            FeedTarget::Window { window_id } => format!("window {window_id}"),
        }
    }
}

struct FeedSlot {
    target: Option<FeedTarget>,
    last_use_ms: u128,
    task_running: bool,
    /// Last published pixels per target, for unchanged-frame dedup.
    last_pixels: Option<(StreamTarget, u32, u32, Vec<u8>)>,
    /// Cached logical display width in points + when it was read.
    geometry: Option<(f64, u128)>,
}

impl Default for FeedSlot {
    fn default() -> Self {
        Self {
            target: None,
            last_use_ms: 0,
            task_running: false,
            last_pixels: None,
            geometry: None,
        }
    }
}

fn slot() -> &'static Mutex<FeedSlot> {
    static SLOT: OnceLock<Mutex<FeedSlot>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(FeedSlot::default()))
}

/// The slot, recovering from poisoning: a failed poll must not take the feed
/// down for the rest of the process.
fn lock_slot() -> MutexGuard<'static, FeedSlot> {
    slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Point the feed at the main display, starting its task when needed. Never
/// blocks the caller on frames.
pub(crate) fn ensure_feed_display() {
    ensure_feed(FeedTarget::Display);
}

/// Point the feed at one window, starting its task when needed.
pub(crate) fn ensure_feed_window(window_id: i64) {
    ensure_feed(FeedTarget::Window { window_id });
}

fn ensure_feed(target: FeedTarget) {
    let mut slot = lock_slot();
    let now = computer_live::now_ms();
    slot.last_use_ms = now;
    if slot.target != Some(target) {
        slot.target = Some(target);
    }
    if slot.task_running {
        return;
    }
    slot.task_running = true;
    tokio::spawn(async {
        // Whatever ends the loop, the slot must stop claiming a task that is
        // gone, or no computer call could ever restart the feed.
        struct Released;
        impl Drop for Released {
            fn drop(&mut self) {
                lock_slot().task_running = false;
            }
        }
        let _released = Released;
        feed_loop().await;
    });
}

fn should_exit(watchers: usize, since_activity_ms: u128) -> bool {
    since_activity_ms > FEED_WATCHED_IDLE_MS
        || (watchers == 0 && since_activity_ms > FEED_IDLE_MS)
}

async fn feed_loop() {
    // Trajectory recording rides the feed lifetime: one directory per burst
    // of computer use, stopped when the feed idles out. Best-effort — a
    // failed start/stop must never break captures.
    let recording = start_trajectory_recording().await;
    feed_poll_loop().await;
    if recording {
        let _ = driver_call("stop_recording", serde_json::json!({})).await;
    }
}

/// Output directory for one recording burst. Under the global previews dir
/// (like the mirror sidecar) so bursts from any project land together.
fn recording_output_dir() -> Option<PathBuf> {
    let base = crate::computer::global_previews_dir()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    Some(base.join("recordings").join(format!("computer-{stamp}")))
}

async fn start_trajectory_recording() -> bool {
    let Some(dir) = recording_output_dir() else {
        return false;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    driver_call(
        "start_recording",
        serde_json::json!({"output_dir": dir.to_string_lossy()}),
    )
    .await
    .is_ok()
}

async fn feed_poll_loop() {
    loop {
        tokio::time::sleep(Duration::from_millis(FEED_INTERVAL_MS)).await;
        let (target, watchers) = {
            let slot = lock_slot();
            let Some(target) = slot.target else {
                return;
            };
            let watchers = computer_live::watcher_count();
            if should_exit(watchers, computer_live::now_ms().saturating_sub(slot.last_use_ms)) {
                // Re-check under a fresh lock: a computer call may have landed
                // since the snapshot, and it must find either a live task or
                // a free slot — never a running flag with nobody behind it.
                let mut slot = lock_slot();
                let still_idle = slot.target.is_some_and(|current| {
                    current == target
                        && should_exit(
                            watchers,
                            computer_live::now_ms().saturating_sub(slot.last_use_ms),
                        )
                });
                if !still_idle {
                    continue;
                }
                slot.target = None;
                computer_live::set_status(LiveStatus {
                    running: false,
                    target: Some(target.stream_target()),
                    last_capture_ms: computer_live::now_ms(),
                    interval_ms: 0,
                    last_error: None,
                    pointer: None,
                });
                return;
            }
            (target, watchers)
        };
        let started = computer_live::now_ms();
        let mut error = None;
        match capture_frame(target).await {
            Ok(Some(frame)) => {
                let unchanged = lock_slot().last_pixels.as_ref().is_some_and(
                    |(prev_target, w, h, pixels)| {
                        *prev_target == frame.target
                            && *w == frame.width
                            && *h == frame.height
                            && *pixels == frame.bgra
                    },
                );
                if !unchanged {
                    computer_live::publish_frame(frame_snapshot(&frame));
                    lock_slot().last_pixels = Some((
                        frame.target,
                        frame.width,
                        frame.height,
                        frame.bgra.clone(),
                    ));
                }
            }
            Ok(None) => {}
            Err(message) => error = Some(format!("{}: {message}", target.label())),
        }
        let pointer = if watchers > 0 {
            cursor_position().await
        } else {
            None
        };
        computer_live::set_status(LiveStatus {
            running: true,
            target: Some(target.stream_target()),
            last_capture_ms: started,
            interval_ms: FEED_INTERVAL_MS,
            last_error: error,
            pointer,
        });
    }
}

struct CapturedFrame {
    target: StreamTarget,
    width: u32,
    height: u32,
    bgra: Vec<u8>,
    origin_points: (f64, f64),
    points_width: f64,
}

fn frame_snapshot(frame: &CapturedFrame) -> LiveFrame {
    LiveFrame {
        seq: 0,
        ts_ms: computer_live::now_ms(),
        target: frame.target,
        width: frame.width,
        height: frame.height,
        bgra: frame.bgra.clone(),
        origin_points: frame.origin_points,
        points_width: frame.points_width,
    }
}

fn frame_png_path() -> PathBuf {
    std::env::temp_dir().join(format!("threadlane-cua-frame-{}.png", std::process::id()))
}

/// Capture one frame for `target`: screenshot to a temp PNG (no base64 over
/// stdio), decode, downscale, convert to opaque BGRA.
async fn capture_frame(initial: FeedTarget) -> Result<Option<CapturedFrame>, String> {
    // A stale window id falls back to the display within the same tick; the
    // loop runs at most twice.
    let mut target = initial;
    loop {
        match target {
            FeedTarget::Display => return capture_display().await,
            FeedTarget::Window { window_id } => {
                // Re-resolve every poll: windows move, resize, and die.
                let (pid, (x, y, w, _)) = match window_identity(window_id).await {
                    Ok(identity) => identity,
                    Err(_) => {
                        retarget_display();
                        target = FeedTarget::Display;
                        continue;
                    }
                };
                match capture_window(window_id, pid).await {
                    Ok(frame) => {
                        return png_to_frame(
                            &frame,
                            StreamTarget::Window(window_id as u32),
                            (x, y),
                            w.max(1.0),
                        );
                    }
                    Err(error) if is_window_refusal(&error) => {
                        retarget_display();
                        target = FeedTarget::Display;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

fn retarget_display() {
    lock_slot().target = Some(FeedTarget::Display);
}

/// Ownership races (panel services, relaunch pid reuse) read as refusals.
fn is_window_refusal(error: &str) -> bool {
    error.contains("window_id_not_found")
        || error.contains("window_owner_pid_mismatch")
        || error.contains("ax_window_unresolved")
}

async fn capture_display() -> Result<Option<CapturedFrame>, String> {
    let path = frame_png_path();
    driver_call(
        "get_desktop_state",
        serde_json::json!({"screenshot_out_file": path.to_string_lossy()}),
    )
    .await?;
    let points_width = display_geometry().await;
    png_to_frame(&path, StreamTarget::Display, (0.0, 0.0), points_width)
}

async fn capture_window(window_id: i64, pid: i64) -> Result<PathBuf, String> {
    let path = frame_png_path();
    driver_call(
        "get_window_state",
        serde_json::json!({
            "pid": pid,
            "window_id": window_id,
            "screenshot_out_file": path.to_string_lossy(),
        }),
    )
    .await?;
    Ok(path)
}

/// Live (pid, bounds) for a window id through a fresh driver listing.
async fn window_identity(window_id: i64) -> Result<(i64, (f64, f64, f64, f64)), String> {
    let result = driver_call("list_windows", serde_json::json!({})).await?;
    let windows = result
        .structured
        .as_ref()
        .and_then(|structured| structured.get("windows"))
        .and_then(|windows| windows.as_array());
    let Some(windows) = windows else {
        return Err("Window list returned no windows.".to_string());
    };
    windows
        .iter()
        .filter(|window| window.get("window_id").and_then(|id| id.as_i64()) == Some(window_id))
        .find_map(|window| {
            let pid = window.get("pid")?.as_i64()?;
            let bounds = window.get("bounds")?;
            let coord = |name: &str| bounds.get(name).and_then(|v| v.as_f64()).unwrap_or(0.0);
            Some((
                pid,
                (coord("x"), coord("y"), coord("width"), coord("height")),
            ))
        })
        .ok_or_else(|| format!("Window {window_id} is gone."))
}

/// Cached logical display width in points. Falls back to 1:1 pixel mapping
/// when the driver won't say; the frame's width/points ratio is all the
/// mirror's projection math needs.
async fn display_geometry() -> f64 {
    let now = computer_live::now_ms();
    if let Some((width, at)) = lock_slot().geometry {
        if now.saturating_sub(at) < GEOMETRY_TTL_MS {
            return width;
        }
    }
    let width = driver_call("get_screen_size", serde_json::json!({}))
        .await
        .ok()
        .and_then(|result| result.structured)
        .and_then(|structured| structured.get("width")?.as_f64())
        .unwrap_or(0.0);
    lock_slot().geometry = Some((width, now));
    width
}

async fn cursor_position() -> Option<(f64, f64)> {
    driver_call("get_cursor_position", serde_json::json!({}))
        .await
        .ok()?
        .structured
        .as_ref()
        .and_then(|structured| {
            Some((
                structured.get("x")?.as_f64()?,
                structured.get("y")?.as_f64()?,
            ))
        })
}

/// Decode a PNG screenshot to bounded opaque BGRA (B, G, R, 255): the layout
/// gpui uploads untouched. Pure for testability; failures are feed errors,
// not panics.
fn png_to_bgra(png: &[u8], max_width: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let image = image::load_from_memory(png).map_err(|error| format!("Frame decode failed: {error}"))?;
    let rgba = image.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    if width == 0 || height == 0 {
        return Err("Frame is empty.".to_string());
    }
    let (width, height) = if width > max_width {
        let height = ((u64::from(height) * u64::from(max_width)) / u64::from(width)).max(1) as u32;
        (max_width, height)
    } else {
        (width, height)
    };
    let resized = image::imageops::resize(&rgba, width, height, image::imageops::FilterType::Triangle);
    let mut bgra = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in resized.pixels() {
        bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    Ok((bgra, width, height))
}

fn png_to_frame(
    path: &PathBuf,
    target: StreamTarget,
    origin_points: (f64, f64),
    points_width: f64,
) -> Result<Option<CapturedFrame>, String> {
    let png = std::fs::read(path).map_err(|error| format!("Frame file missing: {error}"))?;
    let (bgra, width, height) = png_to_bgra(&png, LIVE_FRAME_MAX_WIDTH)?;
    Ok(Some(CapturedFrame {
        target,
        width,
        height,
        bgra,
        origin_points,
        points_width: if points_width > 0.0 {
            points_width
        } else {
            f64::from(width)
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_exits_idle_or_watched_only() {
        assert!(!should_exit(0, FEED_IDLE_MS));
        assert!(should_exit(0, FEED_IDLE_MS + 1));
        assert!(!should_exit(1, FEED_IDLE_MS + 1));
        assert!(should_exit(1, FEED_WATCHED_IDLE_MS + 1));
    }

    #[test]
    fn png_decodes_to_opaque_bgra() {
        // 2x1 red+green RGBA frame: hand-encoded PNG round-trips through the
        // converter with R/B swapped and alpha forced opaque.
        let mut rgba = image::RgbaImage::new(2, 1);
        rgba.put_pixel(0, 0, image::Rgba([255, 0, 0, 128]));
        rgba.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (bgra, width, height) = png_to_bgra(&png, 1280).unwrap();
        assert_eq!((width, height), (2, 1));
        assert_eq!(
            bgra,
            vec![0, 0, 255, 255, 0, 255, 0, 255],
            "B,G,R,A with opaque alpha"
        );
        assert!(png_to_bgra(b"not a png", 1280).is_err());
    }

    #[test]
    fn frame_targets_label() {
        assert_eq!(FeedTarget::Display.label(), "the main display");
        assert_eq!(
            FeedTarget::Window { window_id: 7 }.label(),
            "window 7"
        );
    }

    #[test]
    fn recording_dirs_are_namespaced_per_burst() {
        let dir = recording_output_dir().expect("home dir present");
        assert!(dir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("computer-")));
        assert!(dir
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == "recordings"));
    }
}
