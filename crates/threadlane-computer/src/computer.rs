//! Computer use through the CUA driver (`cua-driver`), not raw OS APIs.
//!
//! Every window list, screenshot, accessibility snapshot, and input action
//! runs through [`crate::driver::driver_call`] (`cua-driver mcp --direct`
//! over MCP stdio). Approval-gated through [`crate::ComputerApproval`]:
//! screenshots, accessibility snapshots *with* pixels, and all input or
//! mutating calls require user approval before executing; read-only discovery
//! (window lists, permission status, tree-only snapshots) does not. The first
//! gated action prompts with Once/Always scopes and an Always grant is
//! remembered per project; unattended sessions deny by default, matching the
//! ACP reject-by-default policy.
//!
//! Screenshots reach the model as images attached to the tool result
//! alongside text metadata; the full file also lands in
//! `<work_dir>/.threadlane/previews/` for the user, with a `latest.json`
//! sidecar in the global previews dir so the GPUI mirror popup keeps showing
//! the last capture plus action line. There is no live video poller: the
//! driver's own agent-cursor overlay shows live input, and the mirror falls
//! back to the last screenshot. Every screenshot and act still publishes a
//! [`threadlane_protocol::live`] overlay so the user sees where a click lands.
//!
//! When no driver binary is installed every tool fails closed with an install
//! hint instead of touching OS input APIs directly.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use threadlane_protocol::{AgentToolDefinition, ImageAttachment, ToolExecutor, ToolOutput};

use crate::driver::{driver_available, driver_call, driver_version, DriverResult, DRIVER_MISSING_HINT};
use crate::{ComputerApproval, ComputerDecision};

pub const COMPUTER_STATUS_TOOL: &str = "computer_status";
pub const COMPUTER_WINDOWS_TOOL: &str = "computer_windows";
pub const COMPUTER_SCREENSHOT_TOOL: &str = "computer_screenshot";
pub const COMPUTER_ACT_TOOL: &str = "computer_act";
/// Accessibility snapshot of one window: element tree plus a grounding
/// screenshot. The cheap re-index path before element actions.
pub const COMPUTER_AX_TOOL: &str = "computer_ax";
/// Full-catalog passthrough: any `cua-driver` MCP tool by name. Read-only
/// discovery skips approval; everything else prompts like `computer_act`.
pub const CUA_CALL_TOOL: &str = "cua_call";

const MAX_TYPE_CHARS: usize = 4_000;
/// Screenshots larger than this ride as metadata only, never pixels.
pub const MAX_IMAGE_BYTES: usize = 2_000_000;
/// Tree text beyond this is truncated with a note (the driver already caps
/// the AX walk; this bounds the model-visible Markdown rendering).
const MAX_TREE_CHARS: usize = 24_000;

pub struct ComputerToolExecutor {
    permissions: Option<Arc<dyn ComputerApproval>>,
}

impl ComputerToolExecutor {
    pub fn new(permissions: Option<Arc<dyn ComputerApproval>>) -> Self {
        Self { permissions }
    }

    async fn approve(&self, title: String, detail: String) -> Result<(), String> {
        let Some(permissions) = &self.permissions else {
            return Err("Computer use has no permission channel in this session.".to_string());
        };
        match permissions.request_computer(&title, &detail).await {
            ComputerDecision::Deny => Err(format!(
                "The user denied this computer action ({title}). Do not retry it verbatim; ask how to proceed."
            )),
            ComputerDecision::AllowOnce | ComputerDecision::AllowAlways => Ok(()),
        }
    }
}

fn computer_tool_definitions() -> Arc<[AgentToolDefinition]> {
    vec![
        AgentToolDefinition::new(
            COMPUTER_STATUS_TOOL,
            "Report computer-use availability: CUA driver version, OS permission status, and window/screenshot/input support. Call this before computer_windows/computer_screenshot/computer_act on a new machine.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            COMPUTER_WINDOWS_TOOL,
            "List top-level windows: id, app, title, pid, and bounds. Window ids and pids from this list feed computer_screenshot, computer_ax, and computer_act.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "on_screen_only": {
                        "type": "boolean",
                        "description": "Drop windows not on the current Space. Default true."
                    }
                },
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            COMPUTER_SCREENSHOT_TOOL,
            "Capture the main display (or one window by id from computer_windows). You receive the image plus its path and dimensions; the user sees the same file and must approve each capture. Read click positions off the returned image. Prefer the embedded browser tools for web pages.",
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
            COMPUTER_AX_TOOL,
            "Accessibility snapshot of one window (pid + window_id from computer_windows): interactive element tree plus a grounding screenshot. Ground on both and cross-check — the tree lies on some surfaces (custom-drawn canvases, virtualized rows). Pass element indices from a fresh snapshot to cua_call element actions; indices expire on the next snapshot of the same window. Set include_screenshot:false for the cheap tree-only re-index before acting.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pid": { "type": "integer", "description": "Owner pid from computer_windows." },
                    "window_id": { "type": "integer", "description": "Window id from computer_windows." },
                    "query": { "type": "string", "description": "Case-insensitive filter: matching rows plus ancestors, indices unchanged." },
                    "max_elements": { "type": "integer", "description": "Cap the AX walk (default 2000). Lower for huge Electron trees." },
                    "include_screenshot": { "type": "boolean", "description": "Default true. False returns the tree only (no approval needed)." }
                },
                "required": ["pid", "window_id"],
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            COMPUTER_ACT_TOOL,
            "Control mouse and keyboard: click, double_click, move, scroll, type, press. Every call asks the user for approval first; denied actions must not be retried verbatim. Delivery is background-first via the CUA driver (cursor and focus usually untouched). Coordinates are screenshot pixels: targeted coordinates are window-local (read off a window screenshot/ax snapshot), untargeted ones are display pixels.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["click", "double_click", "move", "scroll", "type", "press"],
                        "description": "click/double_click/move need x,y. scroll needs dx/dy in pixels. type needs text. press needs key."
                    },
                    "x": { "type": "number", "description": "Screenshot x pixel (window-local when target is set, display pixel otherwise)." },
                    "y": { "type": "number", "description": "Screenshot y pixel." },
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
                        "description": "Optional window id from computer_windows. Coordinates become window-local and input is delivered to that app in the background. Omit for foreground display coordinates."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            CUA_CALL_TOOL,
            "Full CUA driver catalog passthrough: call any driver tool not covered above (element clicks via element_token, drag, hotkey, set_value, invoke_menu, clipboard_read/write, launch_app, bring_to_front, set_window_frame, verify_state, zoom, browser_* page tools, recording, sessions). Read-only discovery skips approval; everything else prompts first and denied actions must not be retried verbatim. Get element_token/snapshot_id from computer_ax; get pid/window_id from computer_windows.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tool": {
                        "type": "string",
                        "description": "Driver tool name: click, double_click, drag, move_cursor, scroll, type_text, set_value, press_key, hotkey, right_click, get_window_state, get_accessibility_tree, get_desktop_state, zoom, clipboard_read, clipboard_write, launch_app, kill_app, bring_to_front, set_window_frame, invoke_menu, verify_state, browser_navigate, browser_click, browser_type, browser_pointer, browser_dialog, browser_download, browser_set_input_files, browser_prepare, get_browser_state, get_screen_size, get_cursor_position, check_permissions, start_session, end_session, list_sessions, start_recording, stop_recording, and more (cua-driver list-tools)."
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Tool arguments object (element_token + snapshot_id for element actions, pid/window_id for window actions).",
                        "additionalProperties": true
                    }
                },
                "required": ["tool"],
                "additionalProperties": false
            }),
        ),
    ]
    .into()
}

/// Validated `computer_act` intent. Pure and cross-platform for testability;
/// execution goes through the CUA driver.
#[derive(Debug, PartialEq)]
pub enum ComputerAct {
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

pub fn parse_computer_act(args: &str) -> Result<ComputerAct, String> {
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
            // (cmd+L, ctrl+C): single characters ride press_key directly.
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

pub fn parse_act_target(args: &str) -> Result<Option<i64>, String> {
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

/// Driver modifier names: ours match except `alt`, which the driver spells
/// `option`.
fn driver_modifiers(modifiers: &[String]) -> Vec<String> {
    modifiers
        .iter()
        .map(|modifier| {
            if modifier == "alt" {
                "option".to_string()
            } else {
                modifier.clone()
            }
        })
        .collect()
}

/// Driver key names are lowercase (`return`, `escape`, …); single characters
/// pass through lowercased.
fn driver_key(key: &str) -> String {
    match key {
        "Enter" => "return".to_string(),
        "Escape" => "escape".to_string(),
        "Tab" => "tab".to_string(),
        "Space" => "space".to_string(),
        "Backspace" | "Delete" => "delete".to_string(),
        "Up" => "up".to_string(),
        "Down" => "down".to_string(),
        "Left" => "left".to_string(),
        "Right" => "right".to_string(),
        single => single.to_lowercase(),
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
            COMPUTER_STATUS_TOOL => Some(Ok(computer_status().await)),
            COMPUTER_WINDOWS_TOOL => Some(list_windows(args).await),
            COMPUTER_SCREENSHOT_TOOL => Some(
                self.screenshot_with_output(args, work_dir)
                    .await
                    .map(|output| output.content),
            ),
            COMPUTER_AX_TOOL => Some(
                self.ax_with_output(args, work_dir)
                    .await
                    .map(|output| output.content),
            ),
            COMPUTER_ACT_TOOL => Some(self.act(args, work_dir).await),
            CUA_CALL_TOOL => Some(
                self.cua_call_with_output(args, work_dir)
                    .await
                    .map(|output| output.content),
            ),
            _ => None,
        }
    }

    /// Image-bearing tools ride the rich path so pixels reach the provider
    /// payload; every other tool keeps the default string mapping.
    async fn execute_tool_with_output_in_workspace(
        &self,
        name: &str,
        args: &str,
        work_dir: Option<&Path>,
    ) -> Option<Result<ToolOutput, String>> {
        match name {
            COMPUTER_SCREENSHOT_TOOL => Some(self.screenshot_with_output(args, work_dir).await),
            COMPUTER_AX_TOOL => Some(self.ax_with_output(args, work_dir).await),
            CUA_CALL_TOOL => Some(self.cua_call_with_output(args, work_dir).await),
            _ => self
                .execute_tool_in_workspace(name, args, work_dir)
                .await
                .map(|result| result.map(ToolOutput::from)),
        }
    }
}

async fn computer_status() -> String {
    let Some(version) = driver_version().await else {
        return DRIVER_MISSING_HINT.to_string();
    };
    // Read-only probe: prompt:false never raises a system dialog.
    let permissions = driver_call("check_permissions", serde_json::json!({"prompt": false}))
        .await
        .ok();
    let permission_line = permissions
        .as_ref()
        .and_then(|result| result.structured.clone())
        .map(|structured| {
            let flag = |name: &str| {
                structured
                    .get(name)
                    .and_then(|value| value.as_bool())
                    .map(|granted| {
                        if granted {
                            "granted".to_string()
                        } else {
                            "missing".to_string()
                        }
                    })
                    .unwrap_or_else(|| "unknown".to_string())
            };
            let source = structured
                .get("source")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown identity");
            format!(
                "Accessibility {}, Screen Recording {} ({}).",
                flag("accessibility"),
                flag("screen_recording"),
                source
            )
        })
        .unwrap_or_else(|| "Permission status unavailable.".to_string());
    format!(
        "Computer use is available through the CUA driver ({version}): computer_windows lists top-level windows, computer_ax snapshots one window's accessibility tree plus screenshot, computer_screenshot captures the display or one window, computer_act clicks/types/presses keys background-first, cua_call reaches the rest of the driver catalog. {permission_line} Every screenshot and input action asks for approval first; if a macOS permission is missing, grant it in System Settings → Privacy & Security, then retry."
    )
}

/// One live `list_windows` row: (window_id, pid, app, title).
fn driver_window_rows(
    structured: Option<&serde_json::Value>,
) -> Vec<(i64, i64, String, String)> {
    let Some(windows) = structured
        .and_then(|structured| structured.get("windows"))
        .and_then(|windows| windows.as_array())
    else {
        return Vec::new();
    };
    windows
        .iter()
        .filter_map(|window| {
            Some((
                window.get("window_id")?.as_i64()?,
                window.get("pid")?.as_i64()?,
                window
                    .get("app_name")
                    .and_then(|app| app.as_str())
                    .unwrap_or("")
                    .to_string(),
                window
                    .get("title")
                    .and_then(|title| title.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect()
}

async fn list_windows(args: &str) -> Result<String, String> {
    if !driver_available() {
        return Err(DRIVER_MISSING_HINT.to_string());
    }
    let on_screen_only = serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|value| value.get("on_screen_only")?.as_bool())
        .unwrap_or(true);
    let result = driver_call(
        "list_windows",
        serde_json::json!({"on_screen_only": on_screen_only}),
    )
    .await?;
    let structured = result.structured.clone().unwrap_or(serde_json::Value::Null);
    let windows = structured
        .get("windows")
        .and_then(|windows| windows.as_array())
        .cloned()
        .unwrap_or_default();
    let mut rows = Vec::new();
    for window in windows.iter().take(50) {
        let field = |name: &str| {
            window
                .get(name)
                .map(|value| value.to_string().trim_matches('"').to_string())
                .unwrap_or_default()
        };
        let bounds = window.get("bounds");
        let coord = |name: &str| {
            bounds
                .and_then(|bounds| bounds.get(name))
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
        };
        rows.push(format!(
            "id={} app={:?} title={:?} pid={} x={:.0} y={:.0} w={:.0} h={:.0} z={} on_screen={}",
            field("window_id"),
            field("app_name"),
            field("title"),
            field("pid"),
            coord("x"),
            coord("y"),
            coord("width"),
            coord("height"),
            field("z_index"),
            field("is_on_screen"),
        ));
    }
    if rows.is_empty() {
        return Ok("No top-level windows found.".to_string());
    }
    Ok(format!(
        "Top-level windows (Threadlane's own windows are hidden from some drivers — never act on them):\n{}",
        rows.join("\n")
    ))
}

/// Resolve a window id to its owner pid through a fresh driver listing.
/// Window ids go stale on relaunch; the error tells the model to re-list.
async fn resolve_window_pid(window_id: i64) -> Result<i64, String> {
    if !driver_available() {
        return Err(DRIVER_MISSING_HINT.to_string());
    }
    let result = driver_call("list_windows", serde_json::json!({})).await?;
    driver_window_rows(result.structured.as_ref())
        .into_iter()
        .find(|(id, _, _, _)| *id == window_id)
        .map(|(_, pid, _, _)| pid)
        .ok_or_else(|| {
            format!("Window {window_id} is gone; re-list with computer_windows and pick a live id.")
        })
}

fn previews_dir(work_dir: Option<&Path>) -> PathBuf {
    work_dir
        .unwrap_or_else(|| Path::new("."))
        .join(".threadlane")
        .join("previews")
}

/// Global live-mirror dir (`~/.threadlane/previews/`): the popup is global
/// while sessions live in per-project worktrees, so `latest.json` lives
/// here. Timestamped history files stay per-project.
pub fn global_previews_dir() -> Option<PathBuf> {
    threadlane_project::default_global_threadlane_dir().map(|dir| dir.join("previews"))
}

/// Resolves the shared mirror directory, falling back to the active project's
/// preview directory when a global Threadlane directory is unavailable.
pub fn resolve_previews_dir(work_dir: Option<&Path>) -> Option<PathBuf> {
    global_previews_dir().or_else(|| work_dir.map(|path| previews_dir(Some(path))))
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

fn preview_path(dir: &Path, extension: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    dir.join(format!("computer-{stamp}.{extension}"))
}

/// Save one driver image block for the user and the model. Returns
/// (file path, data-url image) unless the bytes exceed the model cap, in
/// which case the model gets metadata only.
fn persist_image(
    mime: &str,
    base64: &str,
    dir: &Path,
) -> Result<(PathBuf, Option<ImageAttachment>), String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64.trim())
        .map_err(|error| format!("Driver returned an undecodable image: {error}"))?;
    let extension = if mime.contains("png") { "png" } else { "jpg" };
    let path = preview_path(dir, extension);
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("Could not create preview dir: {error}"))?;
    std::fs::write(&path, &bytes)
        .map_err(|error| format!("Could not save screenshot: {error}"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Ok((path, None));
    }
    let display_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("screenshot.{extension}"));
    Ok((
        path,
        Some(ImageAttachment {
            display_name,
            data_url: format!("data:{mime};base64,{base64}"),
        }),
    ))
}

/// Attach driver images to a [`ToolOutput`]: persist each for the user,
/// attach each within the model byte cap, and note skipped ones.
fn attach_driver_images(
    result: &DriverResult,
    dir: &Path,
    label: &str,
) -> Result<(Vec<PathBuf>, Vec<ImageAttachment>, String), String> {
    let mut paths = Vec::new();
    let mut images = Vec::new();
    let mut skipped = 0usize;
    for image in &result.images {
        let (path, attached) = persist_image(&image.mime, &image.base64, dir)?;
        paths.push(path);
        match attached {
            Some(attachment) => images.push(attachment),
            None => skipped += 1,
        }
    }
    let mut note = format!(
        "{} ({} image{} saved under {})",
        label,
        paths.len(),
        if paths.len() == 1 { "" } else { "s" },
        dir.display(),
    );
    if skipped > 0 {
        note.push_str(&format!(
            "; {skipped} image(s) exceeded the {MAX_IMAGE_BYTES}-byte model limit, metadata only"
        ));
    }
    Ok((paths, images, note))
}

/// Flash the mirror: the model just took a picture.
fn note_screenshot(label: &str) {
    threadlane_protocol::live::publish_overlay(
        threadlane_protocol::live::LiveOverlayKind::Screenshot,
        None,
        None,
        label,
    );
}

impl ComputerToolExecutor {
    async fn screenshot_with_output(
        &self,
        args: &str,
        work_dir: Option<&Path>,
    ) -> Result<ToolOutput, String> {
        if !driver_available() {
            return Err(DRIVER_MISSING_HINT.to_string());
        }
        let window_id: Option<i64> = serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|value| value.get("window_id")?.as_i64());
        let target_label = match window_id {
            Some(id) => format!("window {id}"),
            None => "the main display".to_string(),
        };
        self.approve(
            format!("Screenshot {target_label}"),
            format!("Capture {target_label} via the CUA driver. You will see the image; the agent receives it too."),
        )
        .await?;
        let (tool, tool_args) = match window_id {
            Some(id) => {
                let pid = resolve_window_pid(id).await?;
                crate::mirror::ensure_feed_window(id);
                (
                    "get_window_state",
                    serde_json::json!({"pid": pid, "window_id": id}),
                )
            }
            None => {
                crate::mirror::ensure_feed_display();
                ("get_desktop_state", serde_json::json!({}))
            }
        };
        let result = driver_call(tool, tool_args).await?;
        if result.images.is_empty() {
            return Err(format!(
                "Driver returned no image for {target_label}: {}",
                result.joined_text()
            ));
        }
        let dir = previews_dir(work_dir);
        let (paths, images, note) = attach_driver_images(&result, &dir, &target_label)?;
        let mirror_dir = global_previews_dir().unwrap_or_else(|| dir.clone());
        write_mirror_sidecar(
            &mirror_dir,
            paths.first().map(PathBuf::as_path),
            &format!("Screenshot {target_label} (CUA driver)"),
        );
        let label = format!("Screenshot {target_label} (CUA driver)");
        note_screenshot(&label);
        let content = format!(
            "{note}. {}",
            result.joined_text().chars().take(2000).collect::<String>()
        );
        Ok(ToolOutput { content, images })
    }

    async fn ax_with_output(
        &self,
        args: &str,
        work_dir: Option<&Path>,
    ) -> Result<ToolOutput, String> {
        if !driver_available() {
            return Err(DRIVER_MISSING_HINT.to_string());
        }
        let parsed: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| format!("Invalid {COMPUTER_AX_TOOL} arguments: {error}"))?;
        let pid = parsed
            .get("pid")
            .and_then(|value| value.as_i64())
            .ok_or_else(|| {
                format!("`{COMPUTER_AX_TOOL}` requires `pid` from computer_windows.")
            })?;
        let window_id = parsed
            .get("window_id")
            .and_then(|value| value.as_i64())
            .ok_or_else(|| {
                format!("`{COMPUTER_AX_TOOL}` requires `window_id` from computer_windows.")
            })?;
        let include_screenshot = parsed
            .get("include_screenshot")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        if include_screenshot {
            self.approve(
                format!("Inspect window {window_id}"),
                format!(
                    "Snapshot window {window_id} (pid {pid}) via the CUA driver: accessibility tree plus screenshot. You will see the image; the agent receives it too."
                ),
            )
            .await?;
        }
        let mut tool_args = serde_json::json!({
            "pid": pid,
            "window_id": window_id,
            "include_screenshot": include_screenshot,
        });
        for key in ["query", "max_elements", "max_depth"] {
            if let Some(value) = parsed.get(key) {
                tool_args[key] = value.clone();
            }
        }
        let result = driver_call("get_window_state", tool_args).await?;
        crate::mirror::ensure_feed_window(window_id);
        let structured = result.structured.clone().unwrap_or(serde_json::Value::Null);
        let mut sections =
            vec![format!("Accessibility snapshot of window {window_id} (pid {pid}):")];
        if let Some(reason) = structured
            .get("degraded_reason")
            .and_then(|reason| reason.as_str())
        {
            sections.push(format!(
                "Degraded snapshot ({reason}): the tree may be empty — act by pixel off the screenshot."
            ));
        }
        let tree = structured
            .get("tree_markdown")
            .and_then(|tree| tree.as_str())
            .unwrap_or("");
        if tree.is_empty() {
            sections.push("Empty element tree.".to_string());
        } else if tree.chars().count() > MAX_TREE_CHARS {
            let truncated: String = tree.chars().take(MAX_TREE_CHARS).collect();
            sections.push(format!(
                "{truncated}\n…(tree truncated at {MAX_TREE_CHARS} chars; re-query with `query` or `max_elements` to narrow it)"
            ));
        } else {
            sections.push(tree.to_string());
        }
        if let Some(snapshot) = structured
            .get("snapshot_id")
            .and_then(|snapshot| snapshot.as_str())
        {
            sections.push(format!(
                "Snapshot {snapshot}: pass element_token (or element_index + this snapshot_id) from the tree to cua_call element actions. Indices expire on the next snapshot of this window."
            ));
        }
        let mut content = sections.join("\n\n");
        let mut images = Vec::new();
        if !result.images.is_empty() {
            let dir = previews_dir(work_dir);
            let (paths, attached, note) =
                attach_driver_images(&result, &dir, &format!("window {window_id} snapshot"))?;
            let mirror_dir = global_previews_dir().unwrap_or_else(|| dir.clone());
            write_mirror_sidecar(
                &mirror_dir,
                paths.first().map(PathBuf::as_path),
                &format!("Inspect window {window_id} (CUA driver)"),
            );
            note_screenshot(&format!("Inspect window {window_id}"));
            content.push_str(&format!("\n\n{note}."));
            images = attached;
        }
        Ok(ToolOutput { content, images })
    }

    async fn act(&self, args: &str, work_dir: Option<&Path>) -> Result<String, String> {
        if !driver_available() {
            return Err(DRIVER_MISSING_HINT.to_string());
        }
        let intent = parse_computer_act(args)?;
        let raw_target = parse_act_target(args)?;
        let mut title = intent.approval_title();
        if let Some(id) = raw_target {
            title = format!("{title} in window {id}");
        }
        self.approve(
            title.clone(),
            format!(
                "{title} via the CUA driver (background-first: cursor and focus usually stay untouched). Deny if the target looks wrong."
            ),
        )
        .await?;
        // Targeted coordinates are window-local screenshot pixels; untargeted
        // ones are display pixels. The driver reverses Retina/downscale
        // itself, so no scale bookkeeping is needed — the model reads
        // positions off the last screenshot.
        let pid = match raw_target {
            Some(id) => {
                crate::mirror::ensure_feed_window(id);
                Some(resolve_window_pid(id).await?)
            }
            None => {
                crate::mirror::ensure_feed_display();
                None
            }
        };
        let window_id = raw_target;
        let outcome = self.perform_act(&intent, pid, window_id).await?;
        publish_act_overlay(&intent, &title);
        let mirror_dir = global_previews_dir().unwrap_or_else(|| previews_dir(work_dir));
        write_mirror_sidecar(&mirror_dir, None, &format!("{title} — {outcome}"));
        Ok(outcome)
    }

    /// Translate one compat intent into driver tool call(s).
    async fn perform_act(
        &self,
        intent: &ComputerAct,
        pid: Option<i64>,
        window_id: Option<i64>,
    ) -> Result<String, String> {
        match intent {
            ComputerAct::Click { x, y, modifiers } => {
                let modifier = driver_modifiers(modifiers);
                let args = match (pid, window_id) {
                    (Some(pid), Some(window)) => serde_json::json!({
                        "pid": pid, "window_id": window,
                        "x": x, "y": y, "modifier": modifier,
                    }),
                    _ => serde_json::json!({
                        "scope": "desktop", "x": x, "y": y, "modifier": modifier,
                    }),
                };
                let result = driver_call("click", args).await?;
                Ok(format!("Clicked ({x:.0}, {y:.0}). {}", result.joined_text()))
            }
            ComputerAct::DoubleClick { x, y, modifiers } => {
                let modifier = driver_modifiers(modifiers);
                // The dedicated `double_click` tool requires a pid, so
                // untargeted (foreground) double-clicks go through `click`
                // with a count of 2 in desktop scope — no window guessing.
                let args = match (pid, window_id) {
                    (Some(pid), _) => {
                        let mut args = serde_json::json!({
                            "pid": pid, "x": x, "y": y, "modifier": modifier,
                        });
                        if let Some(window) = window_id {
                            args["window_id"] = serde_json::json!(window);
                        }
                        (true, args)
                    }
                    _ => (false, serde_json::json!({
                        "scope": "desktop", "x": x, "y": y,
                        "modifier": modifier, "count": 2,
                    })),
                };
                let (targeted, args) = args;
                let result = driver_call(if targeted { "double_click" } else { "click" }, args).await?;
                Ok(format!(
                    "Double-clicked ({x:.0}, {y:.0}). {}",
                    result.joined_text()
                ))
            }
            ComputerAct::Move { x, y } => {
                // Window scope moves the agent overlay only; desktop scope
                // moves the real pointer.
                let args = match (pid, window_id) {
                    (Some(pid), Some(window)) => serde_json::json!({
                        "x": x, "y": y,
                        "target": {"kind": "window", "pid": pid, "window_id": window},
                    }),
                    _ => serde_json::json!({
                        "x": x, "y": y, "scope": "desktop",
                    }),
                };
                let result = driver_call("move_cursor", args).await?;
                Ok(format!("Moved to ({x:.0}, {y:.0}). {}", result.joined_text()))
            }
            ComputerAct::Scroll { dx, dy } => {
                let (direction, magnitude) = if dx.abs() >= dy.abs() {
                    (if *dx > 0.0 { "right" } else { "left" }, dx.abs())
                } else {
                    (if *dy > 0.0 { "down" } else { "up" }, dy.abs())
                };
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let amount = (magnitude / 40.0).round().clamp(1.0, 50.0) as u64;
                let mut args =
                    serde_json::json!({"direction": direction, "amount": amount, "by": "line"});
                match (pid, window_id) {
                    (Some(pid), _) => {
                        args["pid"] = serde_json::json!(pid);
                        if let Some(window) = window_id {
                            args["window_id"] = serde_json::json!(window);
                        }
                    }
                    _ => {
                        args["scope"] = serde_json::json!("desktop");
                    }
                }
                let result = driver_call("scroll", args).await?;
                Ok(format!(
                    "Scrolled {direction} (amount {amount}). {}",
                    result.joined_text()
                ))
            }
            ComputerAct::Type { text } => {
                let mut args = serde_json::json!({"text": text});
                match (pid, window_id) {
                    (Some(pid), _) => {
                        args["pid"] = serde_json::json!(pid);
                        if let Some(window) = window_id {
                            args["window_id"] = serde_json::json!(window);
                        }
                    }
                    _ => {
                        args["scope"] = serde_json::json!("desktop");
                    }
                }
                let result = driver_call("type_text", args).await?;
                Ok(format!(
                    "Typed {} characters. {}",
                    text.chars().count(),
                    result.joined_text()
                ))
            }
            ComputerAct::Press { key, modifiers } => {
                let modifier = driver_modifiers(modifiers);
                let mut args = serde_json::json!({
                    "key": driver_key(key),
                    "modifiers": modifier,
                });
                match (pid, window_id) {
                    (Some(pid), _) => {
                        args["pid"] = serde_json::json!(pid);
                        if let Some(window) = window_id {
                            args["window_id"] = serde_json::json!(window);
                        }
                    }
                    _ => {
                        args["scope"] = serde_json::json!("desktop");
                    }
                }
                let result = driver_call("press_key", args).await?;
                Ok(format!("Pressed {key}. {}", result.joined_text()))
            }
        }
    }

    async fn cua_call_with_output(
        &self,
        args: &str,
        work_dir: Option<&Path>,
    ) -> Result<ToolOutput, String> {
        if !driver_available() {
            return Err(DRIVER_MISSING_HINT.to_string());
        }
        let parsed: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| format!("Invalid {CUA_CALL_TOOL} arguments: {error}"))?;
        let tool = parsed
            .get("tool")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .ok_or_else(|| format!("`{CUA_CALL_TOOL}` requires a driver `tool` name."))?;
        if tool == "set_config" {
            return Err("Driver configuration is host-owned: change it with `cua-driver config` in a terminal, not through the model.".to_string());
        }
        let tool_args = parsed
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        if !cua_call_is_read_only(tool, &tool_args) {
            let summary: String = serde_json::to_string(&tool_args)
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            self.approve(
                format!("CUA {tool}"),
                format!("Run CUA driver tool `{tool}` with {summary}. Deny if the target looks wrong."),
            )
            .await?;
        }
        let result = driver_call(tool, tool_args).await?;
        let mut content = result.joined_text();
        if content.chars().count() > MAX_TREE_CHARS {
            let truncated: String = content.chars().take(MAX_TREE_CHARS).collect();
            content = format!("{truncated}\n…(output truncated at {MAX_TREE_CHARS} chars)");
        }
        let mut images = Vec::new();
        if !result.images.is_empty() {
            let dir = previews_dir(work_dir);
            let (paths, attached, note) =
                attach_driver_images(&result, &dir, &format!("CUA {tool}"))?;
            let mirror_dir = global_previews_dir().unwrap_or_else(|| dir.clone());
            write_mirror_sidecar(
                &mirror_dir,
                paths.first().map(PathBuf::as_path),
                &format!("CUA {tool} (CUA driver)"),
            );
            note_screenshot(&format!("CUA {tool}"));
            if content.is_empty() {
                content = note;
            } else {
                content.push_str(&format!("\n\n{note}."));
            }
            images = attached;
        }
        if content.is_empty() {
            content = format!("CUA driver tool `{tool}` returned no text.");
        }
        Ok(ToolOutput { content, images })
    }
}

/// Read-only driver tools that skip the approval prompt in [`CUA_CALL_TOOL`].
/// `get_window_state` is read-only only without a screenshot; anything that
/// returns pixels prompts like `computer_screenshot`.
fn cua_call_is_read_only(tool: &str, args: &serde_json::Value) -> bool {
    match tool {
        "list_windows" | "list_apps" | "get_accessibility_tree" | "get_screen_size"
        | "get_cursor_position" | "check_permissions" | "get_config" | "get_session"
        | "list_sessions" | "get_recording_state" | "get_agent_cursor_state"
        | "check_for_update" => true,
        "get_window_state" => args
            .get("include_screenshot")
            .and_then(|value| value.as_bool())
            .is_some_and(|include| !include),
        _ => false,
    }
}

/// Publish the mirror overlay for an act. Positions are known only for
/// desktop-scope (display pixel) acts; window-local ones ride label-only.
fn publish_act_overlay(intent: &ComputerAct, label: &str) {
    use threadlane_protocol::live::{publish_overlay, LiveOverlayKind};
    match intent {
        ComputerAct::Click { x, y, .. } => {
            publish_overlay(LiveOverlayKind::Click, Some((*x, *y)), None, label);
        }
        ComputerAct::DoubleClick { x, y, .. } => {
            publish_overlay(LiveOverlayKind::DoubleClick, Some((*x, *y)), None, label);
        }
        ComputerAct::Move { x, y } => {
            publish_overlay(LiveOverlayKind::Move, Some((*x, *y)), None, label);
        }
        ComputerAct::Scroll { dx, dy } => {
            publish_overlay(LiveOverlayKind::Scroll, None, Some((*dx, *dy)), label);
        }
        ComputerAct::Type { .. } => {
            publish_overlay(LiveOverlayKind::Type, None, None, label);
        }
        ComputerAct::Press { .. } => {
            publish_overlay(LiveOverlayKind::Press, None, None, label);
        }
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
    fn driver_key_mapping_matches_driver_names() {
        assert_eq!(driver_key("Enter"), "return");
        assert_eq!(driver_key("Escape"), "escape");
        assert_eq!(driver_key("Space"), "space");
        assert_eq!(driver_key("Delete"), "delete");
        assert_eq!(driver_key("Up"), "up");
        assert_eq!(driver_key("L"), "l");
    }

    #[test]
    fn driver_modifiers_spell_alt_as_option() {
        assert_eq!(
            driver_modifiers(&["cmd".to_string(), "alt".to_string()]),
            ["cmd".to_string(), "option".to_string()]
        );
    }

    #[test]
    fn read_only_allowlist_covers_discovery_only() {
        assert!(cua_call_is_read_only("list_windows", &serde_json::json!({})));
        assert!(cua_call_is_read_only(
            "get_window_state",
            &serde_json::json!({"include_screenshot": false})
        ));
        assert!(!cua_call_is_read_only(
            "get_window_state",
            &serde_json::json!({})
        ));
        assert!(!cua_call_is_read_only("click", &serde_json::json!({})));
        assert!(!cua_call_is_read_only(
            "get_desktop_state",
            &serde_json::json!({})
        ));
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
                COMPUTER_AX_TOOL,
                COMPUTER_ACT_TOOL,
                CUA_CALL_TOOL,
            ]
        );
    }

    #[test]
    fn window_rows_parse_driver_shape() {
        let structured = serde_json::json!({
            "windows": [
                {"window_id": 7, "pid": 100, "app_name": "Safari", "title": "Tab"},
            ]
        });
        assert_eq!(
            driver_window_rows(Some(&structured)),
            [(7, 100, "Safari".to_string(), "Tab".to_string())]
        );
        assert!(driver_window_rows(None).is_empty());
    }

    #[test]
    fn mirror_sidecar_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("computer-1.png");
        std::fs::write(&path, b"fake-png").unwrap();
        write_mirror_sidecar(dir.path(), Some(&path), "Screenshot live");
        let sidecar: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("latest.json")).unwrap())
                .unwrap();
        assert_eq!(sidecar["action"], "Screenshot live");
        assert!(sidecar["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("computer-1.png")));
        assert!(sidecar["ts_ms"].as_u64().is_some());
    }

    /// Live driver probe: needs `cua-driver` installed and a WindowServer, so
    /// ignored in CI. Run by hand with `-- --ignored` to prove the pooled MCP
    /// session (one handshake across status, windows, and feed polls), window
    /// listing, and a real mirror frame work end to end. Read-only: no
    /// approval channel, no dialogs (`check_permissions` runs prompt:false,
    /// feed frames never enter model context).
    #[tokio::test]
    #[ignore]
    async fn live_driver_status_and_windows() {
        if !driver_available() {
            eprintln!("skipped: no cua-driver binary");
            return;
        }
        let status = computer_status().await;
        assert!(
            status.contains("CUA driver"),
            "status should name the driver: {status}"
        );
        let windows = list_windows("{}").await.expect("window list works");
        assert!(
            windows.contains("id=") || windows.contains("No top-level windows"),
            "unexpected listing: {windows}"
        );
        // The feed shares the pooled session: one handshake served all three
        // calls above, and a display frame should land within a few polls.
        crate::mirror::ensure_feed_display();
        let mut frame = None;
        for _ in 0..8 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            frame = threadlane_protocol::live::latest_frame();
            if frame.is_some() {
                break;
            }
        }
        let frame = frame.expect("mirror feed should publish a display frame");
        assert!(frame.width <= threadlane_protocol::live::LIVE_FRAME_MAX_WIDTH);
        assert_eq!(
            frame.bgra.len(),
            frame.width as usize * frame.height as usize * 4
        );
    }

    #[tokio::test]
    async fn unknown_tool_is_not_claimed() {
        let executor = ComputerToolExecutor::new(None);
        assert!(executor.execute_tool("read_file", "{}").await.is_none());
    }

    #[tokio::test]
    async fn gated_tools_fail_closed_without_driver_or_permission_channel() {
        // Without a permission channel every gated tool denies, whether or
        // not the driver binary exists on this machine.
        let executor = ComputerToolExecutor::new(None);
        for tool in [COMPUTER_SCREENSHOT_TOOL, COMPUTER_ACT_TOOL, CUA_CALL_TOOL] {
            let result = executor
                .execute_tool(tool, "{}")
                .await
                .expect("handled");
            assert!(result.is_err(), "{tool} should fail closed");
        }
    }
}
