//! Live window frames via in-process polling (macOS only).
//!
//! One polling thread composites the target at ~2fps through `CGWindowList`
//! (no subprocess, no Swift toolchain — `ScreenCaptureKit` bindings were
//! evaluated and rejected: they link Swift runtime dylibs that break the
//! build and packaged app). Frames land as bounded JPEGs in shared memory
//! plus a `latest-frame.jpg` preview file, so `computer_screenshot` serves
//! the current frame instantly and the mirror popup polls a file without
//! any UI dependency in this crate.
//!
//! Frames are never pushed into model context automatically: the model only
//! receives pixels when it explicitly calls `computer_screenshot`.
//!
//! Lifecycle is lazy: the first screenshot starts polling for its target,
//! later screenshots reuse fresh frames, and the thread exits after two
//! minutes without computer calls.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use super::computer::{
    capture_composited_for_target, write_mirror_sidecar_action, SCREENSHOT_JPEG_QUALITY,
    SCREENSHOT_WIDTH,
};

const FRAME_INTERVAL_MS: u64 = 500;
const FRAME_FRESH_MS: u128 = 5_000;
const STREAM_IDLE_MS: u128 = 120_000;

/// What the poller is pointed at. Screenshots reuse frames only when the
/// target matches; otherwise they fall back to one-shot capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StreamTarget {
    Window(u32),
    Display,
}

impl StreamTarget {
    pub(super) fn label(&self) -> String {
        match self {
            StreamTarget::Window(id) => format!("window {id}"),
            StreamTarget::Display => "the main display".to_string(),
        }
    }
}

/// Latest decoded frame plus what it shows and when it landed.
#[derive(Clone)]
pub(super) struct StreamFrame {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub target: StreamTarget,
    pub ts_ms: u128,
}

#[derive(Clone)]
struct PollerState {
    target: StreamTarget,
    preview_path: Option<PathBuf>,
    previews_dir: PathBuf,
    last_use_ms: u128,
}

fn poller() -> &'static Mutex<Option<PollerState>> {
    static POLLER: OnceLock<Mutex<Option<PollerState>>> = OnceLock::new();
    POLLER.get_or_init(|| Mutex::new(None))
}

fn latest() -> &'static Mutex<Option<StreamFrame>> {
    static LATEST: OnceLock<Mutex<Option<StreamFrame>>> = OnceLock::new();
    LATEST.get_or_init(|| Mutex::new(None))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// Serve a fresh frame for `target`, if one exists.
pub(super) fn fresh_frame(target: StreamTarget) -> Option<StreamFrame> {
    let frame = latest().lock().ok()?.clone()?;
    (frame.target == target && now_ms().saturating_sub(frame.ts_ms) < FRAME_FRESH_MS)
        .then_some(frame)
}

/// Point the poller at `target`, starting its thread when needed. Never
/// blocks the caller on frames: the first screenshot after a switch still
/// uses one-shot capture while polling warms up in the background.
pub(super) fn ensure_stream(target: StreamTarget, preview_path: Option<PathBuf>) {
    let previews_dir = preview_path
        .as_ref()
        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()));
    {
        let mut guard = match poller().lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if let Some(running) = guard.as_mut() {
            running.last_use_ms = now_ms();
            if running.target == target {
                if preview_path.is_some() {
                    running.preview_path = preview_path;
                }
                return;
            }
        }
        let Some(previews_dir) = previews_dir else {
            return;
        };
        *guard = Some(PollerState {
            target,
            preview_path,
            previews_dir,
            last_use_ms: now_ms(),
        });
    }
    spawn_poller();
}

fn spawn_poller() {
    static POLLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if POLLING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(FRAME_INTERVAL_MS));
            let snapshot = match poller().lock() {
                Ok(guard) => guard.clone(),
                Err(_) => break,
            };
            let Some(current) = snapshot else {
                break;
            };
            if now_ms().saturating_sub(current.last_use_ms) > STREAM_IDLE_MS {
                if let Ok(mut guard) = poller().lock() {
                    *guard = None;
                }
                break;
            }
            match capture_composited_for_target(
                current.target,
                SCREENSHOT_WIDTH,
                SCREENSHOT_JPEG_QUALITY,
            ) {
                Ok((jpeg, width, height)) => {
                    let frame = StreamFrame {
                        jpeg: jpeg.clone(),
                        width,
                        height,
                        target: current.target,
                        ts_ms: now_ms(),
                    };
                    if let Ok(mut latest) = latest().lock() {
                        *latest = Some(frame);
                    }
                    if let Some(path) = current.preview_path.as_ref() {
                        let _ = std::fs::write(path, &jpeg);
                        write_mirror_sidecar_action(
                            &current.previews_dir,
                            &format!("Live: {}", current.target.label()),
                        );
                    }
                }
                // Failures stay silent here at 2fps; the one-shot screenshot
                // path surfaces capture errors when the user is watching.
                Err(_) => {}
            }
        }
        POLLING.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}

/// True when every pixel is identical: a denied TCC capture composites black.
pub(crate) fn is_blank(rgb: &[u8]) -> bool {
    let (first, rest) = match rgb.split_first_chunk::<3>() {
        Some(pair) => pair,
        None => return true,
    };
    rest.chunks_exact(3).all(|pixel| pixel == first)
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
    #[test]
    fn stream_target_equality_drives_reuse() {
        assert_eq!(
            super::StreamTarget::Window(7),
            super::StreamTarget::Window(7)
        );
        assert_ne!(super::StreamTarget::Window(7), super::StreamTarget::Display);
    }

    #[test]
    fn stream_target_labels() {
        assert_eq!(
            super::StreamTarget::Window(42).label(),
            "window 42"
        );
        assert_eq!(
            super::StreamTarget::Display.label(),
            "the main display"
        );
    }
}
