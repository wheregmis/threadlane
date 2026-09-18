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
//! the last capture plus action line. The live mirror retains a virtual
//! pointer for successful coordinate actions independently of the user's
//! system mouse. Action markers use the same screenshot-to-display projection.
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
/// Act on one UI element by visible text: snapshots fresh, resolves the
/// element, and acts with its token in a single approval. Prefer this over
/// `computer_ax` + pixel `computer_act` for buttons, links, and fields.
pub const COMPUTER_INTERACT_TOOL: &str = "computer_interact";
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
            COMPUTER_INTERACT_TOOL,
            "Click, double-click, type into, press a key on, or scroll one UI element by its visible text: takes pid + window_id from computer_windows, snapshots the window fresh, resolves your query to an element, and acts with its token — all in one approval, never a stale index. Prefer this over computer_ax plus pixel computer_act for buttons, links, text fields, and menu items. For canvases, video, or custom-drawn surfaces with no element text, use computer_screenshot plus computer_act with coordinates instead.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pid": { "type": "integer", "description": "Owner pid from computer_windows." },
                    "window_id": { "type": "integer", "description": "Window id from computer_windows." },
                    "query": { "type": "string", "description": "Visible text of the target (button label, link text, field name). Matched case-insensitively against element labels and values; omit only when passing element_index." },
                    "element_index": { "type": "integer", "description": "Element index from a previous computer_ax snapshot. Re-resolved against a fresh snapshot; prefer query, which never goes stale." },
                    "action": {
                        "type": "string",
                        "enum": ["click", "double_click", "type", "press", "scroll"],
                        "description": "type needs text, press needs key, scroll needs dx/dy."
                    },
                    "text": { "type": "string", "description": "Text for the type action." },
                    "key": {
                        "type": "string",
                        "description": "Key for press: a named key (Enter, Escape, Tab, Space, Backspace, Delete, Up, Down, Left, Right) or a single character for combos."
                    },
                    "modifiers": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["shift", "ctrl", "alt", "cmd"] },
                        "description": "Optional modifiers held for click/press."
                    },
                    "dx": { "type": "number", "description": "Horizontal scroll pixels (positive = right)." },
                    "dy": { "type": "number", "description": "Vertical scroll pixels (positive = down)." }
                },
                "required": ["pid", "window_id", "action"],
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
            "Full CUA driver catalog passthrough: call any driver tool not covered above (element clicks via element_token, drag, hotkey, set_value, invoke_menu, clipboard_read/write, launch_app, bring_to_front, set_window_frame, verify_state, zoom, browser_* page tools, recording, sessions). For Chrome/Edge pages prefer the typed browser route: bind with the pid/window_id from computer_windows (computer_status names the live route), then browser_navigate, browser state reads, and browser_click/browser_type by DOM ref — no screenshots needed; verify with state reads, never transport success alone. Safari has no typed engine: use native AX/pixel tools there. Read-only discovery skips approval; everything else prompts first and denied actions must not be retried verbatim. Get element_token/snapshot_id from computer_ax; get pid/window_id from computer_windows.",
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
    parse_window_target(args, COMPUTER_ACT_TOOL, "target")
}

fn parse_window_target(args: &str, tool: &str, field: &str) -> Result<Option<i64>, String> {
    let parsed: serde_json::Value = serde_json::from_str(args)
        .map_err(|error| format!("Invalid {tool} arguments: {error}"))?;
    if !parsed.is_object() {
        return Err(format!("Invalid {tool} arguments: expected an object."));
    }
    match parsed.get(field) {
        None => Ok(None),
        // Window id 0 is kCGNullWindowID and never names a window; a zero
        // target is always a caller defaulting the field, not a real target.
        Some(value) => value
            .as_i64()
            .filter(|id| *id > 0 && u32::try_from(*id).is_ok())
            .map(Some)
            .ok_or_else(|| {
                format!("`{tool}` {field} must be a window id from computer_windows.")
            })
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
            COMPUTER_INTERACT_TOOL => Some(self.interact(args, work_dir).await),
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
    // Read-only: surfaces the auto-recording tied to the mirror feed.
    let recording_line = driver_call("get_recording_state", serde_json::json!({}))
        .await
        .ok()
        .and_then(|result| result.structured)
        .map(|structured| {
            let enabled = structured
                .get("enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if enabled {
                let dir = structured
                    .get("output_dir")
                    .or_else(|| structured.get("output_directory"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown directory");
                format!("Trajectory recording is on ({dir}).")
            } else {
                "Trajectory recording starts automatically with computer use.".to_string()
            }
        })
        .unwrap_or_else(|| "Trajectory recording state unavailable.".to_string());
    let browser_hint = browser_route_hint().await;
    format!(
        "Computer use is available through the CUA driver ({version}): computer_windows lists top-level windows, computer_ax snapshots one window's accessibility tree plus screenshot, computer_interact acts on one element by text in a single approval, computer_screenshot captures the display or one window, computer_act clicks/types/presses keys background-first, cua_call reaches the rest of the driver catalog. {permission_line} {recording_line} {browser_hint} Every screenshot and input action asks for approval first (allow once, for the session, or always for the project); if a macOS permission is missing, grant it in System Settings → Privacy & Security, then retry."
    )
}

/// Browsers with a driver-typed DOM route (proven: Chrome, Edge, Chromium).
/// Safari has no CDP engine in the driver and goes through native AX/pixel.
fn is_typed_browser_app(app: &str) -> bool {
    let app = app.to_lowercase();
    app.contains("chrome") || app.contains("chromium") || app == "microsoft edge"
}

/// One-line typed-browser route for `computer_status`: the first live
/// Chrome/Edge window, so the model knows DOM-ref tools are available
/// without another discovery round trip.
async fn browser_route_hint() -> String {
    let Ok(result) = driver_call("list_windows", serde_json::json!({})).await else {
        return "Browser route unknown (window list failed).".to_string();
    };
    let found = result
        .structured
        .as_ref()
        .and_then(|structured| structured.get("windows"))
        .and_then(|windows| windows.as_array())
        .and_then(|windows| {
            windows.iter().find_map(|window| {
                let app = window.get("app_name")?.as_str()?;
                if !is_typed_browser_app(app) {
                    return None;
                }
                Some((
                    app.to_string(),
                    window.get("pid")?.as_i64()?,
                    window.get("window_id")?.as_i64()?,
                ))
            })
        });
    match found {
        Some((app, pid, window_id)) => format!(
            "Typed browser route available: {app} (pid {pid}, window {window_id}) — drive its pages with cua_call browser_* tools bound to that pid/window_id (DOM refs beat pixels and screenshots)."
        ),
        None => "No Chrome/Edge window open (their pages get typed DOM tools via cua_call); Safari and other apps go through native AX/pixel tools.".to_string(),
    }
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

/// One actionable element resolved from a fresh snapshot: its opaque token
/// plus human-readable identity for approvals and results.
#[derive(Debug, PartialEq)]
struct InteractTarget {
    token: String,
    role: String,
    label: String,
}

impl InteractTarget {
    fn describe(&self) -> String {
        if self.label.is_empty() {
            format!("({})", self.role)
        } else {
            format!("{:?} ({})", self.label, self.role)
        }
    }
}

struct InteractAttempt {
    target: InteractTarget,
    snapshot_id: String,
}

/// Validated per-action payload for [`COMPUTER_INTERACT_TOOL`].
struct InteractPayload {
    text: Option<String>,
    key: Option<String>,
    modifiers: Vec<String>,
    dx: f64,
    dy: f64,
}

impl InteractPayload {
    /// Dominant scroll axis mapped to a driver direction + wheel amount.
    fn scroll_vector(&self) -> (&'static str, u64) {
        let (direction, magnitude) = if self.dx.abs() >= self.dy.abs() {
            (
                if self.dx > 0.0 { "right" } else { "left" },
                self.dx.abs(),
            )
        } else {
            (if self.dy > 0.0 { "down" } else { "up" }, self.dy.abs())
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let amount = (magnitude / 40.0).round().clamp(1.0, 50.0) as u64;
        (direction, amount)
    }

    fn scroll_delta(&self) -> Option<(f64, f64)> {
        Some((self.dx, self.dy))
    }
}

fn interact_payload(action: &str, parsed: &serde_json::Value) -> Result<InteractPayload, String> {
    let text = parsed
        .get("text")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let key = parsed
        .get("key")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    let delta = |name: &str| {
        parsed
            .get(name)
            .and_then(|value| value.as_f64())
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
    };
    let (dx, dy) = (delta("dx"), delta("dy"));
    match action {
        "type" => {
            let text = text.filter(|text| !text.is_empty()).ok_or_else(|| {
                format!("`{COMPUTER_INTERACT_TOOL}` type requires non-empty `text`.")
            })?;
            if text.chars().count() > MAX_TYPE_CHARS {
                return Err(format!(
                    "`{COMPUTER_INTERACT_TOOL}` type accepts at most {MAX_TYPE_CHARS} characters."
                ));
            }
            Ok(InteractPayload {
                text: Some(text),
                key: None,
                modifiers: Vec::new(),
                dx: 0.0,
                dy: 0.0,
            })
        }
        "press" => {
            let key = key.ok_or_else(|| {
                format!("`{COMPUTER_INTERACT_TOOL}` press requires `key`.")
            })?;
            let is_character = key.chars().count() == 1;
            if !is_character && !VALID_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "`{COMPUTER_INTERACT_TOOL}` press key must be a single character or one of {}.",
                    VALID_KEYS.join(", ")
                ));
            }
            Ok(InteractPayload {
                text: None,
                key: Some(driver_key(&key)),
                modifiers: driver_modifiers(&parse_modifiers(parsed)?),
                dx: 0.0,
                dy: 0.0,
            })
        }
        "scroll" => {
            if dx == 0.0 && dy == 0.0 {
                return Err(format!(
                    "`{COMPUTER_INTERACT_TOOL}` scroll needs a non-zero dx or dy."
                ));
            }
            Ok(InteractPayload {
                text: None,
                key: None,
                modifiers: Vec::new(),
                dx,
                dy,
            })
        }
        "click" | "double_click" => Ok(InteractPayload {
            text: None,
            key: None,
            modifiers: driver_modifiers(&parse_modifiers(parsed)?),
            dx: 0.0,
            dy: 0.0,
        }),
        _ => Err(format!("Unsupported interact action: {action}")),
    }
}

/// Ranked element match: exact label beats prefix beats substring, so
/// "Send" does not land on "Send Feedback".
fn match_element_by_query(
    elements: &[serde_json::Value],
    query: &str,
) -> Result<InteractTarget, String> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Err("empty query matches everything; name the element text.".to_string());
    }
    let mut best: Option<(u8, InteractTarget)> = None;
    let mut tied = 0usize;
    for element in elements {
        let token = element
            .get("element_token")
            .and_then(|token| token.as_str())
            .unwrap_or("");
        if token.is_empty() {
            continue;
        }
        let label = element
            .get("label")
            .and_then(|label| label.as_str())
            .unwrap_or("")
            .trim();
        let value = element
            .get("value")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim();
        let haystack = format!("{label}\n{value}").to_lowercase();
        if haystack.trim().is_empty() {
            continue;
        }
        let rank = if label.to_lowercase() == needle {
            0
        } else if haystack.starts_with(&needle) {
            1
        } else if haystack.contains(&needle) {
            2
        } else {
            continue;
        };
        let candidate = InteractTarget {
            token: token.to_string(),
            role: element
                .get("role")
                .and_then(|role| role.as_str())
                .unwrap_or("element")
                .to_string(),
            label: if label.is_empty() {
                value.to_string()
            } else {
                label.to_string()
            },
        };
        match &best {
            Some((best_rank, _)) if *best_rank < rank => {}
            Some((best_rank, _)) if *best_rank == rank => tied += 1,
            _ => {
                best = Some((rank, candidate));
                tied = 0;
            }
        }
    }
    match best {
        None => Err("matched no actionable element (canvas or custom-drawn surface? use computer_screenshot plus computer_act with coordinates).".to_string()),
        Some((_, _target)) if tied > 0 => Err(format!(
            "matched {} elements; re-query with more specific text or pass element_index from computer_ax.",
            tied + 1
        )),
        Some((_, target)) => Ok(target),
    }
}

fn match_element_by_index(elements: &[serde_json::Value], index: i64) -> Option<InteractTarget> {
    elements.iter().find_map(|element| {
        if element.get("element_index")?.as_i64()? != index {
            return None;
        }
        Some(InteractTarget {
            token: element
                .get("element_token")?
                .as_str()
                .filter(|token| !token.is_empty())?
                .to_string(),
            role: element
                .get("role")
                .and_then(|role| role.as_str())
                .unwrap_or("element")
                .to_string(),
            label: element
                .get("label")
                .and_then(|label| label.as_str())
                .unwrap_or("")
                .to_string(),
        })
    })
}

/// The driver fails closed with an explicit stale error once a newer snapshot
/// supersedes the token's. Match loosely: the exact wording is versioned.
fn is_stale_snapshot_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("stale")
        && (lower.contains("snapshot")
            || lower.contains("supersede")
            || lower.contains("token"))
}

fn action_verb(action: &str) -> &'static str {
    match action {
        "double_click" => "Double-clicked",
        "type" => "Typed into",
        "press" => "Pressed key on",
        "scroll" => "Scrolled",
        _ => "Clicked",
    }
}

/// Mirror overlay for an element action: positions are unknown (token path),
/// so markers ride label-only — except scroll, which carries its delta.
fn publish_interact_overlay(action: &str, title: &str, delta: Option<(f64, f64)>) {
    use threadlane_protocol::live::{publish_overlay, LiveOverlayKind};
    let kind = match action {
        "double_click" => LiveOverlayKind::DoubleClick,
        "type" => LiveOverlayKind::Type,
        "press" => LiveOverlayKind::Press,
        "scroll" => LiveOverlayKind::Scroll,
        _ => LiveOverlayKind::Click,
    };
    publish_overlay(kind, None, delta.filter(|_| action == "scroll"), title);
}

impl ComputerToolExecutor {
    async fn screenshot_with_output(
        &self,
        args: &str,
        work_dir: Option<&Path>,
    ) -> Result<ToolOutput, String> {
        let window_id = parse_window_target(args, "computer_screenshot", "window_id")?;
        if !driver_available() {
            return Err(DRIVER_MISSING_HINT.to_string());
        }
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
    /// Act on one element by visible text: fresh tree-only snapshot, resolve,
    /// approve once against the resolved label, act with the token, and retry
    /// once on a stale snapshot. One approval and zero screenshots for the
    /// common button/link/field case.
    async fn interact(&self, args: &str, work_dir: Option<&Path>) -> Result<String, String> {
        if !driver_available() {
            return Err(DRIVER_MISSING_HINT.to_string());
        }
        let parsed: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| format!("Invalid {COMPUTER_INTERACT_TOOL} arguments: {error}"))?;
        let pid = parsed
            .get("pid")
            .and_then(|value| value.as_i64())
            .ok_or_else(|| {
                format!("`{COMPUTER_INTERACT_TOOL}` requires `pid` from computer_windows.")
            })?;
        let window_id = parsed
            .get("window_id")
            .and_then(|value| value.as_i64())
            .ok_or_else(|| {
                format!("`{COMPUTER_INTERACT_TOOL}` requires `window_id` from computer_windows.")
            })?;
        let action = parsed
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if !["click", "double_click", "type", "press", "scroll"].contains(&action) {
            return Err(format!(
                "`{COMPUTER_INTERACT_TOOL}` action must be one of click, double_click, type, press, scroll."
            ));
        }
        let query = parsed
            .get("query")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|query| !query.is_empty());
        let element_index = parsed
            .get("element_index")
            .and_then(|value| value.as_i64());
        if query.is_none() && element_index.is_none() {
            return Err(format!(
                "`{COMPUTER_INTERACT_TOOL}` needs `query` (visible text) or `element_index`."
            ));
        }
        // Per-action payload, validated before any driver call.
        let payload = interact_payload(action, &parsed)?;
        // Tree-only snapshots are read-only: resolve first so the approval
        // names the exact element, then act.
        let attempt = self
            .resolve_interact_target(pid, window_id, query, element_index)
            .await?;
        let title = format!(
            "{} {} in window {window_id}",
            action_verb(action),
            attempt.target.describe()
        );
        self.approve(
            title.clone(),
            format!(
                "{title} via the CUA driver (background-first: cursor and focus usually stay untouched). Deny if the target looks wrong."
            ),
        )
        .await?;
        match self
            .drive_interact(pid, window_id, &attempt, action, &payload)
            .await
        {
            Ok(outcome) => {
                crate::mirror::ensure_feed_window(window_id);
                publish_interact_overlay(action, &title, payload.scroll_delta());
                let mirror_dir =
                    global_previews_dir().unwrap_or_else(|| previews_dir(work_dir));
                write_mirror_sidecar(&mirror_dir, None, &format!("{title} — {outcome}"));
                Ok(format!(
                    "{outcome}\nAct again with `{COMPUTER_INTERACT_TOOL}` (fresh tokens every call); snapshot indices from `computer_ax` expire."
                ))
            }
            Err(error) if is_stale_snapshot_error(&error) => {
                // One retry against a fresh snapshot: the window re-rendered
                // between resolve and act.
                let attempt = self
                    .resolve_interact_target(pid, window_id, query, element_index)
                    .await
                    .map_err(|retry_error| {
                        format!("{error}\nRetry also failed to resolve: {retry_error}")
                    })?;
                let outcome = self
                    .drive_interact(pid, window_id, &attempt, action, &payload)
                    .await?;
                crate::mirror::ensure_feed_window(window_id);
                Ok(format!("{outcome} (after re-resolving a stale snapshot)"))
            }
            Err(error) => Err(error),
        }
    }

    /// Fresh tree-only snapshot plus element resolution.
    async fn resolve_interact_target(
        &self,
        pid: i64,
        window_id: i64,
        query: Option<&str>,
        element_index: Option<i64>,
    ) -> Result<InteractAttempt, String> {
        let result = driver_call(
            "get_window_state",
            serde_json::json!({
                "pid": pid,
                "window_id": window_id,
                "include_screenshot": false,
            }),
        )
        .await?;
        let elements = result
            .structured
            .as_ref()
            .and_then(|structured| structured.get("elements"))
            .and_then(|elements| elements.as_array())
            .cloned()
            .unwrap_or_default();
        let target = match (query, element_index) {
            (_, Some(index)) => match_element_by_index(&elements, index).ok_or_else(|| {
                format!(
                    "No element {index} in the fresh snapshot of window {window_id}; re-query with `{COMPUTER_INTERACT_TOOL}` or take a new `computer_ax` snapshot."
                )
            })?,
            (Some(query), None) => match_element_by_query(&elements, query).map_err(|hint| {
                format!("No match for {query:?} in window {window_id}: {hint}")
            })?,
            (None, None) => {
                return Err(format!(
                    "`{COMPUTER_INTERACT_TOOL}` needs `query` or `element_index`."
                ));
            }
        };
        let snapshot = result
            .structured
            .as_ref()
            .and_then(|structured| structured.get("snapshot_id"))
            .and_then(|snapshot| snapshot.as_str())
            .unwrap_or("unknown");
        Ok(InteractAttempt {
            target,
            snapshot_id: snapshot.to_string(),
        })
    }

    /// Execute one resolved element action by token.
    async fn drive_interact(
        &self,
        pid: i64,
        window_id: i64,
        attempt: &InteractAttempt,
        action: &str,
        payload: &InteractPayload,
    ) -> Result<String, String> {
        let token = &attempt.target.token;
        let base = serde_json::json!({
            "pid": pid,
            "window_id": window_id,
            "element_token": token,
            "snapshot_id": attempt.snapshot_id,
        });
        let (tool, args) = match action {
            "click" => ("click", base),
            "double_click" => ("double_click", base),
            "type" => {
                let mut args = base;
                args["text"] = serde_json::json!(payload.text.as_deref().unwrap_or(""));
                ("type_text", args)
            }
            "press" => {
                let mut args = base;
                args["key"] = serde_json::json!(payload.key.as_deref().unwrap_or(""));
                args["modifiers"] = serde_json::json!(payload.modifiers);
                ("press_key", args)
            }
            "scroll" => {
                let mut args = base;
                let (direction, amount) = payload.scroll_vector();
                args["direction"] = serde_json::json!(direction);
                args["amount"] = serde_json::json!(amount);
                args["by"] = serde_json::json!("line");
                ("scroll", args)
            }
            _ => return Err(format!("Unsupported interact action: {action}")),
        };
        let result = driver_call(tool, args).await?;
        Ok(format!(
            "{} {} in window {window_id}. {}",
            action_verb(action),
            attempt.target.describe(),
            result.joined_text()
        ))
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
        publish_act_overlay(&intent, window_id, &title);
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

/// Publish the action marker and retain a virtual pointer independently of
/// the user's system mouse. Window-local points need matching frame geometry.
fn publish_act_overlay(intent: &ComputerAct, window_id: Option<i64>, label: &str) {
    use threadlane_protocol::live::{publish_overlay, LiveOverlayKind};
    match intent {
        ComputerAct::Click { x, y, .. }
        | ComputerAct::DoubleClick { x, y, .. }
        | ComputerAct::Move { x, y } => {
            let point = crate::mirror::record_pointer(window_id, (*x, *y));
            let kind = match intent {
                ComputerAct::Click { .. } => LiveOverlayKind::Click,
                ComputerAct::DoubleClick { .. } => LiveOverlayKind::DoubleClick,
                _ => LiveOverlayKind::Move,
            };
            publish_overlay(kind, point, None, label);
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
    fn screenshot_target_parsing_rejects_invalid_ids() {
        assert_eq!(parse_window_target(r#"{"window_id":16958}"#, "computer_screenshot", "window_id").unwrap(), Some(16958));
        assert!(parse_window_target(r#"{"window_id":0}"#, "computer_screenshot", "window_id").is_err());
        assert!(parse_window_target(r#"{"window_id":-1}"#, "computer_screenshot", "window_id").is_err());
        assert!(parse_window_target(r#"{"window_id":4294967296}"#, "computer_screenshot", "window_id").is_err());
        assert!(parse_window_target("not-json", "computer_screenshot", "window_id").is_err());
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
                COMPUTER_INTERACT_TOOL,
                COMPUTER_ACT_TOOL,
                CUA_CALL_TOOL,
            ]
        );
    }

    fn element(index: i64, role: &str, label: &str) -> serde_json::Value {
        serde_json::json!({
            "element_index": index,
            "role": role,
            "label": label,
            "value": "",
            "element_token": format!("s00000001:{index}"),
        })
    }

    #[test]
    fn interact_match_prefers_exact_over_substring() {
        let elements = vec![
            element(0, "AXWindow", "YouTube"),
            element(1, "AXButton", "Send Feedback"),
            element(2, "AXButton", "Send"),
        ];
        // Exact label wins even though it sorts last.
        let target = match_element_by_query(&elements, "send").unwrap();
        assert_eq!(target.token, "s00000001:2");
        assert_eq!(target.role, "AXButton");
        // Case-insensitive substring on the label.
        let target = match_element_by_query(&elements, "FEEDBACK").unwrap();
        assert_eq!(target.token, "s00000001:1");
        // Tokenless rows are never actionable.
        let untokened = vec![serde_json::json!({
            "element_index": 9, "role": "AXButton", "label": "Send",
        })];
        assert!(match_element_by_query(&untokened, "send").is_err());
    }

    #[test]
    fn interact_match_rejects_ambiguity_and_emptiness() {
        let elements = vec![
            element(1, "AXButton", "Copy link"),
            element(2, "AXButton", "Copy address"),
        ];
        let error = match_element_by_query(&elements, "copy").unwrap_err();
        assert!(error.contains("2 elements"), "unexpected: {error}");
        // No canvas text: steer to pixels, not a blind guess.
        let error = match_element_by_query(&elements, "play video").unwrap_err();
        assert!(error.contains("computer_act"), "unexpected: {error}");
        assert!(match_element_by_query(&elements, "   ").is_err());
    }

    #[test]
    fn interact_match_by_index_needs_a_live_token() {
        let elements = vec![element(4, "AXTextField", "Search")];
        assert_eq!(
            match_element_by_index(&elements, 4).unwrap().token,
            "s00000001:4"
        );
        assert!(match_element_by_index(&elements, 5).is_none());
    }

    #[test]
    fn stale_snapshot_errors_match_loosely() {
        assert!(is_stale_snapshot_error(
            "element_token is stale: snapshot s00000001 was superseded"
        ));
        assert!(is_stale_snapshot_error("STALE snapshot id"));
        assert!(!is_stale_snapshot_error("window_id_not_found"));
        assert!(!is_stale_snapshot_error("Clicked (1, 2). ok"));
    }

    #[test]
    fn interact_payload_validates_per_action() {
        let args = serde_json::json!({"text": "hello"});
        assert_eq!(
            interact_payload("type", &args).unwrap().text.as_deref(),
            Some("hello")
        );
        assert!(interact_payload("type", &serde_json::json!({"text": ""})).is_err());
        let press = interact_payload(
            "press",
            &serde_json::json!({"key": "L", "modifiers": ["cmd"]}),
        )
        .unwrap();
        assert_eq!(press.key.as_deref(), Some("l"));
        assert_eq!(press.modifiers, ["cmd"]);
        assert!(interact_payload("press", &serde_json::json!({"key": "F13"})).is_err());
        let scroll = interact_payload("scroll", &serde_json::json!({"dy": 120})).unwrap();
        assert_eq!(scroll.scroll_vector(), ("down", 3));
        assert!(interact_payload("scroll", &serde_json::json!({})).is_err());
        assert!(interact_payload("move", &serde_json::json!({})).is_err());
    }

    #[test]
    fn typed_browser_route_covers_chromium_family_only() {
        assert!(is_typed_browser_app("Google Chrome"));
        assert!(is_typed_browser_app("Chromium"));
        assert!(is_typed_browser_app("Microsoft Edge"));
        assert!(!is_typed_browser_app("Safari"));
        assert!(!is_typed_browser_app("Finder"));
        assert!(!is_typed_browser_app(""));
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
