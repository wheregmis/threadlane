//! Native computer-use tools: window introspection, screenshots, input control.
//!
//! Waku-shaped and Threadlane-gated: screenshots and input actions require
//! user approval through the session
//! [`PermissionManager`](crate::permission::PermissionManager). The first
//! action prompts with Once/Always scopes and an Always grant is remembered
//! per project; unattended sessions deny by default, matching the ACP
//! reject-by-default policy. macOS-only; other platforms get a helpful error.
//!
//! Screenshots reach the model as JPEG images attached to the tool result
//! alongside text metadata; the full file also lands in
//! `<work_dir>/.threadlane/previews/` for the user.
//!
//! Every screenshot and act also feeds the in-process live mirror
//! ([`crate::computer_live`]): the poller in `computer_stream` streams the
//! target as video while the GPUI popup watches, and acts publish overlays
//! so the user sees where a click lands.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use threadlane_runtime::{AgentToolDefinition, Capability, ToolExecutor, ToolOutput};

use crate::permission::{PermissionDecision, PermissionManager};

pub const COMPUTER_WINDOWS_TOOL: &str = "computer_windows";
pub const COMPUTER_SCREENSHOT_TOOL: &str = "computer_screenshot";
pub const COMPUTER_ACT_TOOL: &str = "computer_act";
pub const COMPUTER_STATUS_TOOL: &str = "computer_status";

pub const COMPUTER_UNAVAILABLE: &str = "Native computer use is available on macOS only.";

const MAX_TYPE_CHARS: usize = 4_000;
/// Screenshots larger than this ride as metadata only, never pixels.
pub(crate) const MAX_IMAGE_BYTES: usize = 2_000_000;
/// Capture width bound: enough for UI legibility, small enough for context.
pub(crate) const SCREENSHOT_WIDTH: u32 = 1_560;
pub(crate) const SCREENSHOT_JPEG_QUALITY: u8 = 70;

pub(crate) struct ComputerToolExecutor {
    permissions: Option<Arc<PermissionManager>>,
}

impl ComputerToolExecutor {
    pub(crate) fn new(permissions: Option<Arc<PermissionManager>>) -> Self {
        Self { permissions }
    }

    async fn approve(&self, title: String, detail: String) -> Result<(), String> {
        let Some(permissions) = &self.permissions else {
            return Err("Computer use has no permission channel in this session.".to_string());
        };
        match permissions.request_computer(&title, &detail).await {
            PermissionDecision::Deny => Err(format!(
                "The user denied this computer action ({title}). Do not retry it verbatim; ask how to proceed."
            )),
            PermissionDecision::AllowOnce | PermissionDecision::AllowAlways => Ok(()),
        }
    }
}

fn computer_tool_definitions() -> Arc<[AgentToolDefinition]> {
    vec![
        AgentToolDefinition::new(
            COMPUTER_STATUS_TOOL,
            "Report native computer-use availability: OS, window listing, screenshot, and input support. Call this before computer_windows/computer_screenshot/computer_act on a new machine.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            COMPUTER_WINDOWS_TOOL,
            "List on-screen windows: id, app, title, and bounds in display pixels. Coordinates from this list feed computer_act and computer_screenshot.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            COMPUTER_SCREENSHOT_TOOL,
            "Capture the main display (or one window by id from computer_windows). You receive the image plus its path and dimensions; the user sees the same file and must approve each capture. Prefer the embedded browser tools for web pages.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "window_id": {
                        "type": "integer",
                        "description": "Optional window id from computer_windows. Omit for the main display."
                    }
                },
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            COMPUTER_ACT_TOOL,
            "Control mouse and keyboard: click, double_click, move, scroll, type, press. Every call asks the user for approval first; denied actions must not be retried verbatim. Coordinates are display pixels from computer_windows.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["click", "double_click", "move", "scroll", "type", "press"],
                        "description": "click/double_click/move need x,y. scroll needs dx/dy in pixels. type needs text. press needs key."
                    },
                    "x": { "type": "number", "description": "Display x pixel." },
                    "y": { "type": "number", "description": "Display y pixel." },
                    "dx": { "type": "number", "description": "Horizontal scroll pixels (positive = right)." },
                    "dy": { "type": "number", "description": "Vertical scroll pixels (positive = down)." },
                    "text": { "type": "string", "description": "Text for the type action." },
                    "key": {
                        "type": "string",
                        "description": "Key for press: a named key (Enter, Escape, Tab, Space, Backspace, Delete, Up, Down, Left, Right) or a single character for combos (cmd+L, ctrl+C)."
                    },
                    "modifiers": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["shift", "ctrl", "alt", "cmd"] },
                        "description": "Optional modifiers held for click/press."
                    },
                    "target": {
                        "type": "integer",
                        "description": "Optional window id from computer_windows. Coordinates become window-relative and input goes straight to that app without moving your cursor or stealing focus. Omit for foreground control with display coordinates."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        ),
    ]
    .into()
}

/// Validated `computer_act` target: a window id whose coordinates are
/// window-relative and whose input is background-delivered.
/// Resolved delivery: screen-space intent plus where to post it.
#[derive(Debug, PartialEq)]
pub(crate) struct TargetedAct {
    pub intent: ComputerAct,
    /// Target process id for background delivery; None posts to the HID
    /// stream (foreground: moves the cursor, steals focus).
    pub pid: Option<i32>,
    pub app: Option<String>,
}

pub(crate) fn parse_act_target(args: &str) -> Result<Option<i64>, String> {
    let parsed: serde_json::Value = serde_json::from_str(args)
        .map_err(|error| format!("Invalid {COMPUTER_ACT_TOOL} arguments: {error}"))?;
    match parsed.get("target") {
        None => Ok(None),
        // Window id 0 is kCGNullWindowID and never names a window; a zero
        // target is always a caller defaulting the field, not a real target.
        Some(value) => value
            .as_i64()
            .filter(|id| *id > 0)
            .map(Some)
            .ok_or_else(|| {
                "`computer_act` target must be a window id from computer_windows.".to_string()
            }),
    }
}

/// Resolve a parsed intent against an optional target window: window-relative
/// coordinates shift to screen space and delivery becomes background
/// (`post_to_pid`, cursor untouched). Untargeted intents keep display
/// coordinates and HID delivery.
#[cfg(target_os = "macos")]
fn resolve_target(intent: ComputerAct, target: Option<i64>) -> Result<TargetedAct, String> {
    let Some(id) = target else {
        return Ok(TargetedAct {
            intent,
            pid: None,
            app: None,
        });
    };
    let (pid, owner, (origin_x, origin_y)) = mac::find_window(id as i32).ok_or_else(|| {
        format!("Window {id} is gone; re-list with computer_windows and pick a live id.")
    })?;
    let shift = |x: f64, y: f64| (x + origin_x, y + origin_y);
    let intent = match intent {
        ComputerAct::Click { x, y, modifiers } => {
            let (x, y) = shift(x, y);
            ComputerAct::Click { x, y, modifiers }
        }
        ComputerAct::DoubleClick { x, y, modifiers } => {
            let (x, y) = shift(x, y);
            ComputerAct::DoubleClick { x, y, modifiers }
        }
        ComputerAct::Move { x, y } => {
            let (x, y) = shift(x, y);
            ComputerAct::Move { x, y }
        }
        // Scroll deltas, text, and keys are coordinate-free.
        intent => intent,
    };
    Ok(TargetedAct {
        intent,
        pid: Some(pid),
        app: Some(owner),
    })
}

#[cfg(not(target_os = "macos"))]
fn resolve_target(intent: ComputerAct, target: Option<i64>) -> Result<TargetedAct, String> {
    if target.is_some() {
        return Err(COMPUTER_UNAVAILABLE.to_string());
    }
    Ok(TargetedAct {
        intent,
        pid: None,
        app: None,
    })
}

/// Validated `computer_act` intent. Pure and cross-platform for testability;
/// execution is macOS-only.
#[derive(Debug, PartialEq)]
pub(crate) enum ComputerAct {
    Click {
        x: f64,
        y: f64,
        modifiers: Vec<String>,
    },
    DoubleClick {
        x: f64,
        y: f64,
        modifiers: Vec<String>,
    },
    Move {
        x: f64,
        y: f64,
    },
    Scroll {
        dx: f64,
        dy: f64,
    },
    Type {
        text: String,
    },
    Press {
        key: String,
        modifiers: Vec<String>,
    },
}

impl ComputerAct {
    /// One-line approval copy: what the user sees on the permission card.
    fn approval_title(&self) -> String {
        match self {
            ComputerAct::Click { x, y, .. } => format!("Click at ({x:.0}, {y:.0})"),
            ComputerAct::DoubleClick { x, y, .. } => {
                format!("Double-click at ({x:.0}, {y:.0})")
            }
            ComputerAct::Move { x, y } => format!("Move pointer to ({x:.0}, {y:.0})"),
            ComputerAct::Scroll { dx, dy } => format!("Scroll by ({dx:.0}, {dy:.0})"),
            ComputerAct::Type { text } => {
                let preview: String = text.chars().take(48).collect();
                format!("Type {preview:?} ({} chars)", text.chars().count())
            }
            ComputerAct::Press { key, modifiers } => {
                if modifiers.is_empty() {
                    format!("Press {key}")
                } else {
                    format!("Press {}+{key}", modifiers.join("+"))
                }
            }
        }
    }
}

const VALID_MODIFIERS: &[&str] = &["shift", "ctrl", "alt", "cmd"];
const VALID_KEYS: &[&str] = &[
    "Enter",
    "Escape",
    "Tab",
    "Space",
    "Backspace",
    "Delete",
    "Up",
    "Down",
    "Left",
    "Right",
];

fn parse_point(args: &serde_json::Value) -> Result<(f64, f64), String> {
    let point = |name: &str| {
        args.get(name)
            .and_then(|value| value.as_f64())
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                format!("`computer_act` click/double_click/move needs numeric `{name}`.")
            })
    };
    Ok((point("x")?, point("y")?))
}

fn parse_modifiers(args: &serde_json::Value) -> Result<Vec<String>, String> {
    let Some(list) = args.get("modifiers") else {
        return Ok(Vec::new());
    };
    let list = list
        .as_array()
        .ok_or_else(|| "`computer_act` modifiers must be an array.".to_string())?;
    list.iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "`computer_act` modifiers must be strings.".to_string())
                .and_then(|name| {
                    VALID_MODIFIERS
                        .contains(&name)
                        .then(|| name.to_string())
                        .ok_or_else(|| {
                            "`computer_act` modifiers must be shift, ctrl, alt, or cmd.".to_string()
                        })
                })
        })
        .collect()
}

pub(crate) fn parse_computer_act(args: &str) -> Result<ComputerAct, String> {
    let parsed: serde_json::Value = serde_json::from_str(args)
        .map_err(|error| format!("Invalid {COMPUTER_ACT_TOOL} arguments: {error}"))?;
    let action = parsed
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match action {
        "click" => {
            let (x, y) = parse_point(&parsed)?;
            Ok(ComputerAct::Click {
                x,
                y,
                modifiers: parse_modifiers(&parsed)?,
            })
        }
        "double_click" => {
            let (x, y) = parse_point(&parsed)?;
            Ok(ComputerAct::DoubleClick {
                x,
                y,
                modifiers: parse_modifiers(&parsed)?,
            })
        }
        "move" => {
            let (x, y) = parse_point(&parsed)?;
            Ok(ComputerAct::Move { x, y })
        }
        "scroll" => {
            let delta = |name: &str| {
                parsed
                    .get(name)
                    .and_then(|value| value.as_f64())
                    .filter(|value| value.is_finite())
                    .unwrap_or(0.0)
            };
            let (dx, dy) = (delta("dx"), delta("dy"));
            if dx == 0.0 && dy == 0.0 {
                return Err("`computer_act` scroll needs a non-zero dx or dy.".into());
            }
            Ok(ComputerAct::Scroll { dx, dy })
        }
        "type" => {
            let text = parsed
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if text.is_empty() {
                return Err("`computer_act` type requires non-empty `text`.".into());
            }
            if text.chars().count() > MAX_TYPE_CHARS {
                return Err(format!(
                    "`computer_act` type accepts at most {MAX_TYPE_CHARS} characters."
                ));
            }
            Ok(ComputerAct::Type {
                text: text.to_string(),
            })
        }
        "press" => {
            let key = parsed
                .get("key")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .ok_or_else(|| "`computer_act` press requires `key`.".to_string())?;
            // Named keys (Enter, Escape, …) or one character for combos
            // (cmd+L, ctrl+C): single characters ride the unicode path.
            let is_character = key.chars().count() == 1;
            if !is_character && !VALID_KEYS.contains(&key) {
                return Err(format!(
                    "`computer_act` press key must be a single character or one of {}.",
                    VALID_KEYS.join(", ")
                ));
            }
            Ok(ComputerAct::Press {
                key: key.to_string(),
                modifiers: parse_modifiers(&parsed)?,
            })
        }
        _ => Err(
            "`computer_act` action must be one of click, double_click, move, scroll, type, press."
                .into(),
        ),
    }
}

#[async_trait]
impl ToolExecutor for ComputerToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.computer"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        computer_tool_definitions()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        self.execute_tool_in_workspace(name, args, None).await
    }

    async fn execute_tool_in_workspace(
        &self,
        name: &str,
        args: &str,
        work_dir: Option<&Path>,
    ) -> Option<Result<String, String>> {
        match name {
            COMPUTER_STATUS_TOOL => Some(Ok(computer_status())),
            COMPUTER_WINDOWS_TOOL => {
                #[cfg(target_os = "macos")]
                {
                    Some(list_windows())
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = args;
                    Some(Err(COMPUTER_UNAVAILABLE.to_string()))
                }
            }
            COMPUTER_SCREENSHOT_TOOL => {
                #[cfg(target_os = "macos")]
                {
                    Some(self.screenshot(args, work_dir).await)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = (args, work_dir);
                    Some(Err(COMPUTER_UNAVAILABLE.to_string()))
                }
            }
            COMPUTER_ACT_TOOL => {
                #[cfg(target_os = "macos")]
                {
                    Some(self.act(args, work_dir).await)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = (args, work_dir);
                    Some(Err(COMPUTER_UNAVAILABLE.to_string()))
                }
            }
            _ => None,
        }
    }

    /// Screenshots ride the rich path so pixels reach the provider payload;
    /// every other tool keeps the default string mapping.
    #[cfg(target_os = "macos")]
    async fn execute_tool_with_output_in_workspace(
        &self,
        name: &str,
        args: &str,
        work_dir: Option<&Path>,
    ) -> Option<Result<ToolOutput, String>> {
        if name == COMPUTER_SCREENSHOT_TOOL {
            return Some(self.screenshot_with_image(args, work_dir).await);
        }
        self.execute_tool_in_workspace(name, args, work_dir)
            .await
            .map(|result| result.map(ToolOutput::from))
    }
}

fn computer_status() -> String {
    #[cfg(target_os = "macos")]
    {
        "Native computer use is available on this Mac: computer_windows lists on-screen windows, computer_screenshot captures the display or one window, computer_act clicks/types/presses keys. Every screenshot and input action asks for approval first; macOS may also prompt for Screen Recording and Accessibility on first use (System Settings → Privacy & Security)."
            .to_string()
    }
    #[cfg(not(target_os = "macos"))]
    {
        COMPUTER_UNAVAILABLE.to_string()
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::{CFString, CFStringRef};

    pub(super) fn previews_dir(work_dir: Option<&Path>) -> PathBuf {
        work_dir
            .unwrap_or_else(|| Path::new("."))
            .join(".threadlane")
            .join("previews")
    }

    pub(super) fn list_windows() -> Result<String, String> {
        let own_pid = std::process::id() as i32;
        let mut rows = Vec::new();
        for window in window_infos()?
            .into_iter()
            .filter(|window| window.onscreen && window.pid != own_pid && window.id >= 0)
        {
            rows.push(format!(
                "id={} app={:?} title={:?} pid={} layer={} alpha={:.2} x={:.0} y={:.0} w={:.0} h={:.0}",
                window.id,
                window.owner,
                window.title,
                window.pid,
                window.layer,
                window.alpha,
                window.bounds.0,
                window.bounds.1,
                window.bounds.2,
                window.bounds.3
            ));
            if rows.len() >= 50 {
                break;
            }
        }
        if rows.is_empty() {
            return Ok("No on-screen windows found.".to_string());
        }
        Ok(format!(
            "On-screen windows (display pixels; Threadlane's own windows are hidden — never act on them):\n{}",
            rows.join("\n")
        ))
    }

    /// On-screen window ids excluding our own PID, for recursion-free capture.
    /// Hide our own windows: the agent must never drive Threadlane itself,
    /// or approvals and focus chase each other in a loop.
    pub(super) fn capture_window_ids() -> Vec<i32> {
        let own_pid = std::process::id() as i32;
        window_infos()
            .unwrap_or_default()
            .into_iter()
            .filter(|window| window.onscreen && window.pid != own_pid && window.id >= 0)
            .map(|window| window.id)
            .collect()
    }

    /// Resolve a target window id to (pid, owner, bounds origin) for
    /// background delivery. Our own windows stay unaddressable even if a
    /// stale id names one.
    pub(super) fn find_window(id: i32) -> Option<(i32, String, (f64, f64))> {
        let own_pid = std::process::id() as i32;
        window_infos().ok()?.into_iter().find_map(|window| {
            (window.id == id && window.onscreen && window.pid != own_pid).then(|| {
                (
                    window.pid,
                    window.owner.clone(),
                    (window.bounds.0, window.bounds.1),
                )
            })
        })
    }

    /// One WindowServer composite of a target: full-resolution pixels plus
    /// the screen-space geometry needed to scale and map them. Both tiers
    /// (live BGRA for the mirror, JPEG for the model) derive from one
    /// composite, so a poll costs one WindowServer round trip.
    pub(crate) struct Composite {
        image: core_graphics::image::CGImage,
        resolution: CaptureResolution,
        /// Top-left of the captured region in display points.
        pub(crate) origin_points: (f64, f64),
        /// Captured region size in display points.
        pub(crate) points_size: (f64, f64),
    }

    impl Composite {
        /// Downscale to at most `max_width` pixels wide as opaque
        /// little-endian BGRA (B, G, R, A in memory with A at 255, give or
        /// take CoreGraphics' resampling rounding), the layout gpui uploads
        /// untouched. CoreGraphics resamples in one pass; no Rust
        /// per-pixel loop. Model-tier (best-resolution) composites use
        /// proper filtering so hairlines and small text survive the ~2×
        /// reduction; the live tier takes the cheaper low-quality resample.
        /// A refused bitmap context is an error, never a panic: the poller
        /// must survive it and the one-shot path must fall back.
        pub(crate) fn bgra(&self, max_width: u32) -> Result<(Vec<u8>, u32, u32), String> {
            use core_graphics::context::CGInterpolationQuality;
            let interpolation = match self.resolution {
                CaptureResolution::Best => CGInterpolationQuality::CGInterpolationQualityDefault,
                CaptureResolution::Nominal => CGInterpolationQuality::CGInterpolationQualityLow,
            };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scale_bgra(&self.image, max_width, interpolation)
            }))
            .unwrap_or_else(|_| Err("CoreGraphics refused the bitmap context.".to_string()))
        }

        /// Bounded JPEG for the model from the same composite.
        pub(crate) fn jpeg(
            &self,
            max_width: u32,
            quality: u8,
        ) -> Result<(Vec<u8>, u32, u32), String> {
            let (bgra, width, height) = self.bgra(max_width)?;
            let jpeg = crate::computer_stream::encode_bgra_jpeg(&bgra, width, height, quality)?;
            Ok((jpeg, width, height))
        }
    }

    /// Whether this process may read other apps' pixels. Never prompts.
    /// Without the grant a composite is not blank but hollow — other apps'
    /// windows are silently omitted — so ask first instead of inspecting.
    pub(crate) fn screen_capture_granted() -> bool {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGPreflightScreenCaptureAccess() -> bool;
        }
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub(crate) const SCREEN_RECORDING_HINT: &str = "Screen Recording permission is missing: grant it to this app in System Settings → Privacy & Security, then retry.";

    /// Pixel density of a composite. The WindowServer round trip costs the
    /// same either way (~22ms measured); everything downstream is 4× cheaper
    /// at nominal, which is why the live mirror uses it while the model keeps
    /// full resolution for legible text in small windows.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum CaptureResolution {
        /// Display points, 1:1 — no scale bookkeeping, no Retina backing.
        Nominal,
        /// The display's best (Retina 2×) backing.
        Best,
    }

    /// Composite `target` through the WindowServer: every on-screen window
    /// except ours for the display, or one window cropped to its bounds so
    /// the model sees just what it drives and coordinates stay window-local.
    /// Unlike `screencapture` this never includes Threadlane's own windows,
    /// so a mirror popup cannot recurse. Fails closed without Screen
    /// Recording access so callers fall back to `screencapture`, which owns
    /// the TCC prompt.
    pub(crate) fn composite_target(
        target: crate::computer_stream::StreamTarget,
        resolution: CaptureResolution,
    ) -> Result<Composite, String> {
        use core_foundation::array::CFArray;
        use core_graphics::display::CGDisplay;
        use core_graphics::geometry::{CGPoint, CGRect, CGSize};
        use core_graphics::window::{
            create_image_from_array, kCGWindowImageDefault, kCGWindowImageNominalResolution,
        };

        if !screen_capture_granted() {
            return Err(SCREEN_RECORDING_HINT.to_string());
        }
        let (ids, rect) = match target {
            crate::computer_stream::StreamTarget::Display => {
                let ids = capture_window_ids();
                if ids.is_empty() {
                    return Err("No capturable windows.".to_string());
                }
                (ids, CGDisplay::main().bounds())
            }
            crate::computer_stream::StreamTarget::Window(id) => {
                let id = id as i32;
                // Our own windows stay unaddressable here too, or a stale
                // id could point the mirror at itself.
                let own_pid = std::process::id() as i32;
                let bounds = window_infos()
                    .ok()
                    .and_then(|infos| {
                        infos.into_iter().find_map(|window| {
                            (window.id == id && window.onscreen && window.pid != own_pid)
                                .then_some(window.bounds)
                        })
                    })
                    .ok_or_else(|| {
                        format!(
                            "Window {id} is gone; re-list with computer_windows and pick a live id."
                        )
                    })?;
                if bounds.2 < 1.0 || bounds.3 < 1.0 {
                    return Err(format!("Window {id} has no visible area."));
                }
                (
                    vec![id],
                    CGRect::new(
                        &CGPoint::new(bounds.0, bounds.1),
                        &CGSize::new(bounds.2, bounds.3),
                    ),
                )
            }
        };
        // Plain window numbers with no CF callbacks, exactly like the system
        // `create_window_list` array: WindowServer reads each element as a
        // `CGWindowID` cast to `void*`, not a CFNumber object. The elements
        // must be pointer-sized — `CFArrayCreate` copies `len` pointers out
        // of the buffer, so a `u32` slice would pair up ids and read past
        // its end.
        let raw: Vec<usize> = ids.iter().map(|id| *id as u32 as usize).collect();
        let array = CFArray::from_copyable(&raw).to_untyped();
        let option = match resolution {
            CaptureResolution::Nominal => kCGWindowImageNominalResolution,
            CaptureResolution::Best => kCGWindowImageDefault,
        };
        let image = create_image_from_array(rect, array, option)
            .ok_or_else(|| "Window composite failed.".to_string())?;
        if image.width() == 0 || image.height() == 0 {
            return Err("Window composite is empty.".to_string());
        }
        Ok(Composite {
            image,
            resolution,
            origin_points: (rect.origin.x, rect.origin.y),
            points_size: (rect.size.width, rect.size.height),
        })
    }

    /// Draw `image` into a bounded bitmap context and hand back its bytes:
    /// opaque BGRA, rows top-down, `width × 4` bytes per row. The context is
    /// first filled with a dark backdrop: windows-only composites leave
    /// uncovered desktop transparent, and gpui blends with straight alpha,
    /// so opaque output is the only layout both tiers agree on. Panics if
    /// CoreGraphics refuses the context (`create_bitmap_context` asserts);
    /// callers catch that.
    fn scale_bgra(
        image: &core_graphics::image::CGImage,
        max_width: u32,
        interpolation: core_graphics::context::CGInterpolationQuality,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        use core_graphics::base::{kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedFirst};
        use core_graphics::context::{CGContext, CGInterpolationQuality};
        use core_graphics::geometry::{CGPoint, CGRect, CGSize};

        let source_width = u32::try_from(image.width()).unwrap_or(u32::MAX);
        let source_height = u32::try_from(image.height()).unwrap_or(u32::MAX);
        if source_width == 0 || source_height == 0 {
            return Err("Window composite is empty.".to_string());
        }
        let width = source_width.min(max_width.max(1));
        let height =
            ((u64::from(source_height) * u64::from(width)) / u64::from(source_width)).max(1) as u32;
        let bytes_per_row = width as usize * 4;
        // The composite's own profile: a DeviceRGB/sRGB context would colour
        // match every pixel (+25ms measured) for a picture that is, by
        // definition, already what the screen shows.
        let color_space = image.color_space();
        let mut context = CGContext::create_bitmap_context(
            None,
            width as usize,
            height as usize,
            8,
            bytes_per_row,
            &color_space,
            kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
        );
        let rect = CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(f64::from(width), f64::from(height)),
        );
        context.set_rgb_fill_color(0.08, 0.08, 0.09, 1.0);
        context.fill_rect(rect);
        context.set_interpolation_quality(if width < source_width {
            interpolation
        } else {
            CGInterpolationQuality::CGInterpolationQualityNone
        });
        context.draw_image(rect, image);
        context.flush();
        let stride = context.bytes_per_row();
        let rows = height as usize;
        let data = context.data();
        if data.len() < stride * rows {
            return Err("Bitmap context is short.".to_string());
        }
        let mut bgra = Vec::with_capacity(bytes_per_row * rows);
        if stride == bytes_per_row {
            bgra.extend_from_slice(&data[..bytes_per_row * rows]);
        } else {
            for row in 0..rows {
                bgra.extend_from_slice(&data[row * stride..row * stride + bytes_per_row]);
            }
        }
        Ok((bgra, width, height))
    }

    /// Current pointer position in display points (top-left origin), for the
    /// mirror's cursor overlay. `None` without an event source.
    pub(crate) fn pointer_location() -> Option<(f64, f64)> {
        use core_graphics::event::CGEvent;
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
        let location = CGEvent::new(source).ok()?.location();
        Some((location.x, location.y))
    }

    /// Display points per served image pixel: window bounds and input
    /// events live in points while screenshots are downscaled pixels.
    /// Kept for callers that only know the served width; prefer passing the
    /// exact source width through capture results instead.
    pub(super) fn display_scale_for(served_width: u32) -> f64 {
        use core_graphics::display::CGDisplay;
        if served_width == 0 {
            return 1.0;
        }
        let points = CGDisplay::main().bounds().size.width;
        if points <= 0.0 {
            return 1.0;
        }
        points / f64::from(served_width)
    }

    /// Read one window dictionary by comparing key names. Comparing
    /// content (not pointers) keeps this independent of CF key-callback
    /// details; the kCGWindow* names are stable API.
    pub(super) struct WindowInfo {
        pub(super) id: i32,
        pub(super) owner: String,
        pub(super) title: String,
        pub(super) pid: i32,
        pub(super) layer: i32,
        pub(super) alpha: f64,
        pub(super) bounds: (f64, f64, f64, f64),
        pub(super) onscreen: bool,
    }
    fn cf_string(ptr: *const std::ffi::c_void) -> String {
        unsafe { CFString::wrap_under_get_rule(ptr as CFStringRef) }.to_string()
    }

    fn cf_number(value: &core_foundation::base::CFType) -> Option<CFNumber> {
        value.downcast::<CFNumber>()
    }

    fn read_dictionary(dict: &CFDictionary) -> WindowInfo {
        let mut strings = std::collections::HashMap::new();
        let mut numbers = std::collections::HashMap::new();
        let mut onscreen = false;
        let mut bounds = (0.0, 0.0, 0.0, 0.0);
        let (keys, values) = dict.get_keys_and_values();
        for (key, value) in keys.iter().zip(values.iter()) {
            let name = cf_string(*key);
            let value = unsafe { core_foundation::base::CFType::wrap_under_get_rule(*value) };
            if name == "kCGWindowBounds" {
                if let Some(rect) = value.downcast::<CFDictionary>() {
                    let (rect_keys, rect_values) = rect.get_keys_and_values();
                    let mut components = std::collections::HashMap::new();
                    for (rect_key, rect_value) in rect_keys.iter().zip(rect_values.iter()) {
                        let rect_value = unsafe {
                            core_foundation::base::CFType::wrap_under_get_rule(*rect_value)
                        };
                        if let Some(number) = cf_number(&rect_value) {
                            if let Some(component) = number.to_f64() {
                                components.insert(cf_string(*rect_key), component);
                            }
                        }
                    }
                    bounds = (
                        components.get("X").copied().unwrap_or(0.0),
                        components.get("Y").copied().unwrap_or(0.0),
                        components.get("Width").copied().unwrap_or(0.0),
                        components.get("Height").copied().unwrap_or(0.0),
                    );
                }
                continue;
            }
            if name == "kCGWindowIsOnscreen" {
                onscreen = value
                    .downcast::<CFBoolean>()
                    .is_some_and(|flag| flag == CFBoolean::true_value());
                continue;
            }
            if let Some(text) = value.downcast::<CFString>() {
                strings.insert(name, text.to_string());
                continue;
            }
            if let Some(number) = cf_number(&value) {
                numbers.insert(name, number);
                continue;
            }
        }
        let integer = |name: &str, fallback: i32| {
            numbers
                .get(name)
                .and_then(|number| {
                    number
                        .to_i32()
                        .or_else(|| number.to_f64().map(|value| value as i32))
                })
                .unwrap_or(fallback)
        };
        WindowInfo {
            id: integer("kCGWindowNumber", -1),
            owner: strings
                .get("kCGWindowOwnerName")
                .cloned()
                .unwrap_or_default(),
            title: strings.get("kCGWindowName").cloned().unwrap_or_default(),
            pid: integer("kCGWindowOwnerPID", -1),
            layer: integer("kCGWindowLayer", -1),
            alpha: numbers
                .get("kCGWindowAlpha")
                .and_then(|number| number.to_f64())
                .unwrap_or(1.0),
            bounds,
            onscreen,
        }
    }

    pub(super) fn window_infos() -> Result<Vec<WindowInfo>, String> {
        use core_foundation::base::{CFIndex, TCFType};
        use core_foundation::dictionary::CFDictionary;
        use core_graphics::window::{
            copy_window_info, kCGNullWindowID, kCGWindowListExcludeDesktopElements,
            kCGWindowListOptionOnScreenOnly,
        };

        let info = copy_window_info(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        )
        .ok_or_else(|| "Could not list windows.".to_string())?;
        let mut windows = Vec::new();
        for index in 0..info.len().min(50) {
            let Some(item) = info.get(index as CFIndex) else {
                continue;
            };
            let raw: *const std::ffi::c_void = *item;
            let dict = unsafe { core_foundation::base::CFType::wrap_under_get_rule(raw) };
            let Some(dict) = dict.downcast::<CFDictionary>() else {
                continue;
            };
            windows.push(read_dictionary(&dict));
        }
        Ok(windows)
    }
}

#[cfg(target_os = "macos")]
fn list_windows() -> Result<String, String> {
    mac::list_windows()
}

#[cfg(target_os = "macos")]
impl ComputerToolExecutor {
    async fn screenshot(&self, args: &str, work_dir: Option<&Path>) -> Result<String, String> {
        Ok(self.screenshot_with_image(args, work_dir).await?.content)
    }

    async fn screenshot_with_image(
        &self,
        args: &str,
        work_dir: Option<&Path>,
    ) -> Result<ToolOutput, String> {
        let window_id: Option<i64> = serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|value| value.get("window_id")?.as_i64());
        let target = match window_id {
            Some(id) => format!("window {id}"),
            None => "the main display".to_string(),
        };
        self.approve(
            format!("Screenshot {target}"),
            format!("Capture {target}. You will see the image; the agent receives it too."),
        )
        .await?;
        let dir = mac::previews_dir(work_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("Could not create preview dir: {error}"))?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("computer-{stamp}.jpg"));
        // The live mirror is global (one popup, many project sessions) while
        // history stays per-project.
        let mirror_dir = global_previews_dir().unwrap_or_else(|| dir.clone());
        // Live stream first: a fresh frame for this exact target is instant
        // and already excludes our own windows. Otherwise fall back to
        // one-shot capture (which also warms the stream for next time).
        let target = match window_id {
            Some(id) => crate::computer_stream::StreamTarget::Window(id as u32),
            None => crate::computer_stream::StreamTarget::Display,
        };
        crate::computer_stream::ensure_stream(target);
        if let Some(frame) = crate::computer_stream::fresh_frame(target) {
            // Only the JPEG encode is paid here, off the async executor.
            let (width, height, src_points_width) =
                (frame.width, frame.height, frame.src_points_width);
            let jpeg = tokio::task::spawn_blocking(move || {
                crate::computer_stream::encode_bgra_jpeg(
                    &frame.bgra,
                    frame.width,
                    frame.height,
                    SCREENSHOT_JPEG_QUALITY,
                )
            })
            .await
            .map_err(|error| format!("Screenshot encode failed: {error}"))??;
            std::fs::write(&path, &jpeg)
                .map_err(|error| format!("Could not save screenshot: {error}"))?;
            write_mirror_sidecar(
                &mirror_dir,
                Some(&path),
                &format!("Screenshot {} (live)", target.label()),
            );
            note_screenshot(target);
            return Ok(attach_jpeg(
                &path,
                &jpeg,
                &format!("{width}x{height}"),
                Some(target),
                src_points_width / f64::from(width.max(1)),
            ));
        }
        // One-shot capture, target-aware: a window id composites just that
        // window so the model sees what it drives instead of a full display
        // with Threadlane mixed in. Falls back to `screencapture` (which owns
        // the TCC prompt) when the composite is unavailable or blank.
        // Each arm yields (bytes, dims text, source points width) so clicks
        // convert image pixels back to display points exactly.
        // The composite, downscale, and encode are all CPU work; keep them
        // off the async executor like every other capture path.
        let composited = tokio::task::spawn_blocking(move || {
            capture_composited_for_target(target, SCREENSHOT_WIDTH, SCREENSHOT_JPEG_QUALITY)
        })
        .await
        .map_err(|error| format!("Screenshot task failed: {error}"))
        .and_then(|result| result);
        let (bytes, dims, src_points_width) = match composited {
            Ok((bytes, width, height, src_points_width)) => {
                std::fs::write(&path, &bytes)
                    .map_err(|error| format!("Could not save screenshot: {error}"))?;
                (bytes, format!("{width}x{height}"), src_points_width)
            }
            Err(_) => {
                let bytes = tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || capture_jpeg(window_id, &path)
                })
                .await
                .map_err(|error| format!("Screenshot task failed: {error}"))??;
                let dims = jpeg_dimensions(&path).unwrap_or_else(|| "unknown size".to_string());
                let served_width = dims
                    .split('x')
                    .next()
                    .and_then(|width| width.parse::<u32>().ok())
                    .unwrap_or(SCREENSHOT_WIDTH);
                // screencapture covers the display: fall back to display scale
                // (source points = served pixels × scale).
                let src_points_width = served_width as f64 * mac::display_scale_for(served_width);
                (bytes, dims, src_points_width)
            }
        };
        write_mirror_sidecar(
            &mirror_dir,
            Some(&path),
            &format!("Screenshot {}", target.label()),
        );
        note_screenshot(target);
        let served_width = dims
            .split('x')
            .next()
            .and_then(|width| width.parse::<u32>().ok())
            .unwrap_or(SCREENSHOT_WIDTH);
        Ok(attach_jpeg(
            &path,
            &bytes,
            &dims,
            Some(target),
            src_points_width / f64::from(served_width.max(1)),
        ))
    }

    async fn act(&self, args: &str, work_dir: Option<&Path>) -> Result<String, String> {
        let intent = parse_computer_act(args)?;
        let raw_target = parse_act_target(args)?;
        // Convert image-pixel coordinates to display points using the scale
        // recorded when this target was last screenshotted. Without a scale
        // reference the model is acting blind: proceed unscaled but say so.
        let scale_target = match raw_target {
            Some(id) => Some(crate::computer_stream::StreamTarget::Window(id as u32)),
            None => Some(crate::computer_stream::StreamTarget::Display),
        };
        let scale = scale_target
            .and_then(crate::computer_stream::served_scale)
            .unwrap_or(1.0);
        let scale_note = if scale_target
            .is_some_and(|target| crate::computer_stream::served_scale(target).is_none())
        {
            " (no scale reference on file — screenshot the target first for precise clicks)"
        } else {
            ""
        };
        let mut intent = intent;
        match &mut intent {
            ComputerAct::Click { x, y, .. }
            | ComputerAct::DoubleClick { x, y, .. }
            | ComputerAct::Move { x, y } => {
                *x *= scale;
                *y *= scale;
            }
            // Scroll deltas are wheel units, not screen positions.
            ComputerAct::Scroll { .. } | ComputerAct::Type { .. } | ComputerAct::Press { .. } => {}
        }
        let targeted = resolve_target(intent, raw_target)?;
        let mut title = targeted.intent.approval_title();
        if let Some(app) = &targeted.app {
            title = format!("{title} in {app}");
        }
        let delivery = if targeted.pid.is_some() {
            "Background delivery: your cursor and focus stay untouched."
        } else {
            "Foreground delivery: the cursor will move and focus may change."
        };
        self.approve(
            title.clone(),
            format!("{title} on this Mac. {delivery} Deny if the target looks wrong."),
        )
        .await?;
        // Keep the live mirror rolling through act sequences and show the
        // user where this one lands as it happens.
        let mirror_dir = global_previews_dir().unwrap_or_else(|| mac::previews_dir(work_dir));
        crate::computer_stream::touch_or_start(
            scale_target.unwrap_or(crate::computer_stream::StreamTarget::Display),
        );
        publish_act_overlay(&targeted.intent, &title);
        let outcome =
            tokio::task::spawn_blocking(move || perform_act(&targeted.intent, targeted.pid))
                .await
                .map_err(|error| format!("Input task failed: {error}"))?;
        if let Ok(outcome) = &outcome {
            write_mirror_sidecar(
                &mirror_dir,
                None,
                &format!("{title} — {outcome}{scale_note}"),
            );
        }
        outcome.map(|outcome| format!("{outcome}{scale_note}"))
    }
}

/// Flash the mirror: the model just took a picture of `target`.
#[cfg(target_os = "macos")]
fn note_screenshot(target: crate::computer_stream::StreamTarget) {
    crate::computer_live::publish_overlay(
        crate::computer_live::LiveOverlayKind::Screenshot,
        None,
        None,
        format!("Screenshot {}", target.label()),
    );
}

/// Publish the mirror overlay for an act in screen space, after target
/// resolution so window-relative coordinates already carry the window
/// origin.
#[cfg(target_os = "macos")]
fn publish_act_overlay(intent: &ComputerAct, label: &str) {
    use crate::computer_live::{publish_overlay, LiveOverlayKind};
    match intent {
        ComputerAct::Click { x, y, .. } => {
            publish_overlay(LiveOverlayKind::Click, Some((*x, *y)), None, label)
        }
        ComputerAct::DoubleClick { x, y, .. } => {
            publish_overlay(LiveOverlayKind::DoubleClick, Some((*x, *y)), None, label)
        }
        ComputerAct::Move { x, y } => {
            publish_overlay(LiveOverlayKind::Move, Some((*x, *y)), None, label)
        }
        // Wheel events land under the pointer.
        ComputerAct::Scroll { dx, dy } => publish_overlay(
            LiveOverlayKind::Scroll,
            mac::pointer_location(),
            Some((*dx, *dy)),
            label,
        ),
        ComputerAct::Type { .. } => publish_overlay(LiveOverlayKind::Type, None, None, label),
        ComputerAct::Press { .. } => publish_overlay(LiveOverlayKind::Press, None, None, label),
    };
}

/// Build the screenshot tool output: text metadata plus the JPEG for the
/// model, unless it exceeds the model byte cap (metadata only then) or is
/// pixel-identical to what the model last received for the target (a
/// one-line unchanged note, no re-attached image).
#[cfg(target_os = "macos")]
fn attach_jpeg(
    path: &Path,
    bytes: &[u8],
    dims: &str,
    target: Option<crate::computer_stream::StreamTarget>,
    points_per_pixel: f64,
) -> ToolOutput {
    let content = format!(
        "Screenshot saved to {} ({} pixels, {} bytes). Display points = image pixels × {:.3}.",
        path.display(),
        dims,
        bytes.len(),
        points_per_pixel
    );
    if bytes.len() > MAX_IMAGE_BYTES {
        return ToolOutput {
            content: format!(
                "{content} Image exceeded the {MAX_IMAGE_BYTES}-byte model limit, so only metadata is attached."
            ),
            images: Vec::new(),
        };
    }
    if let Some(since_ms) = target.and_then(|target| {
        crate::computer_stream::frame_unchanged_since_with_scale(
            target,
            bytes,
            Some(points_per_pixel),
        )
    }) {
        return ToolOutput {
            content: format!(
                "{content} Unchanged since {} — same pixels as the screenshot you already have, so no new image is attached.",
                crate::computer_stream::ago_ms(since_ms)
            ),
            images: Vec::new(),
        };
    }
    use base64::Engine as _;
    let data_url = format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    ToolOutput {
        content,
        images: vec![threadlane_runtime::ImageAttachment {
            display_name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "screenshot.jpg".into()),
            data_url,
        }],
    }
}

/// Target-aware one-shot JPEG for the model: a single window by id, or the
/// display minus our own windows. Returns (jpeg bytes, served width, served
/// height, source points width).
#[cfg(target_os = "macos")]
pub(crate) fn capture_composited_for_target(
    target: crate::computer_stream::StreamTarget,
    max_width: u32,
    quality: u8,
) -> Result<(Vec<u8>, u32, u32, f64), String> {
    let composite = mac::composite_target(target, mac::CaptureResolution::Best)?;
    let (jpeg, width, height) = composite.jpeg(max_width, quality)?;
    Ok((jpeg, width, height, composite.points_size.0))
}

#[cfg(target_os = "macos")]
pub(crate) use mac::{composite_target, pointer_location, CaptureResolution};

/// Start the live feed on the main display for the user's own mirror, with
/// no model in the loop: a developer hook (`THREADLANE_MIRROR_DEBUG`) for
/// observing and profiling the video path. Frames stay in-process and are
/// never attached to a tool result, so no permission gate applies. Like any
/// watched stream it idles out after ten minutes without computer calls;
/// relaunch to resume.
#[cfg(target_os = "macos")]
pub fn watch_display_for_debug() {
    crate::computer_stream::ensure_stream(crate::computer_stream::StreamTarget::Display);
}

#[cfg(not(target_os = "macos"))]
pub fn watch_display_for_debug() {}

/// Global live-mirror dir (`~/.threadlane/previews/`): the popup is global
/// while sessions live in per-project worktrees, so `latest.json` lives
/// here. Timestamped history files stay per-project.
#[cfg(target_os = "macos")]
pub fn global_previews_dir() -> Option<PathBuf> {
    threadlane_wasi::packages::default_global_threadlane_dir().map(|dir| dir.join("previews"))
}

#[cfg(not(target_os = "macos"))]
pub fn global_previews_dir() -> Option<PathBuf> {
    None
}

/// Mirror-popup state for the GPUI frontend: the latest preview path (if any),
/// the last computer action, and when it happened. Polled, never pushed, so
/// the session crate keeps no UI dependency.
fn write_mirror_sidecar(dir: &Path, path: Option<&Path>, action: &str) {
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let sidecar = serde_json::json!({
        "path": path.map(|path| path.to_string_lossy().to_string()),
        "action": action,
        "ts_ms": ts_ms,
    });
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join("latest.json"), sidecar.to_string());
}

/// Capture via the system `screencapture` CLI (blocking): it owns TCC prompts
/// and encoding, so no new native deps are needed for pixels. Returns the
/// normalized JPEG bytes; the file stays behind for the user.
#[cfg(target_os = "macos")]
fn capture_jpeg(window_id: Option<i64>, path: &Path) -> Result<Vec<u8>, String> {
    let mut command = std::process::Command::new("/usr/sbin/screencapture");
    command.args(["-x", "-t", "jpg"]);
    match window_id {
        Some(id) => {
            command.args(["-o", "-l", &id.to_string()]);
        }
        None => {
            command.arg("-m");
        }
    }
    command.arg(path);
    let output = command
        .output()
        .map_err(|error| format!("Could not start screencapture: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(screenshot_error_hint(&stderr));
    }
    normalize_jpeg(path)?;
    std::fs::read(path).map_err(|error| format!("Screenshot missing: {error}"))
}

#[cfg(target_os = "macos")]
fn screenshot_error_hint(stderr: &str) -> String {
    if stderr.contains("not permitted")
        || stderr.contains("permission")
        || stderr.contains("Screen Recording")
    {
        "Screenshot blocked: grant Screen Recording to this app in System Settings → Privacy & Security, then retry."
            .to_string()
    } else {
        format!("screencapture failed: {}", stderr.trim())
    }
}

/// Bound capture size for context: 1560px wide JPEG at quality 70.
#[cfg(target_os = "macos")]
fn normalize_jpeg(path: &Path) -> Result<(), String> {
    let output = std::process::Command::new("/usr/bin/sips")
        .args([
            "-Z",
            &SCREENSHOT_WIDTH.to_string(),
            "-s",
            "formatOptions",
            &SCREENSHOT_JPEG_QUALITY.to_string(),
        ])
        .arg(path)
        .output()
        .map_err(|error| format!("Could not start sips: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "sips normalize failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Dimensions via `sips` (JPEG headers need an SOF walk; the CLI is cheaper).
#[cfg(target_os = "macos")]
fn jpeg_dimensions(path: &Path) -> Option<String> {
    let output = std::process::Command::new("/usr/bin/sips")
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(path)
        .output()
        .ok()?;
    parse_sips_dimensions(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "macos")]
fn parse_sips_dimensions(text: &str) -> Option<String> {
    let mut width = None;
    let mut height = None;
    for line in text.lines() {
        let mut parts = line.split(':');
        match (parts.next(), parts.next()) {
            (Some(key), Some(value)) if key.trim() == "pixelWidth" => {
                width = value.trim().parse::<u32>().ok()
            }
            (Some(key), Some(value)) if key.trim() == "pixelHeight" => {
                height = value.trim().parse::<u32>().ok()
            }
            _ => {}
        }
    }
    Some(format!("{}x{}", width?, height?))
}

#[cfg(target_os = "macos")]
fn perform_act(intent: &ComputerAct, pid: Option<i32>) -> Result<String, String> {
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, KeyCode,
        ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;

    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|_| "Could not create an input event source (grant Accessibility access in System Settings → Privacy & Security).".to_string())?;
    // Background delivery posts straight to the target process: the cursor
    // never moves and focus never changes. Untargeted acts use the HID
    // stream (foreground behavior).
    let post = |event: &CGEvent| match pid {
        Some(pid) => event.post_to_pid(pid),
        None => event.post(CGEventTapLocation::HID),
    };
    // Fire-and-forget delivery cannot confirm Chromium/Electron targets, which
    // drop per-PID clicks at the renderer boundary; say so in the result.
    let background_note = pid
        .is_some()
        .then_some(" (background — cursor untouched; Chromium/Electron targets may ignore it)");
    let point = |x: f64, y: f64| CGPoint::new(x, y);
    let flags = |modifiers: &[String]| {
        let mut flags = CGEventFlags::empty();
        for modifier in modifiers {
            match modifier.as_str() {
                "shift" => flags |= CGEventFlags::CGEventFlagShift,
                "ctrl" => flags |= CGEventFlags::CGEventFlagControl,
                "alt" => flags |= CGEventFlags::CGEventFlagAlternate,
                "cmd" => flags |= CGEventFlags::CGEventFlagCommand,
                _ => {}
            }
        }
        flags
    };
    match intent {
        ComputerAct::Move { x, y } => {
            let event = CGEvent::new_mouse_event(
                source,
                CGEventType::MouseMoved,
                point(*x, *y),
                CGMouseButton::Left,
            )
            .map_err(|_| "Could not create mouse event.".to_string())?;
            post(&event);
            Ok(format!(
                "Moved pointer to ({x:.0}, {y:.0}).{}",
                background_note.unwrap_or("")
            ))
        }
        ComputerAct::Click { x, y, modifiers } | ComputerAct::DoubleClick { x, y, modifiers } => {
            let clicks = usize::from(matches!(intent, ComputerAct::DoubleClick { .. }));
            let event_flags = flags(modifiers);
            for _ in 0..=clicks {
                let down = CGEvent::new_mouse_event(
                    source.clone(),
                    CGEventType::LeftMouseDown,
                    point(*x, *y),
                    CGMouseButton::Left,
                )
                .map_err(|_| "Could not create mouse event.".to_string())?;
                down.set_flags(event_flags);
                post(&down);
                std::thread::sleep(Duration::from_millis(30));
                let up = CGEvent::new_mouse_event(
                    source.clone(),
                    CGEventType::LeftMouseUp,
                    point(*x, *y),
                    CGMouseButton::Left,
                )
                .map_err(|_| "Could not create mouse event.".to_string())?;
                up.set_flags(event_flags);
                post(&up);
                std::thread::sleep(Duration::from_millis(60));
            }
            Ok(if clicks == 0 {
                format!("Clicked ({x:.0}, {y:.0}).{}", background_note.unwrap_or(""))
            } else {
                format!(
                    "Double-clicked ({x:.0}, {y:.0}).{}",
                    background_note.unwrap_or("")
                )
            })
        }
        ComputerAct::Scroll { dx, dy } => {
            // Vertical wheel first (macOS natural order), then horizontal.
            for (wheel1, wheel2) in [(dy.round() as i32, 0), (0, dx.round() as i32)] {
                if wheel1 == 0 && wheel2 == 0 {
                    continue;
                }
                let event = CGEvent::new_scroll_event(
                    source.clone(),
                    ScrollEventUnit::PIXEL,
                    2,
                    wheel1,
                    wheel2,
                    0,
                )
                .map_err(|_| "Could not create scroll event.".to_string())?;
                post(&event);
            }
            Ok(format!(
                "Scrolled by ({dx:.0}, {dy:.0}).{}",
                background_note.unwrap_or("")
            ))
        }
        ComputerAct::Type { text } => {
            let mut typed = 0usize;
            for character in text.chars() {
                let mut buffer = [0u8; 4];
                let encoded = character.encode_utf8(&mut buffer);
                let down = CGEvent::new_keyboard_event(source.clone(), 0, true)
                    .map_err(|_| "Could not create keyboard event.".to_string())?;
                down.set_string(encoded);
                post(&down);
                let up = CGEvent::new_keyboard_event(source.clone(), 0, false)
                    .map_err(|_| "Could not create keyboard event.".to_string())?;
                post(&up);
                typed += 1;
            }
            Ok(format!(
                "Typed {typed} characters.{}",
                background_note.unwrap_or("")
            ))
        }
        ComputerAct::Press { key, modifiers } => {
            let event_flags = flags(modifiers);
            // Single characters (cmd+L, ctrl+C) ride the unicode path with
            // modifier flags; named keys use hardware keycodes.
            if key.chars().count() == 1 {
                let down = CGEvent::new_keyboard_event(source.clone(), 0, true)
                    .map_err(|_| "Could not create keyboard event.".to_string())?;
                down.set_flags(event_flags);
                down.set_string(key);
                post(&down);
                let up = CGEvent::new_keyboard_event(source, 0, false)
                    .map_err(|_| "Could not create keyboard event.".to_string())?;
                up.set_flags(event_flags);
                post(&up);
                return Ok(format!(
                    "Pressed {}{}.{}",
                    if modifiers.is_empty() {
                        String::new()
                    } else {
                        format!("{}+", modifiers.join("+"))
                    },
                    key,
                    background_note.unwrap_or("")
                ));
            }
            let keycode = match key.as_str() {
                "Enter" => KeyCode::RETURN,
                "Escape" => KeyCode::ESCAPE,
                "Tab" => KeyCode::TAB,
                "Space" => KeyCode::SPACE,
                "Backspace" | "Delete" => KeyCode::DELETE,
                "Up" => KeyCode::UP_ARROW,
                "Down" => KeyCode::DOWN_ARROW,
                "Left" => KeyCode::LEFT_ARROW,
                "Right" => KeyCode::RIGHT_ARROW,
                _ => return Err(format!("Unsupported key: {key}")),
            };
            let event_flags = flags(modifiers);
            let down = CGEvent::new_keyboard_event(source.clone(), keycode, true)
                .map_err(|_| "Could not create keyboard event.".to_string())?;
            down.set_flags(event_flags);
            post(&down);
            let up = CGEvent::new_keyboard_event(source, keycode, false)
                .map_err(|_| "Could not create keyboard event.".to_string())?;
            up.set_flags(event_flags);
            post(&up);
            Ok(format!("Pressed {key}.{}", background_note.unwrap_or("")))
        }
    }
}

pub(crate) struct ComputerCapability {
    pub(crate) permissions: Option<Arc<PermissionManager>>,
}

impl Capability for ComputerCapability {
    fn id(&self) -> &str {
        "computer"
    }

    fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        vec![Arc::new(ComputerToolExecutor::new(
            self.permissions.clone(),
        ))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn act_validation_accepts_click() {
        let act = parse_computer_act(r#"{"action":"click","x":120,"y":300}"#).unwrap();
        assert_eq!(
            act,
            ComputerAct::Click {
                x: 120.0,
                y: 300.0,
                modifiers: Vec::new()
            }
        );
        assert_eq!(act.approval_title(), "Click at (120, 300)");
    }

    #[test]
    fn act_validation_rejects_bad_input() {
        assert!(parse_computer_act(r#"{"action":"click","x":"left"}"#).is_err());
        assert!(parse_computer_act(r#"{"action":"click"}"#).is_err());
        assert!(parse_computer_act(r#"{"action":"scroll"}"#).is_err());
        assert!(parse_computer_act(r#"{"action":"scroll","dx":0,"dy":0}"#).is_err());
        assert!(parse_computer_act(r#"{"action":"type","text":""}"#).is_err());
        assert!(parse_computer_act(r#"{"action":"press","key":"F13"}"#).is_err());
        assert!(
            parse_computer_act(r#"{"action":"press","key":"Enter","modifiers":["super"]}"#)
                .is_err()
        );
        assert!(parse_computer_act(r#"{"action":"dance"}"#).is_err());
    }

    #[test]
    fn act_validation_caps_type_length() {
        let long = "x".repeat(MAX_TYPE_CHARS + 1);
        assert!(parse_computer_act(&format!(
            r#"{{"action":"type","text":{}}}"#,
            serde_json::json!(long)
        ))
        .is_err());
    }

    #[test]
    fn press_title_lists_modifiers() {
        let act =
            parse_computer_act(r#"{"action":"press","key":"Enter","modifiers":["cmd"]}"#).unwrap();
        assert_eq!(act.approval_title(), "Press cmd+Enter");
    }

    #[test]
    fn press_accepts_single_character_combos() {
        let act =
            parse_computer_act(r#"{"action":"press","key":"L","modifiers":["cmd"]}"#).unwrap();
        assert!(matches!(
            act,
            ComputerAct::Press { ref key, .. } if key == "L"
        ));
        assert_eq!(act.approval_title(), "Press cmd+L");
        assert!(parse_computer_act(r#"{"action":"press","key":"F13"}"#).is_err());
        assert!(parse_computer_act(r#"{"action":"press","key":"ab"}"#).is_err());
    }

    #[test]
    fn act_target_rejects_null_window() {
        assert_eq!(
            parse_act_target(r#"{"action":"click","x":1,"y":2,"target":16958}"#).unwrap(),
            Some(16958)
        );
        // 0 is kCGNullWindowID: never a real target, always a caller default.
        assert!(parse_act_target(r#"{"action":"click","x":1,"y":2,"target":0}"#).is_err());
    }

    #[test]
    fn act_target_parsing() {
        assert_eq!(
            parse_act_target(r#"{"action":"click","x":1,"y":2}"#).unwrap(),
            None
        );
        assert_eq!(
            parse_act_target(r#"{"action":"click","x":1,"y":2,"target":16958}"#).unwrap(),
            Some(16958)
        );
        assert!(parse_act_target(r#"{"action":"click","target":-1}"#).is_err());
        assert!(parse_act_target(r#"{"action":"click","target":"x"}"#).is_err());
    }

    #[test]
    fn resolve_without_target_keeps_foreground_delivery() {
        let intent = parse_computer_act(r#"{"action":"click","x":10,"y":20}"#).unwrap();
        let targeted = resolve_target(intent, None).unwrap();
        assert_eq!(targeted.pid, None);
        assert_eq!(targeted.app, None);
        assert!(matches!(
            targeted.intent,
            ComputerAct::Click {
                x: 10.0,
                y: 20.0,
                ..
            }
        ));
    }

    #[test]
    fn resolve_stale_target_errors_helpfully() {
        let intent = parse_computer_act(r#"{"action":"click","x":10,"y":20}"#).unwrap();
        let error = resolve_target(intent, Some(2_000_000_000)).unwrap_err();
        #[cfg(target_os = "macos")]
        assert!(error.contains("re-list"), "unexpected: {error}");
        #[cfg(not(target_os = "macos"))]
        assert!(error.contains("macOS"), "unexpected: {error}");
    }

    #[test]
    fn definitions_cover_all_tools() {
        let definitions = computer_tool_definitions();
        let names: Vec<_> = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                COMPUTER_STATUS_TOOL,
                COMPUTER_WINDOWS_TOOL,
                COMPUTER_SCREENSHOT_TOOL,
                COMPUTER_ACT_TOOL,
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_window_list_parses() {
        // No TCC prompt: CGWindowList is metadata-only.
        let result = mac::list_windows();
        assert!(result.is_ok(), "window list failed: {result:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sips_dimensions_parse() {
        let sample = "/tmp/shot.jpg\n  pixelWidth: 1560\n  pixelHeight: 960\n";
        assert_eq!(parse_sips_dimensions(sample).as_deref(), Some("1560x960"));
        assert_eq!(parse_sips_dimensions("garbage"), None);
    }

    #[test]
    fn mirror_sidecar_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("computer-1.jpg");
        std::fs::write(&path, b"fake-jpeg").unwrap();
        write_mirror_sidecar(dir.path(), Some(&path), "Screenshot live");
        let sidecar: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("latest.json")).unwrap())
                .unwrap();
        assert_eq!(sidecar["action"], "Screenshot live");
        assert!(sidecar["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("computer-1.jpg")));
        assert!(sidecar["ts_ms"].as_u64().is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn global_previews_dir_is_shared_location() {
        let dir = global_previews_dir().expect("home dir present");
        assert!(dir.ends_with("previews"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn debug_composite_apis() {
        use core_foundation::array::CFArray;
        use core_foundation::base::{CFType, TCFType};
        use core_foundation::number::CFNumber;
        use core_graphics::display::CGDisplay;
        use core_graphics::window::{
            create_image, create_image_from_array, create_window_list, kCGNullWindowID,
            kCGWindowImageDefault, kCGWindowListOptionOnScreenOnly,
        };
        let bounds = CGDisplay::main().bounds();
        eprintln!("bounds: {:?}", bounds);
        let single = create_image(
            bounds,
            kCGWindowListOptionOnScreenOnly,
            kCGNullWindowID,
            kCGWindowImageDefault,
        );
        eprintln!("single-call image: {}", single.is_some());
        if let Some(image) = single {
            eprintln!("size: {}x{}", image.width(), image.height());
        }
        // Raw system list, unfiltered: ItemRef<u32> derefs to the id value.
        let all = create_window_list(kCGWindowListOptionOnScreenOnly, kCGNullWindowID);
        if let Some(all) = all {
            eprintln!("system window count: {}", all.len());
            let raw_ids: Vec<i32> = (0..all.len().min(5))
                .filter_map(|i| all.get(i).map(|item| *item as i32))
                .collect();
            eprintln!("raw ids: {:?}", raw_ids);
            // Variation 0: system array passed straight through.
            let all2 =
                create_window_list(kCGWindowListOptionOnScreenOnly, kCGNullWindowID).expect("list");
            let direct = create_image_from_array(bounds, all2.to_untyped(), kCGWindowImageDefault);
            eprintln!("from-array direct: {}", direct.is_some());
            // Variation 1: single window id.
            let one: Vec<CFType> = raw_ids
                .iter()
                .take(1)
                .map(|id| CFNumber::from(*id).as_CFType())
                .collect();
            let arr1 = CFArray::from_CFTypes(&one).to_untyped();
            let img1 = create_image_from_array(bounds, arr1, kCGWindowImageDefault);
            eprintln!("from-array single: {}", img1.is_some());
            // Variation 2: nominal resolution option.
            use core_graphics::window::kCGWindowImageNominalResolution;
            let typed: Vec<CFType> = raw_ids
                .iter()
                .map(|id| CFNumber::from(*id).as_CFType())
                .collect();
            let arr = CFArray::from_CFTypes(&typed).to_untyped();
            let img = create_image_from_array(bounds, arr, kCGWindowImageNominalResolution);
            eprintln!("from-array nominal: {}", img.is_some());
        }
    }

    /// Live composite to BGRA: needs Screen Recording TCC, so ignored in CI.
    /// Proves the bitmap-context path yields opaque, bounded, non-blank
    /// frames with the geometry the mirror needs to map points.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn live_composite_scales_to_bgra() {
        use crate::computer_live::LIVE_FRAME_MAX_WIDTH;
        let composite = super::mac::composite_target(
            crate::computer_stream::StreamTarget::Display,
            super::mac::CaptureResolution::Nominal,
        )
        .expect("display composite");
        let (bgra, width, height) = composite.bgra(LIVE_FRAME_MAX_WIDTH).expect("bgra frame");
        assert!(width <= LIVE_FRAME_MAX_WIDTH && width > 0 && height > 0);
        assert_eq!(bgra.len(), (width * height * 4) as usize);
        // The backdrop fill makes every pixel opaque, uncovered desktop
        // included, which is what gpui's straight-alpha blend needs. A few
        // pixels land on 254 after CoreGraphics resamples in the display's
        // colour space; that is rounding, not transparency.
        assert!(bgra.chunks_exact(4).all(|pixel| pixel[3] >= 250), "opaque");
        assert!(composite.points_size.0 > 0.0);
        let started = std::time::Instant::now();
        for _ in 0..10 {
            let composite = super::mac::composite_target(
                crate::computer_stream::StreamTarget::Display,
                super::mac::CaptureResolution::Nominal,
            )
            .expect("display composite");
            composite.bgra(LIVE_FRAME_MAX_WIDTH).expect("bgra frame");
        }
        eprintln!(
            "live tier (nominal composite + bgra): {:.1}ms per frame at {}x{}",
            started.elapsed().as_secs_f64() * 100.0,
            width,
            height
        );
        let started = std::time::Instant::now();
        for _ in 0..5 {
            super::capture_composited_for_target(
                crate::computer_stream::StreamTarget::Display,
                super::SCREENSHOT_WIDTH,
                super::SCREENSHOT_JPEG_QUALITY,
            )
            .expect("model jpeg");
        }
        eprintln!(
            "model tier (best composite + jpeg): {:.1}ms per frame",
            started.elapsed().as_secs_f64() * 200.0
        );
    }

    /// Live capture: needs Screen Recording TCC, so ignored in CI. Run by
    /// hand with `-- --ignored` to prove pixels flow end to end.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore]
    async fn live_screenshot_attaches_image() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".threadlane")).unwrap();
        std::fs::write(
            dir.path().join(".threadlane").join("permissions.json"),
            r#"{"computer_allowed":true}"#,
        )
        .unwrap();
        let (event_tx, _) = tokio::sync::broadcast::channel(4);
        let permissions = Arc::new(crate::permission::PermissionManager::new(
            dir.path().to_path_buf(),
            event_tx,
        ));
        assert!(permissions.computer_is_approved());
        let executor = ComputerToolExecutor::new(Some(permissions));
        let output = executor
            .execute_tool_with_output_in_workspace(COMPUTER_SCREENSHOT_TOOL, "{}", Some(dir.path()))
            .await
            .expect("handled")
            .expect("screenshot ok");
        assert_eq!(output.images.len(), 1);
        assert!(output.images[0]
            .data_url
            .starts_with("data:image/jpeg;base64,"));

        // Let the poller warm up, then prove the stream serves frames.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let frame =
            crate::computer_stream::fresh_frame(crate::computer_stream::StreamTarget::Display);
        assert!(
            frame.is_some(),
            "stream poller should have produced a display frame"
        );
    }

    /// Live resolution: read-only window lookup plus coordinate shift. No
    /// input is posted, so this is safe anywhere with a WindowServer.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn live_target_resolution_shifts_coordinates() {
        let ids = super::mac::capture_window_ids();
        let id = ids.into_iter().next().expect("a visible window");
        let (pid, owner, (origin_x, origin_y)) =
            super::mac::find_window(id).expect("window resolves");
        assert!(pid > 0, "unexpected pid for {owner}");
        let intent = parse_computer_act(r#"{"action":"click","x":10,"y":20}"#).unwrap();
        let targeted = resolve_target(intent, Some(id as i64)).unwrap();
        assert_eq!(targeted.pid, Some(pid));
        assert_eq!(targeted.app.as_deref(), Some(owner.as_str()));
        assert!(matches!(
            targeted.intent,
            ComputerAct::Click { x, y, .. } if x == 10.0 + origin_x && y == 20.0 + origin_y
        ));

        // Window captures are cropped to the window, not the display canvas.
        let infos = super::mac::window_infos().unwrap_or_default();
        let big = infos
            .into_iter()
            .find(|window| window.bounds.2 > 400.0 && window.bounds.3 > 300.0)
            .expect("a sizable window");
        let (bytes, width, height, src_points) = super::capture_composited_for_target(
            crate::computer_stream::StreamTarget::Window(big.id as u32),
            super::SCREENSHOT_WIDTH,
            super::SCREENSHOT_JPEG_QUALITY,
        )
        .expect("window crop captures");
        assert!(!bytes.is_empty());
        let aspect = f64::from(width) / f64::from(height.max(1));
        let expected = big.bounds.2 / big.bounds.3.max(1.0);
        assert!(
            (aspect - expected).abs() < 0.05,
            "crop aspect {aspect} should match window {expected}"
        );
        assert!(
            (src_points - big.bounds.2).abs() < 1.0,
            "source width should be window points"
        );
    }

    #[tokio::test]
    async fn unknown_tool_is_not_claimed() {
        let executor = ComputerToolExecutor::new(None);
        assert!(executor.execute_tool("read_file", "{}").await.is_none());
    }

    #[tokio::test]
    async fn status_reports_availability() {
        let executor = ComputerToolExecutor::new(None);
        let result = executor
            .execute_tool(COMPUTER_STATUS_TOOL, "{}")
            .await
            .expect("handled");
        assert!(result.is_ok());
    }

    #[test]
    fn computer_tools_survive_core_schema_filter() {
        // Regression shape of the browser discovery bug: core_tool_schema_mode
        // strips every non-core schema from the provider payload.
        let dir = tempfile::tempdir().unwrap();
        let session_file = dir.path().join("session.jsonl");
        let agent =
            crate::coding_agent::CodingAgent::new(crate::coding_agent::CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "gpt-4o".into(),
                work_dir: dir.path().to_path_buf(),
                session_file: Some(session_file),
                system_prompt: Default::default(),
                agent_config: None,
                coding_config: None,
                browser: crate::browser::BrowserBridge::unavailable(),
            });
        let names: Vec<String> = agent
            .agent
            .configured_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        for tool in [
            COMPUTER_STATUS_TOOL,
            COMPUTER_WINDOWS_TOOL,
            COMPUTER_SCREENSHOT_TOOL,
            COMPUTER_ACT_TOOL,
        ] {
            assert!(
                names.iter().any(|name| name == tool),
                "model-visible schemas must include {tool}"
            );
        }
    }
}
