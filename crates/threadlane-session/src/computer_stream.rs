//! Live window frames via in-process polling (macOS only).
//!
//! One polling thread composites the target through `CGWindowList` (no
//! subprocess, no Swift toolchain — `ScreenCaptureKit` bindings were
//! evaluated and rejected: they link Swift runtime dylibs that break the
//! build and packaged app). Each poll serves two tiers:
//!
//! - **Live tier** ([`crate::computer_live`]): a nominal-resolution composite
//!   scaled to bounded premultiplied BGRA for the GPUI mirror, published
//!   in-process at up to 30fps while a mirror is subscribed and skipped when
//!   the pixels did not change. This is what makes the mirror feel like
//!   video instead of a slideshow.
//! - **Model tier**: a best-resolution composite kept in memory as a raw
//!   1560px BGRA frame at most every 500ms, so `computer_screenshot` serves
//!   the current picture instantly; the JPEG is encoded only when the model
//!   asks. Full resolution keeps text legible when the target is a small
//!   window. Nothing touches disk until a screenshot is actually served.
//!
//! Frames are never pushed into model context automatically: the model only
//! receives pixels when it explicitly calls `computer_screenshot`.
//!
//! Lifecycle is lazy: the first computer call starts polling for its target,
//! later calls reuse fresh frames, and the thread exits after two minutes
//! without computer calls (ten while a mirror is still watching).

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::computer::{composite_target, pointer_location, CaptureResolution, SCREENSHOT_WIDTH};
pub(crate) use crate::computer_live::StreamTarget;
use crate::computer_live::{self, LiveFrame, LiveStatus, LIVE_FRAME_MAX_WIDTH};

/// Model tier cadence: JPEG plus preview file at most this often.
const MODEL_FRAME_INTERVAL_MS: u64 = 500;
/// Live tier cadence while a mirror is watching and something is happening:
/// a WindowServer composite costs ~22ms and a nominal frame ~30ms all in
/// (measured on an M1 Pro), so 20fps leaves slack; 30fps would peg a core.
const LIVE_ACTIVE_INTERVAL_MS: u64 = 50;
/// Live tier cadence while a mirror is watching but the picture has settled.
const LIVE_QUIET_INTERVAL_MS: u64 = 200;
/// How long after computer activity or a changed frame the live tier stays
/// at full rate.
const LIVE_BOOST_MS: u128 = 10_000;
/// After this long without activity or change a watched feed idles down to
/// the model cadence; any change snaps it back.
const LIVE_SETTLE_MS: u128 = 60_000;
const FRAME_FRESH_MS: u128 = 5_000;
/// Exit after this long without computer calls when nobody is watching.
const STREAM_IDLE_MS: u128 = 120_000;
/// Exit after this long without computer calls even while a mirror watches.
const WATCHED_IDLE_MS: u128 = 600_000;

/// Latest model-tier frame plus what it shows and when it landed. Raw
/// opaque BGRA: encode with [`encode_bgra_jpeg`] to serve it.
#[derive(Clone)]
pub(super) struct StreamFrame {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Source width in display points: display points = image pixels ×
    /// src_points_width / width.
    pub src_points_width: f64,
    pub target: StreamTarget,
    pub ts_ms: u128,
}

#[derive(Clone)]
struct PollerState {
    target: StreamTarget,
    /// Last computer tool call (screenshot or act) that touched the stream.
    last_use_ms: u128,
    /// Last time the live picture actually changed.
    last_change_ms: u128,
}

fn poller() -> &'static Mutex<Option<PollerState>> {
    static POLLER: OnceLock<Mutex<Option<PollerState>>> = OnceLock::new();
    POLLER.get_or_init(|| Mutex::new(None))
}

fn latest() -> &'static Mutex<Option<StreamFrame>> {
    static LATEST: OnceLock<Mutex<Option<StreamFrame>>> = OnceLock::new();
    LATEST.get_or_init(|| Mutex::new(None))
}

/// Last image served to the model per target, for unchanged-frame dedup.
fn last_served() -> &'static Mutex<std::collections::HashMap<StreamTarget, ServedFrame>> {
    static SERVED: OnceLock<Mutex<std::collections::HashMap<StreamTarget, ServedFrame>>> =
        OnceLock::new();
    SERVED.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[derive(Clone, Copy)]
struct ServedFrame {
    hash: u64,
    ts_ms: u128,
    /// Display points per image pixel at serve time. Coordinates the model
    /// reads off the image divide by nothing here — instead `act` divides by
    /// this scale — because window bounds and input events live in points
    /// while screenshots are downscaled pixels.
    points_per_pixel: f64,
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// True when these bytes match what the model last received for `target`,
/// returning when that was. Records the new hash otherwise. Identical
/// consecutive screenshots (static pages, spinners settled) then ride as a
/// one-line note instead of another ~300KB image. Records the point/pixel
/// scale of a changed frame for later coordinate conversion in `act`.
pub(super) fn frame_unchanged_since_with_scale(
    target: StreamTarget,
    bytes: &[u8],
    points_per_pixel: Option<f64>,
) -> Option<u128> {
    let hash = hash_bytes(bytes);
    let mut guard = last_served().lock().ok()?;
    if let Some(served) = guard.get(&target) {
        if served.hash == hash {
            return Some(served.ts_ms);
        }
    }
    let carried_scale = guard.get(&target).map(|served| served.points_per_pixel);
    guard.insert(
        target,
        ServedFrame {
            hash,
            ts_ms: now_ms(),
            points_per_pixel: points_per_pixel.or(carried_scale).unwrap_or(1.0),
        },
    );
    None
}

/// Display-points-per-image-pixel last recorded for `target`, if any
/// screenshot was served for it.
pub(super) fn served_scale(target: StreamTarget) -> Option<f64> {
    last_served()
        .lock()
        .ok()?
        .get(&target)
        .map(|served| served.points_per_pixel)
}

pub(super) fn ago_ms(ts_ms: u128) -> String {
    let secs = now_ms().saturating_sub(ts_ms) / 1_000;
    if secs < 60 {
        format!("{secs}s ago")
    } else {
        format!("{}m ago", secs / 60)
    }
}

fn now_ms() -> u128 {
    computer_live::now_ms()
}

/// Serve a fresh model-tier frame for `target`, if one exists.
pub(super) fn fresh_frame(target: StreamTarget) -> Option<StreamFrame> {
    let frame = latest().lock().ok()?.clone()?;
    (frame.target == target && now_ms().saturating_sub(frame.ts_ms) < FRAME_FRESH_MS)
        .then_some(frame)
}

/// Point the poller at `target`, starting its thread when needed. Never
/// blocks the caller on frames: the first screenshot after a switch still
/// uses one-shot capture while polling warms up in the background.
pub(super) fn ensure_stream(target: StreamTarget) {
    {
        let mut guard = match poller().lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if let Some(running) = guard.as_mut() {
            running.last_use_ms = now_ms();
            if running.target == target {
                return;
            }
        }
        let now = now_ms();
        *guard = Some(PollerState {
            target,
            last_use_ms: now,
            last_change_ms: now,
        });
    }
    spawn_poller();
}

/// An input action happened: keep the stream alive and at full rate without
/// retargeting it (a background act on one window must not flip a mirror
/// that is showing another). Starts polling `target` only when nothing is
/// running, so the mirror shows video during act sequences even when the
/// model has not screenshotted recently.
pub(super) fn touch_or_start(target: StreamTarget) {
    let running = poller()
        .lock()
        .ok()
        .and_then(|mut guard| {
            guard.as_mut().map(|running| {
                running.last_use_ms = now_ms();
            })
        })
        .is_some();
    if !running {
        ensure_stream(target);
    }
}

/// Next capture delay from who is watching and how lively the target is.
/// Nobody watching means the model tier alone; a watched target runs at
/// full rate while acts or picture changes are recent, settles to a calmer
/// rate, and idles down to the model cadence after a quiet minute.
pub(crate) fn tick_interval_ms(
    watchers: usize,
    since_activity_ms: u128,
    since_change_ms: u128,
) -> u64 {
    let liveliness = since_activity_ms.min(since_change_ms);
    if watchers == 0 || liveliness >= LIVE_SETTLE_MS {
        MODEL_FRAME_INTERVAL_MS
    } else if liveliness < LIVE_BOOST_MS {
        LIVE_ACTIVE_INTERVAL_MS
    } else {
        LIVE_QUIET_INTERVAL_MS
    }
}

/// True once the poller should stop: no computer calls for two minutes with
/// nobody watching, or ten minutes regardless.
pub(crate) fn should_exit(watchers: usize, since_activity_ms: u128) -> bool {
    since_activity_ms > WATCHED_IDLE_MS || (watchers == 0 && since_activity_ms > STREAM_IDLE_MS)
}

fn spawn_poller() {
    static POLLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if POLLING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let mut last_model_ms: u128 = 0;
        let mut sleep_ms = LIVE_ACTIVE_INTERVAL_MS;
        loop {
            std::thread::sleep(Duration::from_millis(sleep_ms));
            let snapshot = match poller().lock() {
                Ok(guard) => guard.clone(),
                Err(_) => break,
            };
            let Some(current) = snapshot else {
                break;
            };
            let started = now_ms();
            let watchers = computer_live::watcher_count();
            let since_activity = started.saturating_sub(current.last_use_ms);
            if should_exit(watchers, since_activity) {
                if let Ok(mut guard) = poller().lock() {
                    *guard = None;
                }
                computer_live::set_status(LiveStatus {
                    running: false,
                    target: Some(current.target),
                    last_capture_ms: started,
                    interval_ms: 0,
                    last_error: None,
                    pointer: None,
                });
                break;
            }
            let interval = tick_interval_ms(
                watchers,
                since_activity,
                started.saturating_sub(current.last_change_ms),
            );
            let model_due =
                started.saturating_sub(last_model_ms) >= u128::from(MODEL_FRAME_INTERVAL_MS);
            // Nobody watching and nothing due: nothing to composite this tick.
            if watchers == 0 && !model_due {
                sleep_ms = interval;
                continue;
            }
            let live_composite = if watchers > 0 {
                Some(composite_target(current.target, CaptureResolution::Nominal))
            } else {
                None
            };
            let model_composite = if model_due {
                Some(composite_target(current.target, CaptureResolution::Best))
            } else {
                None
            };
            let pointer = pointer_location();
            let failure = [live_composite.as_ref(), model_composite.as_ref()]
                .into_iter()
                .flatten()
                .find_map(|result| result.as_ref().err().cloned());
            if let Some(error) = failure {
                // Failures stay quiet here; the one-shot screenshot path
                // surfaces capture errors to the model. The mirror header
                // still learns why the picture stopped.
                computer_live::set_status(LiveStatus {
                    running: true,
                    target: Some(current.target),
                    last_capture_ms: started,
                    interval_ms: interval,
                    last_error: Some(error),
                    pointer,
                });
                sleep_ms = interval;
                continue;
            }
            if let Some(Ok(composite)) = live_composite.as_ref() {
                if let Ok((bgra, width, height)) = composite.bgra(LIVE_FRAME_MAX_WIDTH) {
                    let unchanged = computer_live::latest_frame().is_some_and(|previous| {
                        previous.target == current.target
                            && previous.width == width
                            && previous.height == height
                            && previous.bgra == bgra
                    });
                    if !unchanged {
                        computer_live::publish_frame(LiveFrame {
                            seq: 0,
                            ts_ms: started,
                            target: current.target,
                            width,
                            height,
                            bgra,
                            origin_points: composite.origin_points,
                            points_width: composite.points_size.0,
                        });
                        if let Ok(mut guard) = poller().lock() {
                            if let Some(running) = guard.as_mut() {
                                running.last_change_ms = started;
                            }
                        }
                    }
                }
            }
            if let Some(Ok(composite)) = model_composite.as_ref() {
                if let Ok((bgra, width, height)) = composite.bgra(SCREENSHOT_WIDTH) {
                    last_model_ms = started;
                    if let Ok(mut latest) = latest().lock() {
                        *latest = Some(StreamFrame {
                            bgra,
                            width,
                            height,
                            src_points_width: composite.points_size.0,
                            target: current.target,
                            ts_ms: started,
                        });
                    }
                }
            }
            computer_live::set_status(LiveStatus {
                running: true,
                target: Some(current.target),
                last_capture_ms: started,
                interval_ms: interval,
                last_error: None,
                pointer,
            });
            // Pace on wall clock so capture time does not stretch the period.
            let elapsed = now_ms().saturating_sub(started);
            sleep_ms = u64::from(interval).saturating_sub(elapsed as u64).max(1);
        }
        POLLING.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

/// True when every pixel is identical, e.g. a composite of nothing. Every
/// pixel equals the first exactly when every pixel equals its neighbour, so
/// one shifted slice compare (a memcmp, fast even unoptimised) answers it.
pub(crate) fn is_blank(pixels: &[u8], bytes_per_pixel: usize) -> bool {
    let bytes_per_pixel = bytes_per_pixel.max(1);
    if pixels.len() < bytes_per_pixel * 2 {
        return true;
    }
    pixels[bytes_per_pixel..] == pixels[..pixels.len() - bytes_per_pixel]
}

/// JPEG for the model from an opaque BGRA frame: drop alpha, swap to RGB,
/// encode at `quality`. Width is already bounded by the capture.
pub(crate) fn encode_bgra_jpeg(
    bgra: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>, String> {
    let mut rgb = Vec::with_capacity(bgra.len() / 4 * 3);
    for pixel in bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    encode_bounded_jpeg(&rgb, width, height, width, quality).map(|(jpeg, _, _)| jpeg)
}

pub(crate) fn encode_bounded_jpeg(
    rgb: &[u8],
    width: u32,
    height: u32,
    max_width: u32,
    quality: u8,
) -> Result<(Vec<u8>, u32, u32), String> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageBuffer, Rgb};

    let buffer: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_raw(width, height, rgb.to_vec())
        .ok_or_else(|| "Could not wrap capture pixels.".to_string())?;
    let (buffer, width, height) = if width > max_width {
        let height = ((height as u64) * (max_width as u64) / (width as u64)) as u32;
        let resized = image::imageops::resize(
            &buffer,
            max_width,
            height.max(1),
            image::imageops::FilterType::Triangle,
        );
        (resized, max_width, height.max(1))
    } else {
        (buffer, width, height)
    };
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode_image(&buffer)
        .map_err(|error| format!("JPEG encode failed: {error}"))?;
    Ok((bytes, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_frames_dedup_per_target_with_scale() {
        let target = StreamTarget::Window(424242);
        let other = StreamTarget::Display;
        assert!(frame_unchanged_since_with_scale(target, b"frame-a", Some(0.5)).is_none());
        // Same bytes: unchanged, scale preserved from the first serve.
        assert!(frame_unchanged_since_with_scale(target, b"frame-a", Some(0.9)).is_some());
        assert_eq!(served_scale(target), Some(0.5));
        // Different bytes: served fresh with the new scale.
        assert!(frame_unchanged_since_with_scale(target, b"frame-b", Some(0.9)).is_none());
        assert_eq!(served_scale(target), Some(0.9));
        // Other targets are independent.
        assert!(served_scale(other).is_none());
        assert!(!ago_ms(0).is_empty());
    }

    #[test]
    fn tick_interval_follows_watchers_and_liveliness() {
        // Nobody watching: the model tier alone.
        assert_eq!(tick_interval_ms(0, 0, 0), MODEL_FRAME_INTERVAL_MS);
        assert_eq!(tick_interval_ms(0, 60_000, 60_000), MODEL_FRAME_INTERVAL_MS);
        // A watcher plus recent activity or motion: video rate.
        assert_eq!(tick_interval_ms(1, 0, 60_000), LIVE_ACTIVE_INTERVAL_MS);
        assert_eq!(tick_interval_ms(1, 60_000, 0), LIVE_ACTIVE_INTERVAL_MS);
        assert_eq!(
            tick_interval_ms(2, LIVE_BOOST_MS - 1, LIVE_BOOST_MS - 1),
            LIVE_ACTIVE_INTERVAL_MS
        );
        // A watcher but a settled picture: still live, just calmer.
        assert_eq!(
            tick_interval_ms(1, LIVE_BOOST_MS, LIVE_BOOST_MS),
            LIVE_QUIET_INTERVAL_MS
        );
        assert_eq!(
            tick_interval_ms(1, LIVE_SETTLE_MS - 1, LIVE_SETTLE_MS),
            LIVE_QUIET_INTERVAL_MS
        );
        // A quiet minute: idle at the model cadence until something moves.
        assert_eq!(
            tick_interval_ms(1, LIVE_SETTLE_MS, LIVE_SETTLE_MS),
            MODEL_FRAME_INTERVAL_MS
        );
        assert_eq!(
            tick_interval_ms(1, LIVE_SETTLE_MS, 0),
            LIVE_ACTIVE_INTERVAL_MS
        );
        assert!(LIVE_ACTIVE_INTERVAL_MS < LIVE_QUIET_INTERVAL_MS);
        assert!(LIVE_QUIET_INTERVAL_MS < MODEL_FRAME_INTERVAL_MS);
    }

    #[test]
    fn poller_exits_on_idle_unless_watched() {
        assert!(!should_exit(0, STREAM_IDLE_MS));
        assert!(should_exit(0, STREAM_IDLE_MS + 1));
        // A mirror keeps the feed alive past the unwatched idle limit...
        assert!(!should_exit(1, STREAM_IDLE_MS + 1));
        // ...but not forever.
        assert!(should_exit(1, WATCHED_IDLE_MS + 1));
    }

    #[test]
    fn blank_detection_handles_pixel_widths() {
        assert!(is_blank(&[0, 0, 0, 255, 0, 0, 0, 255], 4));
        assert!(!is_blank(&[0, 0, 0, 255, 1, 0, 0, 255], 4));
        assert!(!is_blank(&[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 1, 255], 4));
        assert!(is_blank(&[7, 7, 7, 7, 7, 7], 3));
        assert!(!is_blank(&[7, 7, 7, 7, 7, 8], 3));
        assert!(is_blank(&[], 4));
        assert!(is_blank(&[1, 2, 3, 4], 4));
    }

    #[test]
    fn bgra_jpeg_encodes_opaque_frames() {
        // 2x2 opaque red (B,G,R,A) frame round-trips through the encoder.
        let bgra = [0u8, 0, 255, 255].repeat(4);
        let jpeg = encode_bgra_jpeg(&bgra, 2, 2, 70).expect("encodes");
        assert!(jpeg.starts_with(&[0xFF, 0xD8]), "jpeg magic");
        let decoded = image::load_from_memory(&jpeg).expect("decodes").to_rgb8();
        assert_eq!(decoded.dimensions(), (2, 2));
        let pixel = decoded.get_pixel(0, 0);
        assert!(
            pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60,
            "{pixel:?}"
        );
        assert!(encode_bgra_jpeg(&bgra[..8], 2, 2, 70).is_err());
    }
}
