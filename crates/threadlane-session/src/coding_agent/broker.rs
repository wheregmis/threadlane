use crate::extension_broker::{BrokerError, BrokerRequest, CapabilityHandler};
use crate::permission::{PermissionDecision, PermissionManager};
use crate::policy::ToolPolicy;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use threadlane_runtime::AgentEvent;
use threadlane_wasi::WasiExtensionManager;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout, Duration};

use super::cancellation::AgentRunTask;
use super::scheduler::{enqueue_harness_follow_up, AgentWork, AgentWorkScheduler};
use super::subagents::{AgentRunner, MAX_SUBAGENT_TASKS, MAX_SUBAGENT_TASK_CHARS};

pub(crate) const CAPABILITY_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const MAX_CAPABILITY_BUFFER_BYTES: usize = 64 * 1024;
/// Generous timeout for public web fetches (DNS + TLS + transfer).
const NETWORK_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Web pages are routinely 100 KiB–1 MiB; the extension truncates after HTML→text.
const MAX_NETWORK_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_PROCESS_TIMEOUT_MS: u64 = 120_000;
pub(crate) const MAX_PROCESS_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_MANAGED_PROCESSES: usize = 16;
const DEFAULT_RECV_TIMEOUT_MS: u64 = 5000;
const MAX_RECV_TIMEOUT_MS: u64 = 30_000;
const MAX_MANAGED_STDOUT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_BROKER_CONTINUATION_ROUNDS: usize = 4;

/// A persistent subprocess managed by the host for WASI extensions.
/// Extensions reference managed processes by name across invocations.
pub(crate) struct ManagedProcess {
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
    stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    stdout_buf: Arc<tokio::sync::Mutex<Vec<u8>>>,
    pid: u32,
    alive: Arc<AtomicBool>,
}

#[derive(Hash, Eq, PartialEq)]
pub(crate) struct ManagedProcessKey {
    extension: String,
    session: Option<String>,
    name: String,
}
pub(crate) type ManagedProcessRegistry =
    Arc<tokio::sync::Mutex<HashMap<ManagedProcessKey, ManagedProcess>>>;

pub(crate) struct HostCapabilityHandler {
    pub(crate) capability: &'static str,
    pub(crate) tool_policy: Option<Arc<tokio::sync::Mutex<ToolPolicy>>>,
    pub(crate) extensions: Arc<WasiExtensionManager>,
    pub(crate) work_dir: PathBuf,
    pub(crate) event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    pub(crate) allowed_hosts: Arc<std::collections::HashSet<String>>,
    pub(crate) permissions: Option<Arc<PermissionManager>>,
    pub(crate) agent_work: AgentWorkScheduler,
    pub(crate) agent_runner: Option<AgentRunner>,
    pub(crate) session_file: Option<PathBuf>,
    pub(crate) persist_tool_policy: bool,
    pub(crate) managed_processes: ManagedProcessRegistry,
}
impl HostCapabilityHandler {
    fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        self.handle_for_extension(request, "")
    }

    fn handle_for_extension(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        match self.capability {
            "tools" => self.handle_tools(request),
            "agent" => self.handle_agent(request),
            "session" => self.handle_session(request, invoking_extension),
            "fs" => self.handle_fs(request),
            "process" | "network" => Err(BrokerError {
                code: "async_required".into(),
                message: format!("Capability `{}` requires async dispatch", self.capability),
            }),
            "ui" => self.handle_ui(request),
            "events" => self.handle_events(request, invoking_extension),
            _ => Err(BrokerError {
                code: "unknown_capability".into(),
                message: format!("Host does not implement capability `{}`", self.capability),
            }),
        }
    }
}

#[async_trait]
impl CapabilityHandler for HostCapabilityHandler {
    fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        HostCapabilityHandler::handle(self, request)
    }

    fn handle_for_extension(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        HostCapabilityHandler::handle_for_extension(self, request, invoking_extension)
    }

    async fn handle_for_extension_async(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        match self.capability {
            "agent" if request.operation == "run" => self.handle_agent_run_async(request).await,
            "process" => self.handle_process_async(request, invoking_extension).await,
            "network" => self.handle_network_async(request).await,
            _ => self.handle_for_extension(request, invoking_extension),
        }
    }
}

impl HostCapabilityHandler {
    fn handle_tools(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let policy = self
            .tool_policy
            .as_ref()
            .ok_or_else(|| internal_error("Tool policy unavailable"))?;
        match request.operation.as_str() {
            "set_policy" => {
                let value = string_argument(&request.arguments, "policy")?;
                let next = match value {
                    "read_only" => ToolPolicy::ReadOnly,
                    "full" => ToolPolicy::FullAccess,
                    _ => return Err(invalid_argument("policy must be `read_only` or `full`")),
                };
                if self.persist_tool_policy {
                    self.extensions
                        .set_host_state("tools.policy", Value::String(value.into()))
                        .map_err(host_error)?;
                }
                let mut current = policy
                    .try_lock()
                    .map_err(|_| internal_error("Tool policy is busy"))?;
                *current = next;
                Ok(Value::Null)
            }
            "get_policy" => {
                let current = policy
                    .try_lock()
                    .map_err(|_| internal_error("Tool policy is busy"))?;
                Ok(serde_json::json!({"message": match *current {
                    ToolPolicy::ReadOnly => "read_only",
                    ToolPolicy::FullAccess => "full",
                }}))
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }

    async fn handle_agent_run_async(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let values = request
            .arguments
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_argument("missing argument `tasks`"))?;
        if values.len() > MAX_SUBAGENT_TASKS {
            return Err(invalid_argument(format!(
                "agent.run accepts at most {MAX_SUBAGENT_TASKS} tasks"
            )));
        }
        let tasks = values
            .iter()
            .map(|value| {
                let agent = value
                    .get("agent")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|agent| !agent.is_empty())
                    .ok_or_else(|| invalid_argument("each task requires a non-empty `agent`"))?;
                let task = value
                    .get("task")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|task| !task.is_empty())
                    .ok_or_else(|| invalid_argument("each task requires a non-empty `task`"))?;
                if agent.chars().count() > 128 || task.chars().count() > MAX_SUBAGENT_TASK_CHARS {
                    return Err(invalid_argument("agent.run task fields exceed size limits"));
                }
                let instructions = value
                    .get("instructions")
                    .or_else(|| value.get("system_prompt"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let tools = value.get("tools").and_then(Value::as_array).map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                });
                let model = value
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                Ok(AgentRunTask {
                    agent: agent.into(),
                    task: task.into(),
                    instructions,
                    tools,
                    model,
                })
            })
            .collect::<Result<Vec<_>, BrokerError>>()?;
        if tasks.is_empty() {
            return Err(invalid_argument("agent.run requires at least one task"));
        }
        let parallel = request
            .arguments
            .get("parallel")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let runner = self
            .agent_runner
            .as_ref()
            .ok_or_else(|| internal_error("Child-agent runner unavailable"))?;
        (runner)(tasks, parallel, None).await.map_err(host_error)
    }

    fn handle_agent(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let content = match request.operation.as_str() {
            "request_turn" => string_argument(&request.arguments, "prompt")?.to_string(),
            "queue_message" => string_argument(&request.arguments, "content")?.to_string(),
            _ => return unknown_operation(self.capability, &request.operation),
        };
        let session_file = self
            .session_file
            .as_deref()
            .ok_or_else(|| internal_error("Agent queue durability is unavailable"))?;
        enqueue_harness_follow_up(session_file, content.clone(), Vec::new()).map_err(host_error)?;
        self.agent_work.schedule(AgentWork::QueueMessage {
            content,
            images: Vec::new(),
        });
        Ok(serde_json::json!({"queued": true}))
    }

    fn handle_session(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        if invoking_extension.is_empty() {
            return Err(invalid_argument(
                "session capability requires host extension identity",
            ));
        }
        match request.operation.as_str() {
            "get_extension_state" => Ok(serde_json::json!({
                "message": self.extensions.extension_state(invoking_extension)
                    .unwrap_or_else(|| serde_json::json!({}))
                    .to_string()
            })),
            "set_extension_state" => {
                let state = request
                    .arguments
                    .get("state")
                    .cloned()
                    .ok_or_else(|| invalid_argument("missing argument `state`"))?;
                self.extensions
                    .set_extension_state(invoking_extension, state)
                    .map_err(host_error)?;
                Ok(Value::Null)
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }

    fn handle_fs(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        match request.operation.as_str() {
            "read_text" => {
                let path = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "path")?,
                )?;
                let text = fs::read_to_string(path).map_err(host_error)?;
                Ok(serde_json::json!({"message": text}))
            }
            "absolute_path" => {
                let path = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "path")?,
                )?;
                Ok(serde_json::json!({"message": path.to_string_lossy()}))
            }
            "write_text" => {
                let path = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "path")?,
                )?;
                let content = string_argument(&request.arguments, "content")?;
                fs::write(path, content).map_err(host_error)?;
                Ok(Value::Null)
            }
            "rename" => {
                let from = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "from")?,
                )?;
                let to =
                    resolve_work_path(&self.work_dir, string_argument(&request.arguments, "to")?)?;
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent).map_err(host_error)?;
                }
                fs::rename(from, to).map_err(host_error)?;
                Ok(Value::Null)
            }
            "list" => {
                let path = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "path")?,
                )?;
                let entries = fs::read_dir(path)
                    .map_err(host_error)?
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                Ok(serde_json::json!({"message": entries.join("\n")}))
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }

    fn managed_process_key(
        &self,
        name: &str,
        invoking_extension: &str,
    ) -> Result<ManagedProcessKey, BrokerError> {
        if invoking_extension.is_empty() {
            return Err(invalid_argument(
                "process capability requires host extension identity",
            ));
        }
        Ok(ManagedProcessKey {
            extension: invoking_extension.into(),
            session: self.extensions.active_session_scope().map_err(host_error)?,
            name: name.into(),
        })
    }
    async fn handle_process_async(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        match request.operation.as_str() {
            "run" => self.handle_process_run(request).await,
            "spawn" => self.handle_process_spawn(request, invoking_extension).await,
            "send" => self.handle_process_send(request, invoking_extension).await,
            "recv" => self.handle_process_recv(request, invoking_extension).await,
            "kill" => self.handle_process_kill(request, invoking_extension).await,
            "status" => {
                self.handle_process_status(request, invoking_extension)
                    .await
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }

    /// Original fire-and-forget process execution.
    async fn handle_process_run(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let program = string_argument(&request.arguments, "program")?;
        let limits = process_run_limits(&request.arguments)?;
        let args = request
            .arguments
            .get("args")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_argument("missing argument `args`"))?;
        let cwd = request
            .arguments
            .get("cwd")
            .and_then(Value::as_str)
            .map(|cwd| resolve_work_path(&self.work_dir, cwd))
            .transpose()?;
        let mut command = tokio::process::Command::new(program);
        command
            .current_dir(cwd.unwrap_or_else(|| self.work_dir.clone()))
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for arg in args {
            command.arg(
                arg.as_str()
                    .ok_or_else(|| invalid_argument("args must be strings"))?,
            );
        }
        let mut child = command.spawn().map_err(host_error)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| internal_error("process stdout pipe unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| internal_error("process stderr pipe unavailable"))?;
        let (stdout, stderr, status) = timeout(limits.timeout, async {
            tokio::try_join!(
                read_limited(
                    stdout,
                    "process_output_too_large",
                    "process stdout",
                    limits.max_output_bytes,
                ),
                read_limited(
                    stderr,
                    "process_output_too_large",
                    "process stderr",
                    limits.max_output_bytes,
                ),
                async { child.wait().await.map_err(host_error) },
            )
        })
        .await
        .map_err(|_| timeout_error("process.run"))??;
        let stdout =
            String::from_utf8(stdout).map_err(|_| invalid_argument("stdout was not UTF-8"))?;
        let stderr =
            String::from_utf8(stderr).map_err(|_| invalid_argument("stderr was not UTF-8"))?;
        Ok(serde_json::json!({"message": serde_json::json!({
            "exit_code": status.code(), "stdout": stdout, "stderr": stderr
        }).to_string()}))
    }

    /// Spawn a named persistent subprocess with piped stdin/stdout.
    async fn handle_process_spawn(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = string_argument(&request.arguments, "name")?;
        let key = self.managed_process_key(name, invoking_extension)?;
        let program = string_argument(&request.arguments, "program")?;
        let args_val = request
            .arguments
            .get("args")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut registry = self.managed_processes.lock().await;

        // Idempotent: if a process with this name is alive, return its info.
        if let Some(existing) = registry.get(&key) {
            if existing.alive.load(Ordering::Relaxed) {
                return Ok(serde_json::json!({"message": serde_json::json!({
                    "name": name, "pid": existing.pid
                }).to_string()}));
            }
            // Dead process — remove and re-spawn below.
            registry.remove(&key);
        }

        if registry.len() >= MAX_MANAGED_PROCESSES {
            return Err(BrokerError {
                code: "limit_exceeded".into(),
                message: format!(
                    "Cannot spawn more than {MAX_MANAGED_PROCESSES} managed processes"
                ),
            });
        }

        let cwd = request
            .arguments
            .get("cwd")
            .and_then(Value::as_str)
            .map(|cwd| resolve_work_path(&self.work_dir, cwd))
            .transpose()?;
        let mut command = tokio::process::Command::new(program);
        command
            .current_dir(cwd.unwrap_or_else(|| self.work_dir.clone()))
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for arg in &args_val {
            command.arg(
                arg.as_str()
                    .ok_or_else(|| invalid_argument("args must be strings"))?,
            );
        }

        let child = Arc::new(tokio::sync::Mutex::new(
            command.spawn().map_err(host_error)?,
        ));
        let pid = child.lock().await.id().unwrap_or(0);
        let mut child_guard = child.lock().await;
        let stdin = child_guard
            .stdin
            .take()
            .ok_or_else(|| internal_error("process stdin pipe unavailable"))?;
        let mut stdout = child_guard
            .stdout
            .take()
            .ok_or_else(|| internal_error("process stdout pipe unavailable"))?;
        drop(child_guard);

        let stdout_buf: Arc<tokio::sync::Mutex<Vec<u8>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let alive = Arc::new(AtomicBool::new(true));

        // Background reader: continuously reads stdout into the shared buffer.
        let buf_clone = stdout_buf.clone();
        let alive_clone = alive.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            loop {
                match stdout.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut buf = buf_clone.lock().await;
                        if buf.len() + n > MAX_MANAGED_STDOUT_BYTES {
                            // Prevent unbounded growth: drop oldest data.
                            let overflow = (buf.len() + n) - MAX_MANAGED_STDOUT_BYTES;
                            buf.drain(..overflow);
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    Err(_) => break,
                }
            }
            alive_clone.store(false, Ordering::Relaxed);
        });

        registry.insert(
            key,
            ManagedProcess {
                child,
                stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
                stdout_buf,
                pid,
                alive,
            },
        );

        Ok(serde_json::json!({"message": serde_json::json!({
            "name": name, "pid": pid
        }).to_string()}))
    }

    /// Write data to a named process's stdin.
    async fn handle_process_send(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = string_argument(&request.arguments, "name")?;
        let data = string_argument(&request.arguments, "data")?;
        let key = self.managed_process_key(name, invoking_extension)?;

        let stdin = {
            let registry = self.managed_processes.lock().await;
            let process = registry.get(&key).ok_or_else(|| BrokerError {
                code: "not_found".into(),
                message: format!("No managed process named `{name}`"),
            })?;
            if !process.alive.load(Ordering::Relaxed) {
                return Err(BrokerError {
                    code: "process_dead".into(),
                    message: format!("Managed process `{name}` is no longer running"),
                });
            }

            process.stdin.clone()
        };
        timeout(CAPABILITY_TIMEOUT, async move {
            let mut stdin = stdin.lock().await;
            stdin.write_all(data.as_bytes()).await.map_err(host_error)?;
            stdin.flush().await.map_err(host_error)
        })
        .await
        .map_err(|_| timeout_error("process.send"))??;

        Ok(serde_json::json!({"message": serde_json::json!({"ok": true}).to_string()}))
    }

    /// Read data from a named process's stdout with optional framing.
    async fn handle_process_recv(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = string_argument(&request.arguments, "name")?;
        let key = self.managed_process_key(name, invoking_extension)?;
        let timeout_ms = bounded_positive_integer_argument(
            &request.arguments,
            "timeout_ms",
            DEFAULT_RECV_TIMEOUT_MS,
            MAX_RECV_TIMEOUT_MS,
        )?;
        let framing = request
            .arguments
            .get("framing")
            .and_then(Value::as_str)
            .unwrap_or("raw");

        let (stdout_buf, alive) = {
            let registry = self.managed_processes.lock().await;
            let process = registry.get(&key).ok_or_else(|| BrokerError {
                code: "not_found".into(),
                message: format!("No managed process named `{name}`"),
            })?;
            (process.stdout_buf.clone(), process.alive.clone())
        };

        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

        loop {
            {
                let mut buf = stdout_buf.lock().await;
                match framing {
                    "content-length" => {
                        if let Some((message, consumed)) = extract_content_length_message(&buf) {
                            buf.drain(..consumed);
                            return Ok(serde_json::json!({"message": serde_json::json!({
                                "data": message, "eof": false
                            }).to_string()}));
                        }
                    }
                    "line" => {
                        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                            let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                            let line = String::from_utf8_lossy(&line_bytes).to_string();
                            return Ok(serde_json::json!({"message": serde_json::json!({
                                "data": line, "eof": false
                            }).to_string()}));
                        }
                    }
                    _ => {
                        // "raw" — return whatever is available
                        if !buf.is_empty() {
                            let data = std::mem::take(&mut *buf);
                            let text = String::from_utf8_lossy(&data).to_string();
                            return Ok(serde_json::json!({"message": serde_json::json!({
                                "data": text, "eof": false
                            }).to_string()}));
                        }
                    }
                }

                // Check if process is dead and buffer is drained.
                if !alive.load(Ordering::Relaxed) && buf.is_empty() {
                    return Ok(serde_json::json!({"message": serde_json::json!({
                        "data": "", "eof": true
                    }).to_string()}));
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Ok(serde_json::json!({"message": serde_json::json!({
                    "data": "", "eof": !alive.load(Ordering::Relaxed)
                }).to_string()}));
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Terminate a named process and remove it from the registry.
    async fn handle_process_kill(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = string_argument(&request.arguments, "name")?;
        let key = self.managed_process_key(name, invoking_extension)?;
        let mut registry = self.managed_processes.lock().await;
        let removed = registry.remove(&key);
        let Some(process) = removed else {
            return Err(BrokerError {
                code: "not_found".into(),
                message: format!("No managed process named `{name}`"),
            });
        };
        drop(registry);
        process
            .child
            .lock()
            .await
            .kill()
            .await
            .map_err(host_error)?;
        Ok(serde_json::json!({"message": serde_json::json!({"ok": true}).to_string()}))
    }

    /// List active managed processes or query a specific one.
    async fn handle_process_status(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let name = request.arguments.get("name").and_then(Value::as_str);
        let registry = self.managed_processes.lock().await;

        if let Some(name) = name {
            let key = self.managed_process_key(name, invoking_extension)?;
            if let Some(process) = registry.get(&key) {
                return Ok(serde_json::json!({"message": serde_json::json!({
                    "processes": [{
                        "name": name,
                        "pid": process.pid,
                        "alive": process.alive.load(Ordering::Relaxed),
                    }]
                }).to_string()}));
            }
            return Ok(serde_json::json!({"message": serde_json::json!({
                "processes": []
            }).to_string()}));
        }

        let processes: Vec<Value> = registry
            .iter()
            .filter(|(key, _)| {
                key.extension == invoking_extension
                    && key.session == self.extensions.active_session_scope().ok().flatten()
            })
            .map(|(key, p)| {
                serde_json::json!({
                    "name": key.name,
                    "pid": p.pid,
                    "alive": p.alive.load(Ordering::Relaxed),
                })
            })
            .collect();

        Ok(serde_json::json!({"message": serde_json::json!({
            "processes": processes
        }).to_string()}))
    }

    async fn handle_network_async(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        use futures::StreamExt as _;

        if request.operation != "http" {
            return unknown_operation(self.capability, &request.operation);
        }
        let url = string_argument(&request.arguments, "url")?;
        let method = string_argument(&request.arguments, "method")?;
        let body = request
            .arguments
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_argument("missing argument `body`"))?;
        let parsed = reqwest::Url::parse(url).map_err(|_| invalid_argument("invalid URL"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(invalid_argument(
                "only http:// and https:// URLs are supported",
            ));
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| invalid_argument("URL is missing a host"))?
            .to_ascii_lowercase();
        let policy_approved = self.allowed_hosts.contains(&host);
        let persisted_approved = self
            .permissions
            .as_ref()
            .is_some_and(|permissions| permissions.network_host_is_approved(&host));
        let approved = policy_approved || persisted_approved;
        let decision = if approved {
            if let Some(permissions) = &self.permissions {
                permissions
                    .trace_preapproved_network_host(url, persisted_approved)
                    .await
                    .map_err(host_error)?;
            }
            PermissionDecision::AllowOnce
        } else if let Some(permissions) = &self.permissions {
            permissions.request_network_host(&host, url).await
        } else {
            PermissionDecision::Deny
        };
        if decision == PermissionDecision::Deny {
            return Err(BrokerError {
                code: "host_denied".into(),
                message: format!("Network access to `{host}` was denied"),
            });
        }
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| invalid_argument("invalid HTTP method"))?;
        let client = reqwest::Client::builder()
            // A redirect may target a host that has not passed the approval flow.
            // Return the 3xx response so the extension can request the destination
            // explicitly and the next broker call can authorize its exact host.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(host_error)?;
        let result = timeout(NETWORK_HTTP_TIMEOUT, async move {
            let response = client
                .request(method, parsed.clone())
                .header(reqwest::header::USER_AGENT, "Threadlane/0.1")
                .body(body.to_owned())
                .send()
                .await
                .map_err(host_error)?;
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(host_error)?;
                if bytes.len().saturating_add(chunk.len()) > MAX_NETWORK_RESPONSE_BYTES {
                    return Err(BrokerError {
                        code: "network_response_too_large".into(),
                        message: format!(
                            "network response exceeded {MAX_NETWORK_RESPONSE_BYTES} bytes"
                        ),
                    });
                }
                bytes.extend_from_slice(&chunk);
            }
            let body =
                String::from_utf8(bytes).map_err(|_| invalid_argument("response was not UTF-8"))?;
            serde_json::to_string(&serde_json::json!({
                "url": parsed.as_str(),
                "status": status,
                "content_type": content_type,
                "location": location,
                "body": body,
            }))
            .map_err(|error| internal_error(error.to_string()))
        })
        .await
        .map_err(|_| timeout_error("network.http"))??;
        Ok(serde_json::json!({"message": result}))
    }

    fn handle_ui(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let message = match request.operation.as_str() {
            "notify" => string_argument(&request.arguments, "message")?,
            "set_status" => string_argument(&request.arguments, "status")?,
            _ => return unknown_operation(self.capability, &request.operation),
        };
        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
            text_delta: Some(message.to_string()),
            reasoning_delta: None,
            tool_call_name: None,
        });
        Ok(Value::Null)
    }

    fn handle_events(
        &self,
        request: &BrokerRequest,
        invoking_extension: &str,
    ) -> Result<Value, BrokerError> {
        let topic = string_argument(&request.arguments, "topic")?;
        match request.operation.as_str() {
            "subscribe" => {
                if invoking_extension.is_empty() {
                    return Err(invalid_argument(
                        "events subscription requires host extension identity",
                    ));
                }
                self.extensions
                    .subscribe_event(invoking_extension, topic.to_string())
                    .map_err(host_error)?;
                Ok(Value::Null)
            }
            "publish" => {
                let payload = request
                    .arguments
                    .get("payload")
                    .cloned()
                    .ok_or_else(|| invalid_argument("missing argument `payload`"))?;
                self.extensions
                    .publish_event(topic.to_string(), payload)
                    .map_err(host_error)?;
                Ok(Value::Null)
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }
}

async fn read_limited(
    mut reader: impl AsyncRead + Unpin,
    code: &'static str,
    source: &'static str,
    max_bytes: usize,
) -> Result<Vec<u8>, BrokerError> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let read = reader.read(&mut chunk).await.map_err(host_error)?;
        if read == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(read) > max_bytes {
            return Err(BrokerError {
                code: code.into(),
                message: format!("{source} exceeds the {max_bytes}-byte buffer limit"),
            });
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessRunLimits {
    pub(crate) timeout: Duration,
    pub(crate) max_output_bytes: usize,
}

pub(crate) fn process_run_limits(arguments: &Value) -> Result<ProcessRunLimits, BrokerError> {
    let timeout_ms = bounded_positive_integer_argument(
        arguments,
        "timeout_ms",
        CAPABILITY_TIMEOUT.as_millis() as u64,
        MAX_PROCESS_TIMEOUT_MS,
    )?;
    let max_output_bytes = bounded_positive_integer_argument(
        arguments,
        "max_output_bytes",
        MAX_CAPABILITY_BUFFER_BYTES as u64,
        MAX_PROCESS_OUTPUT_BYTES as u64,
    )? as usize;
    Ok(ProcessRunLimits {
        timeout: Duration::from_millis(timeout_ms),
        max_output_bytes,
    })
}

fn bounded_positive_integer_argument(
    arguments: &Value,
    name: &str,
    default: u64,
    max: u64,
) -> Result<u64, BrokerError> {
    let Some(value) = arguments.get(name) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid_argument(format!("`{name}` must be a positive integer")))?;
    Ok(value.min(max))
}

fn timeout_error(operation: &str) -> BrokerError {
    BrokerError {
        code: "timeout".into(),
        message: format!("Capability operation `{operation}` timed out"),
    }
}
fn internal_error(message: impl Into<String>) -> BrokerError {
    BrokerError {
        code: "host_error".into(),
        message: message.into(),
    }
}
fn host_error(error: impl std::fmt::Display) -> BrokerError {
    internal_error(error.to_string())
}
fn invalid_argument(message: impl Into<String>) -> BrokerError {
    BrokerError {
        code: "invalid_argument".into(),
        message: message.into(),
    }
}
fn unknown_operation(capability: &str, operation: &str) -> Result<Value, BrokerError> {
    Err(BrokerError {
        code: "unknown_operation".into(),
        message: format!("Capability `{capability}` does not implement operation `{operation}`"),
    })
}
fn string_argument<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, BrokerError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid_argument(format!("missing or empty argument `{name}`")))
}

fn extract_content_length_message(buffer: &[u8]) -> Option<(String, usize)> {
    let header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
    let header = std::str::from_utf8(&buffer[..header_end]).ok()?;
    let content_length = header.lines().find_map(|line| {
        line.split_once(':').and_then(|(name, value)| {
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
    })?;
    let message_end = header_end.checked_add(content_length)?;
    let body = buffer.get(header_end..message_end)?;
    Some((String::from_utf8(body.to_vec()).ok()?, message_end))
}
fn resolve_work_path(work_dir: &Path, relative: &str) -> Result<PathBuf, BrokerError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(invalid_argument("path must remain under work_dir"));
    }
    let root = work_dir.canonicalize().map_err(host_error)?;
    let candidate = root.join(path);
    let checked = if candidate.exists() {
        candidate.canonicalize().map_err(host_error)?
    } else {
        candidate
            .parent()
            .ok_or_else(|| invalid_argument("invalid path"))?
            .canonicalize()
            .map_err(host_error)?
            .join(
                candidate
                    .file_name()
                    .ok_or_else(|| invalid_argument("invalid path"))?,
            )
    };
    if !checked.starts_with(&root) {
        return Err(invalid_argument("path escapes work_dir"));
    }
    Ok(checked)
}
