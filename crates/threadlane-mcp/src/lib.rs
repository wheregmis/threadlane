use async_trait::async_trait;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use threadlane_runtime::{AgentToolDefinition, ToolExecutor};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as TokioMutex;

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
pub struct McpSettings {
    #[allow(dead_code)]
    servers: Vec<McpServerConfig>,
}

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
        fs::write(file_path, bytes).map_err(|e| format!("Failed to write MCP settings: {e}"))
    }
}

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    tool_name: String,
    full_name: String,
    definition: AgentToolDefinition,
}

#[derive(Debug, Clone)]
pub struct McpServerRecord {
    config: McpServerConfig,
    tools: Vec<McpToolInfo>,
}

/// A live stdio session with one MCP server.
///
/// The handshake is performed once when the process starts and the pipes stay
/// open, so a tool call costs one request/response round trip instead of a
/// process spawn plus a full `initialize` exchange.
struct McpSession {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpSession {
    /// Spawns the server and completes the MCP handshake.
    async fn connect(config: &McpServerConfig) -> Result<Self, String> {
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

        let mut session = Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        };

        session
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "threadlane", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await
            .map_err(|error| format!("Initialize failed: {error}"))?;
        session
            .notify("notifications/initialized", Value::Null)
            .await?;
        Ok(session)
    }

    async fn write_line(&mut self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| format!("Failed to write to MCP server: {error}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| format!("Failed to flush MCP server stdin: {error}"))
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    /// Sends a request and reads until the matching response arrives.
    ///
    /// Notifications and unrelated responses are skipped rather than mistaken
    /// for the answer, which a single blind `read_line` would do.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;

        let deadline = tokio::time::Instant::now() + MCP_REQUEST_TIMEOUT;
        loop {
            let mut line = String::new();
            let read = tokio::time::timeout_at(deadline, self.reader.read_line(&mut line))
                .await
                .map_err(|_| format!("MCP request '{method}' timed out"))?
                .map_err(|error| format!("Failed to read from MCP server: {error}"))?;
            if read == 0 {
                return Err("MCP server closed its output stream".to_string());
            }
            let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let text = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP error");
                return Err(text.to_string());
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Terminates the server process.
    ///
    /// Takes `&mut self` rather than `self` so a session can be killed through
    /// its shared handle without needing exclusive ownership of the `Arc`.
    async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

pub struct McpManager {
    global_dir: Option<PathBuf>,
    project_root: Option<PathBuf>,
    servers: TokioMutex<Vec<McpServerRecord>>,
    cached_tool_defs: RwLock<Arc<[AgentToolDefinition]>>,
    /// Live sessions keyed by server id, reused across tool calls.
    ///
    /// Each session carries its own lock so a call to one server never blocks a
    /// call to another; the outer map is held only long enough to look up or
    /// install the handle, never across the request round trip.
    sessions: TokioMutex<HashMap<String, Arc<TokioMutex<McpSession>>>>,
}

impl McpManager {
    pub fn new(global_dir: Option<PathBuf>, project_root: Option<PathBuf>) -> Self {
        Self {
            global_dir,
            project_root,
            servers: TokioMutex::new(Vec::new()),
            cached_tool_defs: RwLock::new(Arc::from([])),
            sessions: TokioMutex::new(HashMap::new()),
        }
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
            session.lock().await.kill().await;
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
            session.lock().await.kill().await;
        }))
        .await;

        let tool_defs: Vec<_> = records
            .iter()
            .flat_map(|record| record.tools.iter().map(|tool| tool.definition.clone()))
            .collect();

        let mut guard = self.servers.lock().await;
        *guard = records.clone();
        if let Ok(mut cached) = self.cached_tool_defs.write() {
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
            previous.lock().await.kill().await;
        }
        let mut session = match McpSession::connect(config).await {
            Ok(session) => session,
            Err(_error) => return Vec::new(),
        };

        let listed = session.request("tools/list", json!({})).await;
        let response = match listed {
            Ok(response) => response,
            Err(_error) => {
                session.kill().await;
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
                    full_name: full_name.clone(),
                    definition: AgentToolDefinition::new(
                        full_name,
                        format!("[MCP: {}] {}", config.name, description),
                        input_schema,
                    ),
                });
            }
        }

        // Keep the session for tool calls instead of killing it here.
        self.sessions
            .lock()
            .await
            .insert(config.id.clone(), Arc::new(TokioMutex::new(session)));
        mcp_tools
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        self.cached_tool_defs
            .read()
            .map(|defs| defs.clone())
            .unwrap_or_default()
    }

    async fn execute_tool(
        &self,
        full_name: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
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

        let parsed_args: Value = match serde_json::from_str(args) {
            Ok(value) => value,
            Err(error) => return Some(Err(format!("Invalid JSON tool arguments: {error}"))),
        };

        // Resolve the handle under the map lock, then release it before doing
        // any I/O so concurrent calls to other servers are not serialized.
        let handle = {
            let existing = self.sessions.lock().await.get(&config.id).cloned();
            match existing {
                Some(handle) => handle,
                None => {
                    // A server that died between calls is restarted once rather
                    // than failing the tool call outright.
                    let session = match McpSession::connect(&config).await {
                        Ok(session) => session,
                        Err(error) => {
                            return Some(Err(format!("Failed to start MCP server: {error}")))
                        }
                    };
                    let handle = Arc::new(TokioMutex::new(session));
                    self.sessions
                        .lock()
                        .await
                        .insert(config.id.clone(), Arc::clone(&handle));
                    handle
                }
            }
        };

        let response = {
            let mut session = handle.lock().await;
            session
                .request(
                    "tools/call",
                    json!({ "name": tool_name, "arguments": parsed_args }),
                )
                .await
        };

        let response = match response {
            Ok(response) => response,
            Err(error) => {
                // The pipe is no longer trustworthy after a failed exchange;
                // drop it so the next call starts a clean session.
                let broken = self.sessions.lock().await.remove(&config.id);
                if let Some(broken) = broken {
                    broken.lock().await.kill().await;
                }
                return Some(Err(error));
            }
        };

        let mut output = String::new();
        if let Some(content) = response.get("content").and_then(Value::as_array) {
            for item in content {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(text);
                }
            }
        }
        if output.is_empty() {
            output = serde_json::to_string_pretty(&response).unwrap_or_default();
        }
        Some(Ok(output))
    }
}

#[async_trait]
impl ToolExecutor for McpManager {
    fn executor_id(&self) -> &str {
        "threadlane.mcp_tools"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        McpManager::tool_definitions(self)
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        McpManager::execute_tool(self, name, args).await
    }
}

/// Compatibility adapter for callers that have not yet registered an
/// `McpManager` directly with `ToolDispatcher`.
pub struct McpToolExecutor {
    manager: Arc<McpManager>,
}

impl McpToolExecutor {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.mcp_tools.adapter"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        self.manager.tool_definitions()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        self.manager.execute_tool(name, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definition_slice_is_reused() {
        let manager = McpManager::new(None, None);
        *manager.cached_tool_defs.write().unwrap() = vec![AgentToolDefinition::new(
            "mcp__stub__echo",
            "echo",
            serde_json::json!({"type": "object"}),
        )]
        .into();

        let first = manager.tool_definitions();
        let second = manager.tool_definitions();

        assert!(Arc::ptr_eq(&first, &second));
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
}
