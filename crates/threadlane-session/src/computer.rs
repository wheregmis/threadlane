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
const MAX_IMAGE_BYTES: usize = 2_000_000;
/// Capture width bound: enough for UI legibility, small enough for context.
const SCREENSHOT_WIDTH: u32 = 1_560;
const SCREENSHOT_JPEG_QUALITY: u8 = 70;

pub(crate) struct ComputerToolExecutor {
    permissions: Option<Arc<PermissionManager>>,
}

impl ComputerToolExecutor {
    pub(crate) fn new(permissions: Option<Arc<PermissionManager>>) -> Self {
        Self { permissions }
    }

    async fn approve(&self, title: String, detail: String) -> Result<(), String> {
        let Some(permissions) = &self.permissions else {
            return Err(
                "Computer use has no permission channel in this session.".to_string(),
            );
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
                        "description": "Key for press: Enter, Escape, Tab, Space, Backspace, Delete, Up, Down, Left, Right."
                    },
                    "modifiers": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["shift", "ctrl", "alt", "cmd"] },
                        "description": "Optional modifiers held for click/press."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        ),
    ]
    .into()
}

/// Validated `computer_act` intent. Pure and cross-platform for testability;
/// execution is macOS-only.
#[derive(Debug, PartialEq)]
pub(crate) enum ComputerAct {
    Click { x: f64, y: f64, modifiers: Vec<String> },
    DoubleClick { x: f64, y: f64, modifiers: Vec<String> },
    Move { x: f64, y: f64 },
    Scroll { dx: f64, dy: f64 },
    Type { text: String },
    Press { key: String, modifiers: Vec<String> },
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
    "Enter", "Escape", "Tab", "Space", "Backspace", "Delete", "Up", "Down", "Left", "Right",
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
                            "`computer_act` modifiers must be shift, ctrl, alt, or cmd."
                                .to_string()
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
            if !VALID_KEYS.contains(&key) {
                return Err(format!(
                    "`computer_act` press key must be one of {}.",
                    VALID_KEYS.join(", ")
                ));
            }
            Ok(ComputerAct::Press {
                key: key.to_string(),
                modifiers: parse_modifiers(&parsed)?,
            })
        }
        _ => Err("`computer_act` action must be one of click, double_click, move, scroll, type, press.".into()),
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
        for window in window_infos()?.into_iter().filter(|window| {
            window.onscreen && window.pid != own_pid && window.id >= 0
        }) {
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

    /// Composite every on-screen window except ours into a bounded JPEG.
    /// Returns (jpeg bytes, width, height). Unlike `screencapture` this never
    /// includes Threadlane's own windows, so a mirror popup cannot recurse.
    /// Fails closed (Err) when the composite is blank, e.g. without Screen
    /// Recording permission — the caller falls back to `screencapture`.
    pub(super) fn capture_composited_jpeg(
        max_width: u32,
        quality: u8,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        use core_foundation::array::CFArray;
        use core_foundation::number::CFNumber;
        use core_graphics::display::CGDisplay;
        use core_graphics::window::{create_image_from_array, kCGWindowImageDefault};

        let ids: Vec<CFNumber> = capture_window_ids()
            .into_iter()
            .map(CFNumber::from)
            .collect();
        if ids.is_empty() {
            return Err("No capturable windows.".to_string());
        }
        let array = CFArray::from_CFTypes(&ids);
        // create_image_from_array takes the untyped array; rewrap the same ref.
        let array: CFArray =
            unsafe { CFArray::wrap_under_get_rule(array.as_concrete_TypeRef()) };
        let bounds = CGDisplay::main().bounds();
        let image = create_image_from_array(bounds, array, kCGWindowImageDefault)
            .ok_or_else(|| "Window composite failed.".to_string())?;
        let width = image.width() as u32;
        let height = image.height() as u32;
        if width == 0 || height == 0 {
            return Err("Window composite is empty.".to_string());
        }
        if image.bits_per_pixel() != 32 {
            return Err(format!(
                "Unexpected composite depth: {}bpp.",
                image.bits_per_pixel()
            ));
        }
        let stride = image.bytes_per_row();
        let pixels = image.data();
        let raw = pixels.bytes();
        // BGRA, honoring row stride, alpha assumed opaque (window server output).
        let mut rgb = Vec::with_capacity((width as usize) * (height as usize) * 3);
        for y in 0..height as usize {
            let row = &raw[y * stride..];
            for x in 0..width as usize {
                let offset = x * 4;
                rgb.push(row[offset + 2]);
                rgb.push(row[offset + 1]);
                rgb.push(row[offset]);
            }
        }
        if is_blank(&rgb) {
            return Err(
                "Window composite is blank (Screen Recording permission likely missing)."
                    .to_string(),
            );
        }
        encode_bounded_jpeg(&rgb, width, height, max_width, quality)
    }

    /// True when every pixel is identical: a denied TCC capture composites black.
    fn is_blank(rgb: &[u8]) -> bool {
        let (first, rest) = match rgb.split_first_chunk::<3>() {
            Some(pair) => pair,
            None => return true,
        };
        rest.chunks_exact(3).all(|pixel| pixel == first)
    }

    fn encode_bounded_jpeg(
        rgb: &[u8],
        width: u32,
        height: u32,
        max_width: u32,
        quality: u8,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        use image::codecs::jpeg::JpegEncoder;
        use image::{ImageBuffer, Rgb};

        let buffer: ImageBuffer<Rgb<u8>, _> =
            ImageBuffer::from_raw(width, height, rgb.to_vec())
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

    /// Read one window dictionary by comparing key names. Comparing
    /// content (not pointers) keeps this independent of CF key-callback
    /// details; the kCGWindow* names are stable API.
    struct WindowInfo {
        id: i32,
        owner: String,
        title: String,
        pid: i32,
        layer: i32,
        alpha: f64,
        bounds: (f64, f64, f64, f64),
        onscreen: bool,
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
                let value = unsafe {
                    core_foundation::base::CFType::wrap_under_get_rule(*value)
                };
                if name == "kCGWindowBounds" {
                    if let Some(rect) = value.downcast::<CFDictionary>() {
                        let (rect_keys, rect_values) = rect.get_keys_and_values();
                        let mut components = std::collections::HashMap::new();
                        for (rect_key, rect_value) in
                            rect_keys.iter().zip(rect_values.iter())
                        {
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
                        number.to_i32().or_else(|| number.to_f64().map(|value| value as i32))
                    })
                    .unwrap_or(fallback)
            };
            WindowInfo {
                id: integer("kCGWindowNumber", -1),
                owner: strings.get("kCGWindowOwnerName").cloned().unwrap_or_default(),
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

        fn window_infos() -> Result<Vec<WindowInfo>, String> {
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
                let dict = unsafe {
                    core_foundation::base::CFType::wrap_under_get_rule(raw)
                };
                let Some(dict) = dict.downcast::<CFDictionary>() else {
                    continue;
                };
                windows.push(read_dictionary(&dict));
            }
            Ok(windows)
        }
}

#[cfg(target_os = "macos")]
fn list_windows() -> Result<String, String> {    mac::list_windows()
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
            format!(
                "Capture {target}. You will see the image; the agent receives it too."
            ),
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
        // Composited capture first: it excludes our own windows, so a mirror
        // popup cannot recurse. Falls back to `screencapture` (which owns the
        // TCC prompt) when the composite is unavailable or blank.
        let (bytes, dims) = match mac::capture_composited_jpeg(
            SCREENSHOT_WIDTH,
            SCREENSHOT_JPEG_QUALITY,
        ) {
            Ok((bytes, width, height)) => {
                std::fs::write(&path, &bytes)
                    .map_err(|error| format!("Could not save screenshot: {error}"))?;
                (bytes, format!("{width}x{height}"))
            }
            Err(_) => {
                let bytes = tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || capture_jpeg(window_id, &path)
                })
                .await
                .map_err(|error| format!("Screenshot task failed: {error}"))??;
                let dims = jpeg_dimensions(&path).unwrap_or_else(|| "unknown size".to_string());
                (bytes, dims)
            }
        };
        write_mirror_sidecar(&dir, Some(&path), &format!("Screenshot {target}"));
        let content = format!(
            "Screenshot saved to {} ({} pixels, {} bytes).",
            path.display(),
            dims,
            bytes.len()
        );
        if bytes.len() > MAX_IMAGE_BYTES {
            return Ok(ToolOutput {
                content: format!(
                    "{content} Image exceeded the {MAX_IMAGE_BYTES}-byte model limit, so only metadata is attached."
                ),
                images: Vec::new(),
            });
        }
        use base64::Engine as _;
        let data_url = format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        Ok(ToolOutput {
            content,
            images: vec![threadlane_runtime::ImageAttachment {
                display_name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "screenshot.jpg".into()),
                data_url,
            }],
        })
    }

    async fn act(&self, args: &str, work_dir: Option<&Path>) -> Result<String, String> {
        let intent = parse_computer_act(args)?;
        let title = intent.approval_title();
        self.approve(title.clone(), format!("{title} on this Mac. Deny if the target looks wrong."))
            .await?;
        let outcome = tokio::task::spawn_blocking(move || perform_act(&intent))
            .await
            .map_err(|error| format!("Input task failed: {error}"))?;
        if let Ok(outcome) = &outcome {
            let dir = mac::previews_dir(work_dir);
            write_mirror_sidecar(&dir, None, &format!("{title} — {outcome}"));
        }
        outcome
    }
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
fn perform_act(intent: &ComputerAct) -> Result<String, String> {
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, KeyCode,
        ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;

    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|_| "Could not create an input event source (grant Accessibility access in System Settings → Privacy & Security).".to_string())?;
    let post = |event: &CGEvent| event.post(CGEventTapLocation::HID);
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
            Ok(format!("Moved pointer to ({x:.0}, {y:.0})."))
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
                format!("Clicked ({x:.0}, {y:.0}).")
            } else {
                format!("Double-clicked ({x:.0}, {y:.0}).")
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
            Ok(format!("Scrolled by ({dx:.0}, {dy:.0})."))
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
            Ok(format!("Typed {typed} characters."))
        }
        ComputerAct::Press { key, modifiers } => {
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
            Ok(format!("Pressed {key}."))
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
        vec![Arc::new(ComputerToolExecutor::new(self.permissions.clone()))]
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
        assert!(parse_computer_act(r#"{"action":"press","key":"Enter","modifiers":["super"]}"#)
            .is_err());
        assert!(parse_computer_act(r#"{"action":"dance"}"#).is_err());
    }

    #[test]
    fn act_validation_caps_type_length() {
        let long = "x".repeat(MAX_TYPE_CHARS + 1);
        assert!(parse_computer_act(&format!(r#"{{"action":"type","text":{}}}"#, serde_json::json!(long))).is_err());
    }

    #[test]
    fn press_title_lists_modifiers() {
        let act =
            parse_computer_act(r#"{"action":"press","key":"Enter","modifiers":["cmd"]}"#).unwrap();
        assert_eq!(act.approval_title(), "Press cmd+Enter");
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
            .execute_tool_with_output_in_workspace(
                COMPUTER_SCREENSHOT_TOOL,
                "{}",
                Some(dir.path()),
            )
            .await
            .expect("handled")
            .expect("screenshot ok");
        assert_eq!(output.images.len(), 1);
        assert!(output.images[0].data_url.starts_with("data:image/jpeg;base64,"));
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
        let agent = crate::coding_agent::CodingAgent::new(crate::coding_agent::CodingAgentOptions {
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
