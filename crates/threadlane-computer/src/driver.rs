//! CUA driver backend: computer use through the `cua-driver` binary.
//!
//! Every pixel and input event flows through `cua-driver mcp --direct`, one
//! long-lived MCP-over-stdio subprocess that owns its runtime in-process: no
//! daemon lifecycle to manage, no `serve` socket, no extra grants beyond what
//! the host app already holds. The process-wide pool handshakes once
//! (`initialize`, `notifications/initialized`) and reuses the connection for
//! every [`driver_call`]; calls are serialized on the session lock (the
//! `McpManager` precedent: one lock per session, never across processes).
//! A transport error or child exit retires the session and the next call
//! spawns a replacement — the failed call itself is never retried, since
//! input actions are not idempotent. The child is killed on drop, so a hung
//! driver cannot outlive the process.
//!
//! Binary discovery checks `THREADLANE_CUA_DRIVER` first (tests and custom
//! installs), then `PATH`, then the well-known install locations
//! (`/Applications/CuaDriver.app/...`, `~/.local/bin/cua-driver`). When no
//! binary is found every tool fails closed with [`DRIVER_MISSING_HINT`];
//! nothing falls back to raw OS APIs.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;

/// Hint attached to every failure when no driver binary is installed.
pub const DRIVER_MISSING_HINT: &str = "CUA driver not found: install it with `/bin/bash -c \"$(curl -fsSL https://cua.ai/driver/install.sh)\"`, then retry. A custom binary path can be set with THREADLANE_CUA_DRIVER.";

/// Overall timeout for one driver call: AX walks and screenshots on a busy
/// desktop can take tens of seconds.
const DRIVER_CALL_TIMEOUT: Duration = Duration::from_secs(120);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// One image block from a driver `tools/call` result.
pub struct DriverImage {
    pub mime: String,
    pub base64: String,
}

/// Parsed `tools/call` result: human-readable text, image blocks, and the
/// raw `structuredContent` for callers that need typed fields.
pub struct DriverResult {
    pub texts: Vec<String>,
    pub images: Vec<DriverImage>,
    pub structured: Option<serde_json::Value>,
}

impl DriverResult {
    /// All text blocks joined, for tools whose result is prose or a summary.
    pub fn joined_text(&self) -> String {
        self.texts.join("\n")
    }
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(override_path) = std::env::var("THREADLANE_CUA_DRIVER") {
        let trimmed = override_path.trim();
        if !trimmed.is_empty() {
            paths.push(PathBuf::from(trimmed));
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            paths.push(dir.join("cua-driver"));
        }
    }
    // Well-known install locations when PATH is bare (GUI launches).
    paths.push(PathBuf::from(
        "/Applications/CuaDriver.app/Contents/MacOS/cua-driver",
    ));
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            paths.push(PathBuf::from(home).join(".local/bin/cua-driver"));
        }
    }
    paths
}

fn is_executable(path: &PathBuf) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
    }
}

/// Resolved driver binary, if one is installed. Cached process-wide: installs
/// mid-process are rare and the caller can set `THREADLANE_CUA_DRIVER` before
/// first use in tests.
pub fn driver_binary() -> Option<PathBuf> {
    static CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHED
        .get_or_init(|| candidate_paths().into_iter().find(is_executable))
        .clone()
}

pub fn driver_available() -> bool {
    driver_binary().is_some()
}

/// `cua-driver --version` output for status lines. `None` when the binary is
/// missing or refuses to answer.
pub async fn driver_version() -> Option<String> {
    let binary = driver_binary()?;
    let output = tokio::time::timeout(PROBE_TIMEOUT, Command::new(binary).arg("--version").output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Call one driver MCP tool on the pooled session, spawning (and
/// handshaking) it on first use. Transport failures retire the session so
/// the next call starts fresh; the failed call is never retried.
pub async fn driver_call(tool: &str, args: serde_json::Value) -> Result<DriverResult, String> {
    driver_binary().ok_or_else(|| DRIVER_MISSING_HINT.to_string())?;
    tokio::time::timeout(DRIVER_CALL_TIMEOUT, pooled_call(tool, args))
        .await
        .map_err(|_| format!("CUA driver tool `{tool}` timed out after 120s."))?
}

/// One pooled `mcp --direct` child plus its framing state. The lock in
/// [`session_pool`] serializes calls, so ids never interleave and responses
/// are matched trivially.
struct PooledSession {
    /// Held for `kill_on_drop`: dropping the pool entry kills the driver.
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

struct SessionPool {
    inner: AsyncMutex<Option<PooledSession>>,
}

fn session_pool() -> &'static SessionPool {
    static POOL: OnceLock<SessionPool> = OnceLock::new();
    POOL.get_or_init(|| SessionPool {
        inner: AsyncMutex::new(None),
    })
}

async fn pooled_call(tool: &str, args: serde_json::Value) -> Result<DriverResult, String> {
    let mut guard = session_pool().inner.lock().await;
    if guard.is_none() {
        match spawn_session().await {
            Ok(session) => *guard = Some(session),
            Err(error) => return Err(error),
        }
    }
    let result = match guard.as_mut() {
        Some(session) => call_on_session(session, tool, args).await,
        None => return Err("CUA driver session unavailable.".to_string()),
    };
    if result.is_err() {
        // Retire the session: the transport is suspect, but the call already
        // ran (or failed) exactly once — the next call spawns a replacement.
        *guard = None;
    }
    result
}

/// Spawn the pooled child and run the MCP handshake.
async fn spawn_session() -> Result<PooledSession, String> {
    let binary = driver_binary().ok_or_else(|| DRIVER_MISSING_HINT.to_string())?;
    let mut child = Command::new(binary)
        .args(["mcp", "--direct"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("Could not start CUA driver: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("Driver stdin unavailable.")?;
    let stdout = child.stdout.take().ok_or("Driver stdout unavailable.")?;
    let lines = BufReader::new(stdout).lines();
    write_line(
        &mut stdin,
        &serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "threadlane", "version": "0.1"},
        }}),
    )
    .await?;
    write_line(
        &mut stdin,
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await?;
    // Drain the initialize response so later reads only see tool results.
    let mut session = PooledSession {
        child,
        stdin,
        lines,
        next_id: 2,
    };
    read_response(&mut session, 1, "initialize").await?;
    Ok(session)
}

async fn write_line(stdin: &mut ChildStdin, message: &serde_json::Value) -> Result<(), String> {
    stdin
        .write_all(format!("{message}\n").as_bytes())
        .await
        .map_err(|error| format!("Could not write to CUA driver: {error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("Could not write to CUA driver: {error}"))?;
    Ok(())
}

/// Read lines until the response with `wanted_id` arrives, skipping logging
/// chatter and unrelated messages.
async fn read_response(
    session: &mut PooledSession,
    wanted_id: u64,
    tool: &str,
) -> Result<serde_json::Value, String> {
    loop {
        let line = session
            .lines
            .next_line()
            .await
            .map_err(|error| format!("Could not read from CUA driver: {error}"))?
            .ok_or_else(|| format!("CUA driver closed stdout before answering `{tool}`."))?;
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let message: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(message) => message,
            Err(_) => continue,
        };
        if message.get("id") == Some(&serde_json::json!(wanted_id)) {
            if let Some(error) = message.get("error") {
                let detail = error
                    .get("message")
                    .and_then(|message| message.as_str())
                    .unwrap_or("unknown driver error");
                return Err(format!("CUA driver tool `{tool}` failed: {detail}"));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| format!("CUA driver tool `{tool}` returned no result."));
        }
        // Other ids and notifications are ignored beyond the handshake.
    }
}

async fn call_on_session(
    session: &mut PooledSession,
    tool: &str,
    args: serde_json::Value,
) -> Result<DriverResult, String> {
    let id = session.next_id;
    session.next_id = session.next_id.saturating_add(1).max(2);
    write_line(
        &mut session.stdin,
        &serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": args}}),
    )
    .await?;
    let result = read_response(session, id, tool).await?;
    parse_tool_result(tool, result)
}

fn parse_tool_result(tool: &str, result: serde_json::Value) -> Result<DriverResult, String> {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    if let Some(blocks) = result.get("content").and_then(|content| content.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|kind| kind.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|text| text.as_str()) {
                        texts.push(text.to_string());
                    }
                }
                Some("image") => {
                    if let (Some(data), Some(mime)) = (
                        block.get("data").and_then(|data| data.as_str()),
                        block.get("mimeType").and_then(|mime| mime.as_str()),
                    ) {
                        images.push(DriverImage {
                            mime: mime.to_string(),
                            base64: data.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    if texts.is_empty() && images.is_empty() && result.is_null() {
        return Err(format!("CUA driver tool `{tool}` returned an empty result."));
    }
    Ok(DriverResult {
        texts,
        images,
        structured: result.get("structuredContent").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_hint_names_installer_and_override() {
        assert!(DRIVER_MISSING_HINT.contains("cua.ai/driver/install.sh"));
        assert!(DRIVER_MISSING_HINT.contains("THREADLANE_CUA_DRIVER"));
    }

    #[test]
    fn parses_text_and_image_blocks() {
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "window_id=1 pid=2 elements=0"},
                {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
            ],
            "structuredContent": {"pid": 2},
        });
        let parsed = parse_tool_result("get_window_state", result).unwrap();
        assert_eq!(parsed.texts, ["window_id=1 pid=2 elements=0"]);
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].mime, "image/png");
        assert_eq!(parsed.structured.unwrap()["pid"], 2);
    }

    #[test]
    fn empty_result_is_an_error() {
        assert!(parse_tool_result("click", serde_json::Value::Null).is_err());
    }

    #[test]
    fn candidate_paths_include_well_known_locations() {
        let paths = candidate_paths();
        assert!(paths
            .iter()
            .any(|path| path.to_string_lossy().ends_with("cua-driver")));
        assert!(paths.contains(&PathBuf::from(
            "/Applications/CuaDriver.app/Contents/MacOS/cua-driver"
        )));
    }
}
