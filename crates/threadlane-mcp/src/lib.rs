//! Model Context Protocol (MCP) stdio client: process lifecycle, connection
//! reuse, discovery, and tool calls.
//!
//! This crate is runtime-agnostic: it exposes MCP-native tool metadata
//! ([`McpToolDescription`]) and structured results ([`McpToolResult`]) and
//! performs no host tool-schema conversion. Hosts adapt it to their own tool
//! runtime (in Threadlane, `threadlane-session` owns the `ToolExecutor`
//! adapter that converts descriptions to `AgentToolDefinition` and flattens
//! results to text).
//!
//! Scope note: this is a stdio MCP tools client, not a complete
//! all-transports MCP SDK. `McpTransport::Sse` configurations are recognized
//! but not connected. The default settings locations keep the Threadlane
//! `.threadlane/mcp.json` convention; other hosts may load configs directly.

use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex as TokioMutex};
use tokio::task::JoinHandle;

const MCP_SETTINGS_FILE: &str = "mcp.json";
const MCP_PROJECT_SETTINGS_RELATIVE_PATH: &str = ".threadlane/mcp.json";
const MAX_MCP_SETTINGS_BYTES: usize = 512 * 1024;
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    #[default]
    Global,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
    },
    Sse {
        url: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub transport: McpTransport,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub scope: McpScope,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct McpSettingsFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Default)]
pub struct McpSettings;

impl McpSettings {
    fn load_global(global_dir: Option<&Path>) -> Vec<McpServerConfig> {
        let Some(dir) = global_dir else {
            return Vec::new();
        };
        let path = dir.join(MCP_SETTINGS_FILE);
        Self::load_file(&path, McpScope::Global)
    }

    fn load_project(project_root: Option<&Path>) -> Vec<McpServerConfig> {
        let Some(root) = project_root else {
            return Vec::new();
        };
        let path = root.join(MCP_PROJECT_SETTINGS_RELATIVE_PATH);
        Self::load_file(&path, McpScope::Project)
    }

    fn load_file(path: &Path, scope: McpScope) -> Vec<McpServerConfig> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
            Err(_) => return Vec::new(),
        };
        if bytes.len() > MAX_MCP_SETTINGS_BYTES {
            return Vec::new();
        }
        let parsed: McpSettingsFile = match serde_json::from_slice(&bytes) {
            Ok(data) => data,
            Err(_) => return Vec::new(),
        };
        parsed
            .servers
            .into_iter()
            .map(|mut config| {
                config.scope = scope;
                config
            })
            .collect()
    }

    pub fn save_global(dir: &Path, servers: &[McpServerConfig]) -> Result<(), String> {
        Self::save_file(&dir.join(MCP_SETTINGS_FILE), servers)
    }

    pub fn save_project(root: &Path, servers: &[McpServerConfig]) -> Result<(), String> {
        Self::save_file(&root.join(MCP_PROJECT_SETTINGS_RELATIVE_PATH), servers)
    }

    fn save_file(file_path: &Path, servers: &[McpServerConfig]) -> Result<(), String> {
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings directory: {e}"))?;
        }
        let file_data = McpSettingsFile {
            servers: servers.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file_data)
            .map_err(|e| format!("Failed to serialize MCP settings: {e}"))?;
        // Atomic swap like the other project-scoped stores: a crash mid-write
        // must not leave a torn settings file behind.
        let temporary = file_path.with_extension("json.tmp");
        fs::write(&temporary, &bytes)
            .map_err(|e| format!("Failed to write MCP settings: {e}"))?;
        fs::rename(&temporary, file_path)
            .map_err(|e| format!("Failed to write MCP settings: {e}"))
    }
}

/// MCP-native description of one tool offered by a server.
///
/// This is the raw listing metadata (namespaced `full_name`, human
/// description, JSON `input_schema`); converting it into a host tool schema
/// is the host adapter's job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolDescription {
    pub(crate) tool_name: String,
    pub full_name: String,
    pub description: String,
    pub input_schema: Value,
}

/// One content item in an MCP `tools/call` result.
///
/// `text` items carry model-readable output; every other item type is
/// retained verbatim as `other` so hosts can decide what to do with
/// structured or binary payloads instead of silently dropping them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContentItem {
    Text {
        text: String,
    },
    #[serde(untagged)]
    Other {
        raw: Value,
    },
}

/// Structured result of an MCP `tools/call`.
///
/// Unlike the flattened text hosts typically feed the model, this preserves
/// the raw response and the server's `isError` flag. Use
/// [`McpToolResult::to_text`] for the legacy text projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolResult {
    pub(crate) content: Vec<McpContentItem>,
    #[serde(default)]
    pub(crate) is_error: bool,
    pub(crate) raw: Value,
}

impl McpToolResult {
    /// Projects the result onto plain text: the `text` items joined by
    /// newlines, or the pretty-printed raw response when there is no text.
    pub fn to_text(&self) -> String {
        let mut output = String::new();
        for item in &self.content {
            if let McpContentItem::Text { text } = item {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(text);
            }
        }
        if output.is_empty() {
            output = serde_json::to_string_pretty(&self.raw).unwrap_or_default();
        }
        output
    }
}

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub(crate) tool_name: String,
    pub(crate) full_name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
}

impl McpToolInfo {
    pub(crate) fn description(&self) -> McpToolDescription {
        McpToolDescription {
            tool_name: self.tool_name.clone(),
            full_name: self.full_name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpServerRecord {
    pub(crate) config: McpServerConfig,
    pub(crate) tools: Vec<McpToolInfo>,
}

/// A live stdio session with one MCP server.
///
/// The handshake is performed once when the process starts and the pipes stay
/// open, so a tool call costs one request/response round trip instead of a
/// process spawn plus a full `initialize` exchange.
///
/// Requests multiplex over the single stdio pair: a background reader task
/// dispatches responses by id into per-request channels, so concurrent tool
/// calls to the same server proceed in parallel instead of queueing behind
/// one session-wide lock.
struct McpSession {
    child: TokioMutex<Child>,
    stdin: TokioMutex<ChildStdin>,
    next_id: AtomicU64,
    pending: Arc<TokioMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    _reader: JoinHandle<()>,
}

impl McpSession {
    /// Spawns the server and completes the MCP handshake.
    async fn connect(
        config: &McpServerConfig,
        client_name: &str,
        client_version: &str,
    ) -> Result<Self, String> {
        let McpTransport::Stdio { command, args, env } = &config.transport else {
            return Err("Only stdio MCP servers can be connected".to_string());
        };

        let mut cmd = Command::new(command);
        cmd.args(args);
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Without this a crashed or replaced session leaks its process.
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|error| format!("Failed to spawn process: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to open stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to open stdout".to_string())?;

        let pending: Arc<TokioMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>> =
            Arc::new(TokioMutex::new(HashMap::new()));
        let reader = tokio::spawn(Self::pump_stdout(stdout, Arc::clone(&pending)));

        let session = Self {
            child: TokioMutex::new(child),
            stdin: TokioMutex::new(stdin),
            next_id: AtomicU64::new(1),
            pending,
            _reader: reader,
        };

        session
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": client_name, "version": client_version }
                }),
            )
            .await
            .map_err(|error| format!("Initialize failed: {error}"))?;
        session
            .notify("notifications/initialized", Value::Null)
            .await?;
        Ok(session)
    }

    /// Background stdout pump: dispatches responses by id so concurrent
    /// requests share the session. Server notifications (no id) and stray
    /// lines are ignored. On EOF or read error every waiter fails instead
    /// of hanging.
    async fn pump_stdout(
        stdout: ChildStdout,
        pending: Arc<TokioMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    ) {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Err(_) => break,
                Ok(_) => {}
            }
            let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let Some(id) = message.get("id").and_then(Value::as_u64) else {
                continue;
            };
            let result = if let Some(error) = message.get("error") {
                Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP error")
                    .to_string())
            } else {
                Ok(message.get("result").cloned().unwrap_or(Value::Null))
            };
            if let Some(sender) = pending.lock().await.remove(&id) {
                let _ = sender.send(result);
            }
        }
        for (_, sender) in pending.lock().await.drain() {
            let _ = sender.send(Err("MCP server closed its output stream".into()));
        }
    }

    async fn write_line(&self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| format!("Failed to write to MCP server: {error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("Failed to flush MCP server stdin: {error}"))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.write_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    /// Sends a request and resolves when the matching response arrives.
    ///
    /// Shares the session with concurrent callers: writes serialize on the
    /// stdin lock (held only for the write), while responses dispatch by id
    /// through the reader task, so a slow tool call no longer blocks every
    /// other call to the same server.
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        if let Err(error) = self
            .write_line(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(MCP_REQUEST_TIMEOUT, receiver).await {
            Ok(Ok(result)) => result,
            // The reader died or the session was retired: the sender is gone.
            Ok(Err(_)) => Err("MCP server closed its output stream".into()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(format!("MCP request '{method}' timed out"))
            }
        }
    }

    /// Terminates the server process (best-effort).
    ///
    /// Takes `&self` so a session can be killed through its shared handle
    /// without needing exclusive ownership of the `Arc`.
    fn kill(&self) {
        if let Ok(mut child) = self.child.try_lock() {
            let _ = child.start_kill();
        }
    }
}

pub struct McpManager {
    global_dir: Option<PathBuf>,
    project_root: Option<PathBuf>,
    client_name: String,
    client_version: String,
    servers: TokioMutex<Vec<McpServerRecord>>,
    cached_tool_descriptions: RwLock<Arc<[McpToolDescription]>>,
    /// Live sessions keyed by server id, reused across tool calls.
    ///
    /// Each session carries its own lock so a call to one server never blocks a
    /// call to another; the outer map is held only long enough to look up or
    /// install the handle, never across the request round trip.
    sessions: TokioMutex<HashMap<String, Arc<McpSession>>>,
}

impl McpManager {
    pub fn new(global_dir: Option<PathBuf>, project_root: Option<PathBuf>) -> Self {
        Self {
            global_dir,
            project_root,
            client_name: "threadlane".to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            servers: TokioMutex::new(Vec::new()),
            cached_tool_descriptions: RwLock::new(Arc::from([])),
            sessions: TokioMutex::new(HashMap::new()),
        }
    }

    /// Identifies the client in the MCP `initialize` handshake.
    ///
    /// Defaults to the Threadlane name and this crate's version; hosts
    /// embedding this client under their own identity should set both.
    pub fn with_client_info(mut self, name: &str, version: &str) -> Self {
        self.client_name = name.to_string();
        self.client_version = version.to_string();
        self
    }

    /// Terminates every live server session.
    ///
    /// Waits for any in-flight call on a session before killing it, so a
    /// concurrent tool call delays shutdown but never escapes it. Call when the
    /// manager's project changes or the app shuts down; sessions otherwise live
    /// as long as the manager does.
    pub async fn shutdown(&self) {
        let sessions = std::mem::take(&mut *self.sessions.lock().await);
        for (_, session) in sessions {
            session.kill();
        }
    }

    pub async fn discover_and_connect(&self) -> Vec<McpServerRecord> {
        let global_configs = McpSettings::load_global(self.global_dir.as_deref());
        let project_configs = McpSettings::load_project(self.project_root.as_deref());

        let mut all_configs = Vec::new();
        let mut seen_ids = BTreeSet::new();

        for config in project_configs.into_iter().chain(global_configs) {
            if seen_ids.insert(config.id.clone()) {
                all_configs.push(config);
            }
        }

        let previous = self
            .servers
            .lock()
            .await
            .iter()
            .cloned()
            .map(|record| (record.config.id.clone(), record))
            .collect::<HashMap<_, _>>();
        let live_ids = self
            .sessions
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let records = join_all(all_configs.into_iter().map(|config| async {
            let tools = if !config.enabled {
                Vec::new()
            } else if live_ids.contains(&config.id)
                && previous
                    .get(&config.id)
                    .is_some_and(|record| record.config == config)
            {
                previous[&config.id].tools.clone()
            } else {
                self.connect_server(&config).await
            };
            McpServerRecord { config, tools }
        }))
        .await;

        let retained_ids = records
            .iter()
            .filter(|record| record.config.enabled)
            .map(|record| record.config.id.clone())
            .collect::<BTreeSet<_>>();
        let retired = {
            let mut sessions = self.sessions.lock().await;
            let stale_ids = sessions
                .keys()
                .filter(|id| !retained_ids.contains(*id))
                .cloned()
                .collect::<Vec<_>>();
            stale_ids
                .into_iter()
                .filter_map(|id| sessions.remove(&id))
                .collect::<Vec<_>>()
        };
        join_all(retired.into_iter().map(|session| async move {
            session.kill();
        }))
        .await;

        let tool_defs: Vec<_> = records
            .iter()
            .flat_map(|record| record.tools.iter().map(|tool| tool.description()))
            .collect();

        let mut guard = self.servers.lock().await;
        *guard = records.clone();
        if let Ok(mut cached) = self.cached_tool_descriptions.write() {
            *cached = tool_defs.into();
        }
        records
    }

    /// Opens (or reuses) a session and lists the server's tools.
    async fn connect_server(&self, config: &McpServerConfig) -> Vec<McpToolInfo> {
        if matches!(config.transport, McpTransport::Sse { .. }) {
            return Vec::new();
        }

        // This path is reached only for a new or changed configuration.
        let previous = self.sessions.lock().await.remove(&config.id);
        if let Some(previous) = previous {
            previous.kill();
        }
        let session =
            match McpSession::connect(config, &self.client_name, &self.client_version).await {
                Ok(session) => session,
                Err(_error) => return Vec::new(),
            };

        let listed = session.request("tools/list", json!({})).await;
        let response = match listed {
            Ok(response) => response,
            Err(_error) => {
                session.kill();
                return Vec::new();
            }
        };

        let mut mcp_tools = Vec::new();
        if let Some(tools) = response.get("tools").and_then(Value::as_array) {
            for tool in tools {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP tool");
                let input_schema = tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                let full_name = format!("mcp__{}__{}", config.id, name);
                mcp_tools.push(McpToolInfo {
                    tool_name: name.to_string(),
                    full_name,
                    description: format!("[MCP: {}] {}", config.name, description),
                    input_schema,
                });
            }
        }

        // Keep the session for tool calls instead of killing it here.
        self.sessions
            .lock()
            .await
            .insert(config.id.clone(), Arc::new(session));
        mcp_tools
    }

    /// Cached [`McpToolDescription`] snapshot, rebuilt by
    /// [`Self::discover_and_connect`].
    pub fn tool_descriptions(&self) -> Arc<[McpToolDescription]> {
        self.cached_tool_descriptions
            .read()
            .map(|defs| defs.clone())
            .unwrap_or_default()
    }

    /// Calls a tool by namespaced (`mcp__<server>__<tool>`) or bare name with
    /// an already-parsed JSON argument value.
    ///
    /// Returns `None` when no enabled server offers the tool. The structured
    /// [`McpToolResult`] preserves content items, the `isError` flag, and the
    /// raw response; hosts that need plain text use [`McpToolResult::to_text`].
    pub async fn call_tool(
        &self,
        full_name: &str,
        args: &Value,
    ) -> Option<Result<McpToolResult, String>> {
        let target = {
            let servers = self.servers.lock().await;
            servers.iter().find_map(|server| {
                if !server.config.enabled {
                    return None;
                }
                server
                    .tools
                    .iter()
                    .find(|tool| tool.full_name == full_name || tool.tool_name == full_name)
                    .map(|tool| (server.config.clone(), tool.tool_name.clone()))
            })
        };
        let (config, tool_name) = target?;

        // Resolve the handle under the map lock, then release it before doing
        // any I/O so concurrent calls are not serialized on the registry.
        // A loser of a connect race kills its duplicate and uses the
        // winner's session instead of orphaning a live process.
        let handle = {
            let existing = self.sessions.lock().await.get(&config.id).cloned();
            match existing {
                Some(handle) => handle,
                None => {
                    // A server that died between calls is restarted once rather
                    // than failing the tool call outright.
                    let session =
                        match McpSession::connect(&config, &self.client_name, &self.client_version)
                            .await
                        {
                            Ok(session) => session,
                            Err(error) => {
                                return Some(Err(format!("Failed to start MCP server: {error}")))
                            }
                        };
                    let handle = Arc::new(session);
                    let mut sessions = self.sessions.lock().await;
                    if let Some(winner) = sessions.get(&config.id) {
                        let winner = Arc::clone(winner);
                        drop(sessions);
                        handle.kill();
                        winner
                    } else {
                        sessions.insert(config.id.clone(), Arc::clone(&handle));
                        handle
                    }
                }
            }
        };

        // No session-wide lock across I/O: requests multiplex by id through
        // the reader task, so concurrent calls to the same server proceed in
        // parallel.
        let response = handle
            .request(
                "tools/call",
                json!({ "name": tool_name, "arguments": args }),
            )
            .await;

        let response = match response {
            Ok(response) => response,
            Err(error) => {
                // The pipe is no longer trustworthy after a failed exchange;
                // drop it so the next call starts a clean session. Only
                // retire our own handle: a concurrent call may already have
                // replaced it with a healthy session.
                let retired = {
                    let mut sessions = self.sessions.lock().await;
                    if sessions
                        .get(&config.id)
                        .is_some_and(|current| Arc::ptr_eq(current, &handle))
                    {
                        sessions.remove(&config.id)
                    } else {
                        None
                    }
                };
                if let Some(broken) = retired {
                    broken.kill();
                }
                return Some(Err(error));
            }
        };

        let mut content = Vec::new();
        if let Some(items) = response.get("content").and_then(Value::as_array) {
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    content.push(McpContentItem::Text {
                        text: text.to_string(),
                    });
                } else {
                    content.push(McpContentItem::Other { raw: item.clone() });
                }
            }
        }
        let is_error = response
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Some(Ok(McpToolResult {
            content,
            is_error,
            raw: response,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_description_slice_is_reused() {
        let manager = McpManager::new(None, None);
        *manager.cached_tool_descriptions.write().unwrap() = vec![McpToolDescription {
            tool_name: "echo".to_string(),
            full_name: "mcp__stub__echo".to_string(),
            description: "echo".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }]
        .into();

        let first = manager.tool_descriptions();
        let second = manager.tool_descriptions();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn tool_result_to_text_prefers_text_and_falls_back_to_raw() {
        let text_result = McpToolResult {
            content: vec![
                McpContentItem::Text {
                    text: "a".to_string(),
                },
                McpContentItem::Text {
                    text: "b".to_string(),
                },
            ],
            is_error: false,
            raw: serde_json::json!({}),
        };
        assert_eq!(text_result.to_text(), "a\nb");

        let raw = serde_json::json!({"structured": [1, 2]});
        let structured = McpToolResult {
            content: vec![McpContentItem::Other { raw: raw.clone() }],
            is_error: false,
            raw: raw.clone(),
        };
        assert_eq!(
            structured.to_text(),
            serde_json::to_string_pretty(&raw).unwrap()
        );
    }

    #[test]
    fn test_mcp_server_config_serialization() {
        let config = McpServerConfig {
            id: "fs".to_string(),
            name: "Filesystem".to_string(),
            transport: McpTransport::Stdio {
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-filesystem".to_string(),
                ],
                env: HashMap::new(),
            },
            enabled: true,
            scope: McpScope::Global,
        };

        let json_str = serde_json::to_string_pretty(&config).unwrap();
        assert!(json_str.contains("Filesystem"));
        assert!(json_str.contains("npx"));

        let deserialized: McpServerConfig = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized, config);
    }

    #[test]
    fn test_mcp_server_config_deserialization_without_scope() {
        let json_str = r#"{
            "servers": [
                {
                    "id": "tokensave",
                    "name": "TokenSave Code Graph",
                    "transport": {
                        "type": "stdio",
                        "command": "tokensave",
                        "args": ["mcp"]
                    },
                    "enabled": true
                }
            ]
        }"#;

        let settings: McpSettingsFile = serde_json::from_str(json_str).unwrap();
        assert_eq!(settings.servers.len(), 1);
        assert_eq!(settings.servers[0].id, "tokensave");
    }

    #[test]
    fn save_global_round_trips_without_temporary_residue() {
        let dir = tempfile::tempdir().unwrap();
        let config = McpServerConfig {
            id: "stub".into(),
            name: "Stub".into(),
            transport: McpTransport::Stdio {
                command: "stub".into(),
                args: Vec::new(),
                env: HashMap::new(),
            },
            enabled: true,
            scope: McpScope::Global,
        };
        McpSettings::save_global(dir.path(), &[config]).unwrap();
        // The atomic swap renames the temporary file into place: no residue
        // may remain alongside the committed settings file.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from("mcp.json")],
            "unexpected files: {entries:?}"
        );
        let reloaded = McpSettings::load_global(Some(dir.path()));
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].id, "stub");
    }
}
