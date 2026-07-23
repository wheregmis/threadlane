use crate::agents::{discover_agents, AgentConfig, AgentScope};
use crate::commands::{execute_slash_command, parse_slash_command, CommandAction};
use crate::context::ProjectContext;
use crate::extension_broker::{
    BrokerError, BrokerRequest, CapabilityDispatcher, CapabilityHandler, BROKER_API_VERSION,
};
use crate::skills::{LoadSkillToolExecutor, SkillManager, SkillRegistry};
use crate::system_prompt::{build_system_prompt, SystemPromptBuildOptions, SystemPromptConfig};
use crate::wasi_extension::{WasiExtensionManager, WasiLegacyEffect};
use async_trait::async_trait;
use mypi_agent::{
    AfterToolCallHook, AfterToolCallResult, Agent, AgentEvent, AgentMessage, AgentState,
    AgentToolCall, AgentToolDefinition, AgentToolResult, BeforeToolCallHook, BeforeToolCallResult,
    ImageAttachment, ReasoningEffort, SessionTree, ToolExecutor,
};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::broadcast;
use tokio::time::{timeout, Duration};

const CAPABILITY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CAPABILITY_BUFFER_BYTES: usize = 64 * 1024;
const MAX_SUBAGENT_TASKS: usize = 8;
const MAX_SUBAGENT_TASK_CHARS: usize = 32_000;
const SUBAGENT_CONCURRENCY_LIMIT: usize = 4;
const SUBAGENT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
static NEXT_SUBAGENT_UI_ID: AtomicU64 = AtomicU64::new(1);

type AgentRunner = Arc<
    dyn Fn(Vec<AgentRunTask>, bool) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;

type SubagentObserverState = Arc<std::sync::Mutex<Option<Arc<std::sync::Mutex<Vec<AgentWork>>>>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    FullAccess,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentWork {
    RequestTurn(String),
    QueueMessage(String),
}

#[derive(Debug, Clone)]
struct AgentRunTask {
    agent: String,
    task: String,
}

#[derive(Clone, Default)]
struct AgentWorkScheduler {
    pending: Arc<std::sync::Mutex<Vec<AgentWork>>>,
    #[cfg(test)]
    test_observer: Arc<std::sync::Mutex<Option<Arc<std::sync::Mutex<Vec<AgentWork>>>>>>,
}

impl AgentWorkScheduler {
    fn schedule(&self, work: AgentWork) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.push(work);
        }
    }

    fn drain(&self) -> Vec<AgentWork> {
        self.pending
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn set_test_observer(&self, observer: Arc<std::sync::Mutex<Vec<AgentWork>>>) {
        if let Ok(mut current) = self.test_observer.lock() {
            *current = Some(observer);
        }
    }

    async fn run(&self, agent: &mut Agent) -> bool {
        let pending = self.drain();
        if pending.is_empty() {
            return false;
        }
        #[cfg(test)]
        if let Ok(observer) = self.test_observer.lock().map(|observer| observer.clone()) {
            if let Some(observer) = observer {
                if let Ok(mut observed) = observer.lock() {
                    observed.extend(pending);
                }
                return true;
            }
        }
        for work in pending {
            match work {
                AgentWork::RequestTurn(prompt) => agent.prompt(&prompt).await,
                AgentWork::QueueMessage(content) => {
                    agent.follow_up(AgentMessage::User { content });
                    agent.run_follow_up().await;
                }
            }
        }
        true
    }
}

struct HostCapabilityHandler {
    capability: &'static str,
    tool_policy: Option<Arc<tokio::sync::Mutex<ToolPolicy>>>,
    extensions: Arc<WasiExtensionManager>,
    work_dir: PathBuf,
    event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    allowed_hosts: Arc<HashSet<String>>,
    agent_work: AgentWorkScheduler,
    agent_runner: Option<AgentRunner>,
    persist_tool_policy: bool,
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
            "process" => self.handle_process_async(request).await,
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
            return Err(invalid_argument(&format!(
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
                Ok(AgentRunTask {
                    agent: agent.into(),
                    task: task.into(),
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
        (runner)(tasks, parallel).await.map_err(host_error)
    }

    fn handle_agent(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        let work = match request.operation.as_str() {
            "request_turn" => {
                AgentWork::RequestTurn(string_argument(&request.arguments, "prompt")?.to_string())
            }
            "queue_message" => {
                AgentWork::QueueMessage(string_argument(&request.arguments, "content")?.to_string())
            }
            _ => return unknown_operation(self.capability, &request.operation),
        };
        self.agent_work.schedule(work);
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
            "write_text" => {
                let path = resolve_work_path(
                    &self.work_dir,
                    string_argument(&request.arguments, "path")?,
                )?;
                let content = string_argument(&request.arguments, "content")?;
                fs::write(path, content).map_err(host_error)?;
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
                Ok(
                    serde_json::json!({"message": serde_json::to_string(&entries).unwrap_or_default()}),
                )
            }
            _ => unknown_operation(self.capability, &request.operation),
        }
    }

    async fn handle_process_async(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        if request.operation != "run" {
            return unknown_operation(self.capability, &request.operation);
        }
        let program = string_argument(&request.arguments, "program")?;
        let args = request
            .arguments
            .get("args")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_argument("missing argument `args`"))?;
        let mut command = tokio::process::Command::new(program);
        command
            .current_dir(&self.work_dir)
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
        let (stdout, stderr, status) = timeout(CAPABILITY_TIMEOUT, async {
            tokio::try_join!(
                read_limited(stdout, "process_output_too_large", "process stdout"),
                read_limited(stderr, "process_output_too_large", "process stderr"),
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

    async fn handle_network_async(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
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
        let (host, port, path) = parse_http_url(url)?;
        if !self.allowed_hosts.contains(host) {
            return Err(BrokerError {
                code: "host_denied".into(),
                message: format!("Network host `{host}` is not allowed"),
            });
        }
        let host = host.to_string();
        let request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let result = timeout(CAPABILITY_TIMEOUT, async move {
            let mut stream = tokio::net::TcpStream::connect((host.as_str(), port))
                .await
                .map_err(host_error)?;
            tokio::io::AsyncWriteExt::write_all(&mut stream, request.as_bytes())
                .await
                .map_err(host_error)?;
            let response = read_limited(
                &mut stream,
                "network_response_too_large",
                "network response",
            )
            .await?;
            String::from_utf8(response).map_err(|_| invalid_argument("response was not UTF-8"))
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
) -> Result<Vec<u8>, BrokerError> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let read = reader.read(&mut chunk).await.map_err(host_error)?;
        if read == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(read) > MAX_CAPABILITY_BUFFER_BYTES {
            return Err(BrokerError {
                code: code.into(),
                message: format!(
                    "{source} exceeds the {MAX_CAPABILITY_BUFFER_BYTES}-byte buffer limit"
                ),
            });
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
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
fn parse_http_url(url: &str) -> Result<(&str, u16, String), BrokerError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| invalid_argument("only http:// URLs are supported"))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = authority
        .rsplit_once(':')
        .map_or((authority, 80), |(host, port)| {
            (host, port.parse().unwrap_or(0))
        });
    if host.is_empty() || port == 0 {
        return Err(invalid_argument("invalid URL"));
    }
    Ok((host, port, format!("/{path}")))
}

pub struct CodingAgentOptions {
    pub api_key: String,
    pub account_id: Option<String>,
    pub model: String,
    pub work_dir: PathBuf,
    pub session_file: Option<PathBuf>,
    pub system_prompt: SystemPromptConfig,
}

pub struct ExtensionBeforeToolHook {
    pub tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub extensions: Arc<WasiExtensionManager>,
    pub broker_dispatcher: Arc<CapabilityDispatcher>,
}

#[async_trait]
impl BeforeToolCallHook for ExtensionBeforeToolHook {
    async fn before_tool_call(
        &self,
        tool_call: &AgentToolCall,
        _state: &AgentState,
    ) -> BeforeToolCallResult {
        let policy = *self.tool_policy.lock().await;
        if policy == ToolPolicy::ReadOnly {
            if matches!(
                tool_call.name.as_str(),
                "write_file" | "edit_file" | "write" | "edit" | "run_command"
            ) {
                return BeforeToolCallResult {
                    block: true,
                    reason: Some(format!(
                        "Tool `{}` is blocked because read-only tool policy is ACTIVE.",
                        tool_call.name
                    )),
                };
            }
        }

        let arguments = serde_json::json!({
            "tool_name": tool_call.name,
            "tool_arguments": tool_call.arguments,
        });
        let hook_responses = self
            .extensions
            .execute_hook_with_broker_requests("before_tool_call", &arguments.to_string());
        for resp in hook_responses {
            let res = match resp {
                Ok(res) => res,
                Err(error) => {
                    return BeforeToolCallResult {
                        block: true,
                        reason: Some(format!("Extension hook error: {error}")),
                    };
                }
            };
            if let Err(error) = dispatch_hook_requests(
                &self.broker_dispatcher,
                &self.extensions,
                res.host_broker_requests,
            )
            .await
            {
                return BeforeToolCallResult {
                    block: true,
                    reason: Some(format!("Extension broker error: {}", error.message)),
                };
            }
            let api_version = res.api_version;
            let response = res.response;
            if api_version == BROKER_API_VERSION {
                if let Some(middleware) = response.middleware {
                    if middleware.block == Some(true) {
                        return BeforeToolCallResult {
                            block: true,
                            reason: middleware.reason,
                        };
                    }
                }
            } else if api_version == 1 {
                if let Some(msg) = response.message {
                    if msg.contains("blocked") {
                        return BeforeToolCallResult {
                            block: true,
                            reason: Some(msg),
                        };
                    }
                }
            }
        }

        BeforeToolCallResult::default()
    }
}

pub struct ExtensionAfterToolHook {
    pub extensions: Arc<WasiExtensionManager>,
    pub broker_dispatcher: Arc<CapabilityDispatcher>,
}

#[async_trait]
impl AfterToolCallHook for ExtensionAfterToolHook {
    async fn after_tool_call(
        &self,
        tool_call: &AgentToolCall,
        result: &AgentToolResult,
        _state: &AgentState,
    ) -> AfterToolCallResult {
        let arguments = serde_json::json!({
            "tool_name": tool_call.name,
            "tool_arguments": tool_call.arguments,
            "result": result.content,
            "is_error": result.is_error,
        });
        // Tool requests are queued by ToolExecutor; dispatch them first so the
        // tool's effects precede the deterministic, name-sorted after hooks.
        dispatch_hook_requests_isolated(
            &self.broker_dispatcher,
            &self.extensions,
            self.extensions.take_pending_broker_requests(),
            "WASI tool broker error",
        )
        .await;
        for response in self
            .extensions
            .execute_hook_with_broker_requests("after_tool_call", &arguments.to_string())
        {
            match response {
                Ok(response) => {
                    dispatch_hook_requests_isolated(
                        &self.broker_dispatcher,
                        &self.extensions,
                        response.host_broker_requests,
                        "WASI after-tool hook broker error",
                    )
                    .await;
                }
                Err(error) => eprintln!("WASI after-tool hook error: {error}"),
            }
        }
        AfterToolCallResult::default()
    }
}

struct BrokerAwareWasiToolExecutor {
    extensions: Arc<WasiExtensionManager>,
    broker_dispatcher: Arc<CapabilityDispatcher>,
}

#[async_trait]
impl ToolExecutor for BrokerAwareWasiToolExecutor {
    fn executor_id(&self) -> &str {
        "mypi.wasi_broker_tools"
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        <WasiExtensionManager as ToolExecutor>::tool_definitions(&self.extensions)
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        let invocation = match self
            .extensions
            .execute_tool_with_broker_requests(name, args)?
        {
            Ok(invocation) => invocation,
            Err(error) => return Some(Err(error)),
        };
        if let Some(error) = invocation.response.error {
            return Some(Err(error));
        }
        let immediate_message = invocation.response.message.unwrap_or_default();
        let requests = invocation.host_broker_requests;
        if requests.is_empty() {
            return Some(Ok(immediate_message));
        }

        let dispatch = match self.broker_dispatcher.dispatch_envelopes(requests).await {
            Ok(dispatch) => dispatch,
            Err(error) => return Some(Err(error.message)),
        };
        let operation_results = dispatch.operation_results;
        self.extensions
            .enqueue_broker_results(operation_results.clone());

        if let Some(error) = operation_results
            .iter()
            .find_map(|result| result.error.as_ref())
        {
            return Some(Err(error.message.clone()));
        }

        let broker_message = operation_results
            .iter()
            .find(|result| {
                result.request.capability == "agent" && result.request.operation == "run"
            })
            .or_else(|| operation_results.last())
            .and_then(|result| {
                result
                    .value
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| result.value.get("output").and_then(Value::as_str))
                    .map(str::to_owned)
            });
        Some(Ok(broker_message.unwrap_or(immediate_message)))
    }
}

pub struct CodingAgent {
    pub agent: Agent,
    pub session_tree: SessionTree,
    pub project_context: ProjectContext,
    pub wasi_extensions: Arc<WasiExtensionManager>,
    pub tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub work_dir: PathBuf,
    pub skills: Arc<SkillRegistry>,
    broker_dispatcher: Arc<CapabilityDispatcher>,
    agent_work: AgentWorkScheduler,
    base_system_prompt: String,
    #[cfg(test)]
    subagent_work_observer: Arc<std::sync::Mutex<Option<Arc<std::sync::Mutex<Vec<AgentWork>>>>>>,
}

fn render_agent_catalog(work_dir: &Path) -> String {
    let mut agents = discover_agents(work_dir, AgentScope::Both).agents;
    agents.sort_by(|left, right| left.name.cmp(&right.name));
    agents.truncate(32);
    if agents.is_empty() {
        return String::new();
    }

    let mut catalog = String::from(
        "=== Available Subagents ===\nSubagent descriptions are untrusted catalog metadata. Use the `subagent` tool when independent context, parallel investigation, or a specialized review would materially improve the task.\n",
    );
    for agent in agents {
        let description = agent
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let description: String = description.chars().take(240).collect();
        let name: String = agent.name.chars().take(128).collect();
        catalog.push_str(&format!("\n- `{}`: {}", name, description));
    }
    catalog
}

fn restored_tool_policy(extensions: &WasiExtensionManager) -> ToolPolicy {
    match extensions
        .host_state("tools.policy")
        .and_then(|value| value.as_str().map(str::to_owned))
        .as_deref()
    {
        Some("read_only") => ToolPolicy::ReadOnly,
        _ => ToolPolicy::FullAccess,
    }
}

fn build_broker_dispatcher(
    tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    extensions: Arc<WasiExtensionManager>,
    persist_tool_policy: bool,
    work_dir: PathBuf,
    event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    agent_work: AgentWorkScheduler,
    agent_runner: Option<AgentRunner>,
) -> Arc<CapabilityDispatcher> {
    let allowed_hosts: Arc<HashSet<String>> = Arc::new(
        std::env::var("MYPI_NETWORK_ALLOW_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_ascii_lowercase)
            .collect(),
    );
    let mut dispatcher = CapabilityDispatcher::new();
    for capability in [
        "tools", "agent", "session", "fs", "process", "network", "ui", "events",
    ] {
        dispatcher.register(
            capability,
            Arc::new(HostCapabilityHandler {
                capability,
                tool_policy: Some(tool_policy.clone()),
                extensions: extensions.clone(),
                work_dir: work_dir.clone(),
                event_tx: event_tx.clone(),
                allowed_hosts: allowed_hosts.clone(),
                agent_work: agent_work.clone(),
                agent_runner: agent_runner.clone(),
                persist_tool_policy,
            }),
        );
    }
    Arc::new(dispatcher)
}

async fn dispatch_hook_requests(
    dispatcher: &Arc<CapabilityDispatcher>,
    extensions: &WasiExtensionManager,
    requests: Vec<crate::extension_broker::HostBrokerRequest>,
) -> Result<(), BrokerError> {
    for request in requests {
        let dispatch = dispatcher.dispatch_envelopes(vec![request]).await?;
        extensions.enqueue_broker_results(dispatch.operation_results);
    }
    Ok(())
}

async fn dispatch_hook_requests_isolated(
    dispatcher: &Arc<CapabilityDispatcher>,
    extensions: &WasiExtensionManager,
    requests: Vec<crate::extension_broker::HostBrokerRequest>,
    label: &str,
) {
    for request in requests {
        if let Err(error) = dispatch_hook_requests(dispatcher, extensions, vec![request]).await {
            eprintln!("{label}: {}", error.message);
        }
    }
}

impl CodingAgent {
    pub fn new(options: CodingAgentOptions) -> Self {
        let mut agent = Agent::new(&options.api_key, options.account_id, &options.model);
        let project_context = ProjectContext::discover(&options.work_dir);
        let mut skill_manager = SkillManager::new();
        skill_manager.discover_skills(Some(&options.work_dir));
        let skills = skill_manager.snapshot();
        let skill_catalog = skills.render_model_catalog();

        // A missing session file represents an unsaved draft. GUI startup uses
        // this mode so merely opening the app neither creates nor selects a
        // conversation; the first send binds the draft to a new session.
        let session_tree = if let Some(session_path) = options.session_file.clone() {
            if session_path.exists() {
                SessionTree::load_from_file(&session_path)
                    .unwrap_or_else(|_| SessionTree::new("session"))
            } else {
                if let Some(parent) = session_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let mut session = SessionTree::new("session");
                session.file_path = Some(session_path);
                session
            }
        } else {
            SessionTree::new("draft")
        };

        agent.set_prompt_cache_key(Some(session_tree.session_id.clone()));

        let mut wasi_extensions = WasiExtensionManager::for_project_session(
            &options.work_dir,
            session_tree.session_id.clone(),
        );
        let loaded_ext_count = wasi_extensions.discover_and_load(&options.work_dir);
        let has_subagent_tool = wasi_extensions
            .get_tools()
            .iter()
            .any(|tool| tool["function"]["name"] == "subagent");
        let agent_catalog = if has_subagent_tool {
            render_agent_catalog(&options.work_dir)
        } else {
            String::new()
        };
        let initial_tool_policy = restored_tool_policy(&wasi_extensions);
        let tool_policy = Arc::new(tokio::sync::Mutex::new(initial_tool_policy));
        let wasi_extensions = Arc::new(wasi_extensions);
        let agent_work = AgentWorkScheduler::default();
        #[cfg(test)]
        let subagent_work_observer = Arc::new(std::sync::Mutex::new(None));
        #[cfg(test)]
        let runner_observer: Option<SubagentObserverState> = Some(subagent_work_observer.clone());
        #[cfg(not(test))]
        let runner_observer: Option<SubagentObserverState> = None;
        let runner_api_key = agent.loop_engine.api_key.clone();
        let runner_account_id = agent.loop_engine.account_id.clone();
        let runner_state = agent.loop_engine.state.clone();
        let runner_work_dir = options.work_dir.clone();
        let runner_extensions = wasi_extensions.clone();
        let runner_event_tx = agent.loop_engine.event_tx.clone();
        let runner_semaphore = Arc::new(tokio::sync::Semaphore::new(SUBAGENT_CONCURRENCY_LIMIT));
        let agent_runner: AgentRunner = Arc::new(move |tasks, parallel| {
            let observer = runner_observer.clone();
            let api_key = runner_api_key.clone();
            let account_id = runner_account_id.clone();
            let state = runner_state.clone();
            let work_dir = runner_work_dir.clone();
            let extensions = runner_extensions.clone();
            let event_tx = runner_event_tx.clone();
            let semaphore = runner_semaphore.clone();
            Box::pin(async move {
                let model = state.lock().await.model.clone();
                let observer = observer
                    .and_then(|observer| observer.lock().ok().and_then(|value| value.clone()));
                let (output, thinking) = run_subagents_with_context(
                    tasks, parallel, api_key, account_id, model, work_dir, extensions, event_tx,
                    observer, semaphore,
                )
                .await?;
                Ok(serde_json::json!({
                    "message": output,
                    "output": output,
                    "thinking": thinking
                }))
            })
        });
        let broker_dispatcher = build_broker_dispatcher(
            tool_policy.clone(),
            wasi_extensions.clone(),
            true,
            options.work_dir.clone(),
            agent.loop_engine.event_tx.clone(),
            agent_work.clone(),
            Some(agent_runner.clone()),
        );
        agent
            .loop_engine
            .register_tool_executor(Arc::new(LoadSkillToolExecutor::new(skills.clone())))
            .expect("reserved load_skill tool must register");
        if let Err(error) =
            agent
                .loop_engine
                .register_tool_executor(Arc::new(BrokerAwareWasiToolExecutor {
                    extensions: wasi_extensions.clone(),
                    broker_dispatcher: broker_dispatcher.clone(),
                }))
        {
            eprintln!("WASI tool registration failed: {error}");
        }
        agent.loop_engine.work_dir = Some(options.work_dir.clone());

        let mut system_prompt_config = options.system_prompt.clone();
        if initial_tool_policy == ToolPolicy::ReadOnly {
            system_prompt_config.guidelines.push(
                "The current workspace tool policy is read-only; do not request file mutations or host commands."
                    .to_string(),
            );
        }
        let prompt_tools = agent.loop_engine.configured_tool_definitions();
        let base_system_prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &system_prompt_config,
            work_dir: &options.work_dir,
            tools: &prompt_tools,
            project_context: &project_context,
            skill_catalog: Some(&skill_catalog),
            agent_catalog: Some(&agent_catalog),
            loaded_extension_count: loaded_ext_count,
        });

        agent.loop_engine.before_tool_call_hook = Some(Arc::new(ExtensionBeforeToolHook {
            tool_policy: tool_policy.clone(),
            extensions: wasi_extensions.clone(),
            broker_dispatcher: broker_dispatcher.clone(),
        }));
        agent.loop_engine.after_tool_call_hook = Some(Arc::new(ExtensionAfterToolHook {
            extensions: wasi_extensions.clone(),
            broker_dispatcher: broker_dispatcher.clone(),
        }));

        {
            let mut state = agent
                .loop_engine
                .state
                .try_lock()
                .expect("Failed to lock initial state");
            state.system_prompt = base_system_prompt.clone();
            state.messages.push(AgentMessage::System {
                content: base_system_prompt.clone(),
            });
        }

        Self {
            agent,
            session_tree,
            project_context,
            wasi_extensions,
            tool_policy,
            work_dir: options.work_dir,
            skills,
            broker_dispatcher,
            agent_work,
            base_system_prompt,
            #[cfg(test)]
            subagent_work_observer,
        }
    }

    async fn run_scheduled_agent_work(&mut self) {
        while self.agent_work.run(&mut self.agent).await {
            self.dispatch_assistant_message_hooks().await;
        }
    }

    pub fn base_system_prompt(&self) -> &str {
        &self.base_system_prompt
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.agent.subscribe()
    }

    async fn dispatch_assistant_hook(&self, message: &AgentMessage) {
        let AgentMessage::Assistant {
            content,
            tool_calls,
        } = message
        else {
            return;
        };
        let arguments = serde_json::json!({
            "content": content,
            "tool_calls": tool_calls,
        });
        for response in self
            .wasi_extensions
            .execute_hook_with_effects("assistant_message", &arguments.to_string())
        {
            if let Ok(response) = response {
                let _ = dispatch_hook_requests(
                    &self.broker_dispatcher,
                    &self.wasi_extensions,
                    response.host_broker_requests,
                )
                .await;
            }
        }
        let _ = dispatch_hook_requests(
            &self.broker_dispatcher,
            &self.wasi_extensions,
            self.wasi_extensions.take_pending_broker_requests(),
        )
        .await;
    }

    async fn dispatch_assistant_message_hooks(&mut self) {
        let state = self.agent.get_state().await;

        // The loop engine keeps the complete provider conversation in memory,
        // including assistant tool-call messages and the tool results that
        // follow them. Persist the portion that is not in the session yet so
        // reloading a session produces the same provider history (and keeps
        // the tool-call/result ordering intact).
        let state_messages: Vec<AgentMessage> = state
            .messages
            .into_iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .collect();
        let persisted_messages = self.session_tree.get_active_branch_messages();

        let common_prefix = state_messages
            .iter()
            .zip(persisted_messages.iter())
            .take_while(|(state_message, persisted_message)| {
                serde_json::to_value(state_message).ok()
                    == serde_json::to_value(persisted_message).ok()
            })
            .count();

        let start_index = if common_prefix == persisted_messages.len() {
            // Agent::prompt records the same user message that CodingAgent
            // already stored for normal prompts. Avoid storing that duplicate.
            if matches!(
                (state_messages.get(common_prefix), persisted_messages.last()),
                (Some(state_message), Some(persisted_message))
                    if state_message.same_user_message(persisted_message)
            ) {
                common_prefix + 1
            } else {
                common_prefix
            }
        } else if persisted_messages.len() == common_prefix + 1
            && state_messages
                .get(common_prefix)
                .is_some_and(AgentMessage::is_user)
        {
            // Skills and extensions store the visible command, then prompt
            // the model with a different, generated user message. Keep that
            // generated message so the restored provider history is exact.
            common_prefix
        } else if state_messages
            .iter()
            .any(|message| mypi_agent::compaction_summary_text(message).is_some())
        {
            // Auto-compaction creates a new active root branch. Persist that
            // branch in-place instead of treating it as a new session.
            let current_turn_start = state_messages
                .iter()
                .rposition(AgentMessage::is_user)
                .unwrap_or(state_messages.len());
            for message in state_messages.iter().skip(current_turn_start + 1) {
                self.dispatch_assistant_hook(message).await;
            }
            self.session_tree.replace_active_branch(state_messages);
            return;
        } else {
            // A non-prefix means the session was changed independently. Do
            // not append a second, potentially duplicated conversation.
            return;
        };

        for message in state_messages.into_iter().skip(start_index) {
            self.dispatch_assistant_hook(&message).await;
            self.session_tree.add_message(message);
        }
    }

    pub async fn switch_session_file(&mut self, session_file: PathBuf) {
        let session_tree = if session_file.exists() {
            SessionTree::load_from_file(&session_file).unwrap_or_else(|_| {
                let mut tree = SessionTree::new(
                    session_file
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "session".into()),
                );
                tree.file_path = Some(session_file.clone());
                tree
            })
        } else {
            if let Some(parent) = session_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut tree = SessionTree::new(
                session_file
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "session".into()),
            );
            tree.file_path = Some(session_file);
            tree
        };

        let branch = session_tree.get_active_branch_messages();
        self.wasi_extensions
            .set_session_scope(session_tree.session_id.clone())
            .unwrap_or_else(|error| {
                eprintln!("Failed to restore session extension state: {error}")
            });
        *self.tool_policy.lock().await = restored_tool_policy(&self.wasi_extensions);
        self.agent
            .set_prompt_cache_key(Some(session_tree.session_id.clone()));
        self.session_tree = session_tree;

        let mut state = self.agent.loop_engine.state.lock().await;
        let system_prompt = state.system_prompt.clone();
        state.messages.clear();
        state.messages.push(AgentMessage::System {
            content: system_prompt,
        });
        for msg in branch {
            if matches!(msg, AgentMessage::System { .. }) {
                continue;
            }
            state.messages.push(msg);
        }
        state.is_streaming = false;
        state.pending_tool_calls.clear();
    }

    pub fn session_file_path(&self) -> Option<&PathBuf> {
        self.session_tree.file_path.as_ref()
    }

    #[cfg(test)]
    fn set_subagent_work_observer(&self, observer: Arc<std::sync::Mutex<Vec<AgentWork>>>) {
        if let Ok(mut current) = self.subagent_work_observer.lock() {
            *current = Some(observer);
        }
    }

    pub async fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.agent.set_reasoning_effort(effort).await;
    }

    pub async fn handle_input(&mut self, input: &str) -> Option<String> {
        self.handle_input_with_images(input, Vec::new()).await
    }

    pub async fn handle_input_with_images(
        &mut self,
        input: &str,
        images: Vec<ImageAttachment>,
    ) -> Option<String> {
        let trimmed = input.trim();

        // 1. Expand prompt templates (e.g. /review, /component Button) if match
        let global_dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".mypi"))
            .unwrap_or_else(|| self.work_dir.join(".mypi"));
        let templates = crate::prompt_templates::load_prompt_templates(&self.work_dir, &global_dir);
        let expanded_input = crate::prompt_templates::expand_prompt_template(trimmed, &templates);
        let effective_input = expanded_input.trim();

        if effective_input.starts_with('/') {
            let mut parts = effective_input[1..].split_whitespace();
            let cmd_name = parts.next().unwrap_or("");
            let cmd_args = parts.collect::<Vec<&str>>().join(" ");

            if cmd_name.starts_with("skill:") || cmd_name == "skill" {
                let skill_name = if cmd_name.starts_with("skill:") {
                    &cmd_name[6..]
                } else {
                    cmd_args.trim()
                };

                match self.skills.get_skill_instructions(skill_name) {
                    Ok(instructions) => {
                        let prompt = format!(
                            "Use the following Skill instructions for '{}':\n\n{}",
                            skill_name, instructions
                        );
                        self.session_tree
                            .add_message(AgentMessage::user(input, images.clone()));
                        self.agent
                            .prompt_message(AgentMessage::user(prompt, images.clone()))
                            .await;
                        self.dispatch_assistant_message_hooks().await;
                        self.run_scheduled_agent_work().await;
                        return Some(format!("Loaded skill '{}'", skill_name));
                    }
                    Err(err) => return Some(format!("Skill Error: {}", err)),
                }
            }

            if let Some(res) = self
                .wasi_extensions
                .execute_command_with_effects(cmd_name, &cmd_args)
            {
                self.session_tree
                    .add_message(AgentMessage::user(input, images.clone()));
                return match res {
                    Ok(result) => {
                        let message = if result.message.is_empty() {
                            None
                        } else {
                            Some(result.message)
                        };
                        let dispatch = match self
                            .broker_dispatcher
                            .dispatch_envelopes(result.host_broker_requests)
                            .await
                        {
                            Ok(dispatch) => dispatch,
                            Err(error) => {
                                return Some(format!("WASI Broker Error: {}", error.message))
                            }
                        };
                        let agent_run_output =
                            dispatch.operation_results.iter().find_map(|result| {
                                if result.request.capability != "agent"
                                    || result.request.operation != "run"
                                {
                                    return None;
                                }
                                if let Some(error) = &result.error {
                                    return Some(Err(format!(
                                        "WASI Broker Error: {}",
                                        error.message
                                    )));
                                }
                                let output = result.value["output"].as_str().ok_or_else(|| {
                                    "agent.run returned no formatted output".to_string()
                                });
                                let thinking = serde_json::from_value::<Vec<AgentMessage>>(
                                    result.value["thinking"].clone(),
                                )
                                .map_err(|error| {
                                    format!("agent.run returned invalid thinking: {error}")
                                });
                                match (output, thinking) {
                                    (Ok(output), Ok(thinking)) => {
                                        for message in thinking {
                                            self.session_tree.add_message(message);
                                        }
                                        self.session_tree.add_message(AgentMessage::Assistant {
                                            content: Some(output.to_string()),
                                            tool_calls: None,
                                        });
                                        Some(Ok(output.to_string()))
                                    }
                                    (Err(error), _) | (_, Err(error)) => Some(Err(error)),
                                }
                            });
                        self.wasi_extensions
                            .enqueue_broker_results(dispatch.operation_results);
                        self.run_scheduled_agent_work().await;
                        if result.api_version == 1 {
                            for effect in result.effects {
                                match effect {
                                    WasiLegacyEffect::SetToolPolicy { policy } => {
                                        let mut pol = self.tool_policy.lock().await;
                                        match policy.as_str() {
                                            "read_only" => *pol = ToolPolicy::ReadOnly,
                                            "full" => *pol = ToolPolicy::FullAccess,
                                            _ => continue,
                                        }
                                    }
                                    WasiLegacyEffect::RequestModelTurn { prompt } => {
                                        self.agent.prompt(&prompt).await;
                                        self.dispatch_assistant_message_hooks().await;
                                    }
                                }
                            }
                        }
                        if let Some(agent_run_output) = agent_run_output {
                            return Some(match agent_run_output {
                                Ok(output) => output,
                                Err(error) => error,
                            });
                        }
                        message
                    }
                    Err(err) => Some(format!("WASI Extension Error: {}", err)),
                };
            }

            if let Some(cmd_action) = parse_slash_command(effective_input) {
                if cmd_action == CommandAction::Quit {
                    return Some("quitting".to_string());
                }
                let output =
                    execute_slash_command(cmd_action, &mut self.agent, &mut self.session_tree)
                        .await;
                return Some(output);
            }
        }

        if self.agent.auto_compact_history().await {
            let state = self.agent.get_state().await;
            self.session_tree.replace_active_branch(state.messages);
        }

        let msg = AgentMessage::user(effective_input, images);
        self.session_tree.add_message(msg.clone());
        self.agent.prompt_message(msg).await;
        self.dispatch_assistant_message_hooks().await;
        self.run_scheduled_agent_work().await;

        None
    }
}

async fn run_subagents_with_context(
    tasks: Vec<AgentRunTask>,
    parallel: bool,
    api_key: String,
    account_id: Option<String>,
    parent_model: String,
    work_dir: PathBuf,
    extensions: Arc<WasiExtensionManager>,
    parent_event_tx: broadcast::Sender<AgentEvent>,
    scheduler_observer: Option<Arc<std::sync::Mutex<Vec<AgentWork>>>>,
    semaphore: Arc<tokio::sync::Semaphore>,
) -> Result<(String, Vec<AgentMessage>), String> {
    let run_one = |task: AgentRunTask| {
        let config = discover_agents(&work_dir, AgentScope::Both)
            .agents
            .into_iter()
            .find(|candidate| candidate.name == task.agent)
            .ok_or_else(|| {
                format!(
                    "Unknown subagent '{}'. Add it to .mypi/agents or ~/.mypi/agents.",
                    task.agent
                )
            });
        let semaphore = semaphore.clone();
        let api_key = api_key.clone();
        let account_id = account_id.clone();
        let parent_model = parent_model.clone();
        let work_dir = work_dir.clone();
        let extensions = extensions.clone();
        let parent_event_tx = parent_event_tx.clone();
        let scheduler_observer = scheduler_observer.clone();
        async move {
            let config = config?;
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|_| "Subagent concurrency limiter closed".to_string())?;
            timeout(
                SUBAGENT_TIMEOUT,
                run_subagent_task(
                    config,
                    task.task,
                    api_key,
                    account_id,
                    parent_model,
                    work_dir,
                    extensions,
                    parent_event_tx,
                    scheduler_observer,
                ),
            )
            .await
            .map_err(|_| "Subagent timed out".to_string())?
        }
    };
    let results = if parallel {
        futures::future::join_all(tasks.iter().cloned().map(run_one)).await
    } else {
        let mut previous = String::new();
        let mut results = Vec::with_capacity(tasks.len());
        for task in tasks.iter().cloned() {
            let task = AgentRunTask {
                agent: task.agent,
                task: task.task.replace("{previous}", &previous),
            };
            let result = run_one(task).await;
            if let Ok(result) = &result {
                previous = result.output.clone();
            }
            results.push(result);
        }
        results
    };
    let thinking = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .flat_map(|result| result.thinking.clone())
        .collect();
    Ok((format_subagent_results(tasks, results), thinking))
}

async fn run_subagent_task(
    config: AgentConfig,
    task: String,
    api_key: String,
    account_id: Option<String>,
    parent_model: String,
    work_dir: PathBuf,
    extensions: Arc<WasiExtensionManager>,
    parent_event_tx: broadcast::Sender<AgentEvent>,
    _scheduler_observer: Option<Arc<std::sync::Mutex<Vec<AgentWork>>>>,
) -> Result<SubagentResult, String> {
    let model = config.model.clone().unwrap_or(parent_model);
    #[cfg(test)]
    if let Some(observer) = _scheduler_observer.as_ref() {
        let scheduler = AgentWorkScheduler::default();
        scheduler.set_test_observer(observer.clone());
        scheduler.schedule(AgentWork::QueueMessage("test subagent follow-up".into()));
        let observed_model = model.clone();
        let mut agent = Agent::new(api_key, account_id, model);
        let _ = scheduler.run(&mut agent).await;
        return Ok(SubagentResult {
            output: format!("test subagent result ({observed_model})"),
            thinking: Vec::new(),
        });
    }
    let mut agent = Agent::new(api_key, account_id, model);
    if let Some(tools) = config.tools.clone() {
        agent
            .loop_engine
            .set_allowed_tool_names(Some(tools.into_iter().collect()));
    }
    let system_prompt = format!(
        "{}\n\nYou are an isolated subagent working in {}. Complete only the assigned task and return a concise final report to your parent agent.",
        config.system_prompt,
        work_dir.display(),
    );
    agent.set_system_prompt(system_prompt).await;
    agent.loop_engine.work_dir = Some(work_dir.clone());

    let policy = Arc::new(tokio::sync::Mutex::new(
        if config.tools.as_ref().is_some_and(|tools| {
            !tools
                .iter()
                .any(|tool| matches!(tool.as_str(), "write_file" | "edit_file" | "write" | "edit"))
        }) {
            ToolPolicy::ReadOnly
        } else {
            ToolPolicy::FullAccess
        },
    ));
    let agent_work = AgentWorkScheduler::default();
    let broker_dispatcher = build_broker_dispatcher(
        policy.clone(),
        extensions.clone(),
        false,
        work_dir.clone(),
        agent.loop_engine.event_tx.clone(),
        agent_work.clone(),
        None,
    );
    agent.loop_engine.before_tool_call_hook = Some(Arc::new(ExtensionBeforeToolHook {
        tool_policy: policy,
        extensions: extensions.clone(),
        broker_dispatcher: broker_dispatcher.clone(),
    }));
    agent.loop_engine.after_tool_call_hook = Some(Arc::new(ExtensionAfterToolHook {
        extensions,
        broker_dispatcher,
    }));

    // The GUI subscribes only to the parent agent. Relay child lifecycle,
    // reasoning, and tool events so users can see subagent progress live.
    // Assistant text stays local and is returned below as one labelled result.
    let mut ui_events = agent.subscribe();
    let ui_event_prefix = format!(
        "subagent-{}:",
        NEXT_SUBAGENT_UI_ID.fetch_add(1, Ordering::Relaxed)
    );
    tokio::spawn(async move {
        while let Ok(event) = ui_events.recv().await {
            if let Some(event) = subagent_ui_event(event, &ui_event_prefix) {
                let _ = parent_event_tx.send(event);
            }
        }
    });

    // Preserve provider and tool-loop errors in the command result as well.
    let mut events = agent.subscribe();
    agent.prompt(&task).await;
    while agent_work.run(&mut agent).await {}

    let mut error = None;
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::AgentError { error: message } = event {
            error = Some(message);
        }
    }
    if let Some(error) = error {
        return Err(format!("Subagent '{}' failed: {error}", config.name));
    }

    let state = agent.get_state().await;
    let output = state
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            AgentMessage::Assistant {
                content: Some(content),
                ..
            } => Some(content.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            format!(
                "Subagent '{}' completed without a final text response.",
                config.name
            )
        })?;
    let thinking = state
        .messages
        .into_iter()
        .filter(|message| matches!(message, AgentMessage::Custom { custom_type, .. } if custom_type == "thinking"))
        .collect();
    Ok(SubagentResult { output, thinking })
}

fn subagent_ui_event(event: AgentEvent, tool_call_prefix: &str) -> Option<AgentEvent> {
    match event {
        // Parent lifecycle and the outer subagent tool own GUI status. Relaying a
        // child's lifecycle would mark a parallel delegation ready or failed
        // while sibling tasks and the parent turn are still running.
        AgentEvent::AgentStart
        | AgentEvent::AgentEnd { .. }
        | AgentEvent::AgentError { .. }
        | AgentEvent::TurnStart { .. }
        | AgentEvent::TurnEnd { .. } => None,
        // Keep internal child prose out of the transcript: the labelled tool or
        // command result is the child’s final report. Reasoning remains visible.
        AgentEvent::MessageUpdate {
            reasoning_delta: Some(reasoning_delta),
            tool_call_name,
            ..
        } => Some(AgentEvent::MessageUpdate {
            text_delta: None,
            reasoning_delta: Some(reasoning_delta),
            tool_call_name,
        }),
        AgentEvent::MessageUpdate { .. } => None,
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            name,
            arguments,
        } => Some(AgentEvent::ToolExecutionStart {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            name,
            arguments,
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            partial_result,
        } => Some(AgentEvent::ToolExecutionUpdate {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            partial_result,
        }),
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            name,
            result,
        } => Some(AgentEvent::ToolExecutionEnd {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            name,
            result,
        }),
        event => Some(event),
    }
}

struct SubagentResult {
    output: String,
    thinking: Vec<AgentMessage>,
}

fn format_subagent_results(
    tasks: Vec<AgentRunTask>,
    results: Vec<Result<SubagentResult, String>>,
) -> String {
    tasks
        .into_iter()
        .zip(results)
        .enumerate()
        .map(|(index, (task, result))| match result {
            Ok(result) => format!(
                "## Subagent {}: {}\n\n{}",
                index + 1,
                task.agent,
                result.output
            ),
            Err(error) => format!(
                "## Subagent {}: {} (failed)\n\n{}",
                index + 1,
                task.agent,
                error
            ),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_broker::CapabilityHandler;
    use std::sync::Mutex;
    use std::time::{Duration as StdDuration, Instant};

    #[tokio::test]
    async fn switching_sessions_updates_prompt_cache_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let initial_key = coding_agent.agent.loop_engine.prompt_cache_key.clone();
        assert_eq!(
            initial_key.as_deref(),
            Some(coding_agent.session_tree.session_id.as_str())
        );

        coding_agent
            .switch_session_file(dir.path().join("sessions/other.jsonl"))
            .await;

        let switched_key = coding_agent.agent.loop_engine.prompt_cache_key.clone();
        assert_eq!(
            switched_key.as_deref(),
            Some(coding_agent.session_tree.session_id.as_str())
        );
        assert_ne!(switched_key, initial_key);
    }

    #[test]
    fn subagent_ui_events_do_not_override_parent_lifecycle() {
        assert!(subagent_ui_event(AgentEvent::AgentStart, "child:").is_none());
        assert!(subagent_ui_event(
            AgentEvent::AgentEnd {
                usage: Default::default()
            },
            "child:"
        )
        .is_none());
        assert!(subagent_ui_event(
            AgentEvent::AgentError {
                error: "child failed".into()
            },
            "child:"
        )
        .is_none());

        let reasoning = subagent_ui_event(
            AgentEvent::MessageUpdate {
                text_delta: Some("hidden child prose".into()),
                reasoning_delta: Some("visible progress".into()),
                tool_call_name: None,
            },
            "child:",
        );
        assert!(matches!(
            reasoning,
            Some(AgentEvent::MessageUpdate {
                text_delta: None,
                reasoning_delta: Some(text),
                ..
            }) if text == "visible progress"
        ));

        let tool = subagent_ui_event(
            AgentEvent::ToolExecutionStart {
                tool_call_id: "tool".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            },
            "child:",
        );
        assert!(matches!(
            tool,
            Some(AgentEvent::ToolExecutionStart { tool_call_id, .. })
                if tool_call_id == "child:tool"
        ));
    }

    fn handler(capability: &'static str, work_dir: PathBuf) -> HostCapabilityHandler {
        handler_with_scheduler(capability, work_dir, AgentWorkScheduler::default())
    }

    fn handler_with_scheduler(
        capability: &'static str,
        work_dir: PathBuf,
        agent_work: AgentWorkScheduler,
    ) -> HostCapabilityHandler {
        let (event_tx, _) = broadcast::channel(4);
        HostCapabilityHandler {
            capability,
            tool_policy: None,
            extensions: Arc::new(WasiExtensionManager::new()),
            work_dir,
            event_tx,
            allowed_hosts: Arc::new(HashSet::new()),
            agent_work,
            agent_runner: None,
            persist_tool_policy: false,
        }
    }

    fn push_unsigned_leb(mut value: u32, bytes: &mut Vec<u8>) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn push_signed_leb(mut value: i64, bytes: &mut Vec<u8>) {
        loop {
            let byte = (value as u8) & 0x7f;
            value >>= 7;
            let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
            bytes.push(if done { byte } else { byte | 0x80 });
            if done {
                break;
            }
        }
    }

    fn push_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
        wasm.push(id);
        push_unsigned_leb(payload.len() as u32, wasm);
        wasm.extend_from_slice(payload);
    }

    fn queue_command_wasm() -> Vec<u8> {
        let manifest = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "name": "queue_command_ext",
            "version": "1.0.0",
            "description": "scheduler integration fixture",
            "capabilities": ["agent"],
            "commands": [{"name": "queue", "description": "queue follow-up"}]
        })
        .to_string();
        let request = serde_json::json!({
            "api_version": BROKER_API_VERSION,
            "capability": "agent",
            "operation": "queue_message",
            "arguments": {"content": "standalone queued work"}
        })
        .to_string();
        let response = br#"{"message":"queued"}"#;
        let response_offset = 1024usize;
        let request_offset = 4096usize;
        let request_response_offset = 6000usize;
        let mut data = vec![0; request_response_offset + 1024];
        data[..manifest.len()].copy_from_slice(manifest.as_bytes());
        data[response_offset..response_offset + response.len()].copy_from_slice(response);
        data[request_offset..request_offset + request.len()].copy_from_slice(request.as_bytes());

        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(
            &mut wasm,
            1,
            &[
                4, 0x60, 0, 1, 0x7e, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 2, 0x7f, 0x7f, 1, 0x7e, 0x60, 4,
                0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f,
            ],
        );
        let mut imports = vec![1, 9];
        imports.extend_from_slice(b"mypi_host");
        imports.push(7);
        imports.extend_from_slice(b"request");
        imports.extend_from_slice(&[0, 3]);
        push_section(&mut wasm, 2, &imports);
        push_section(&mut wasm, 3, &[3, 0, 1, 2]);
        push_section(&mut wasm, 5, &[1, 0, 2]);

        let mut exports = vec![4];
        for (name, kind, index) in [
            ("extension_info", 0, 1),
            ("alloc", 0, 2),
            ("execute_command", 0, 3),
            ("memory", 2, 0),
        ] {
            push_unsigned_leb(name.len() as u32, &mut exports);
            exports.extend_from_slice(name.as_bytes());
            exports.extend_from_slice(&[kind, index]);
        }
        push_section(&mut wasm, 7, &exports);

        let mut bodies = Vec::new();
        for body in [
            {
                let mut body = vec![0, 0x42];
                push_signed_leb(manifest.len() as i64, &mut body);
                body.push(0x0b);
                body
            },
            vec![0, 0x41, 0],
            {
                let mut body = vec![0, 0x41];
                push_signed_leb(request_offset as i64, &mut body);
                body.push(0x41);
                push_signed_leb(request.len() as i64, &mut body);
                body.push(0x41);
                push_signed_leb(request_response_offset as i64, &mut body);
                body.push(0x41);
                push_signed_leb(1024, &mut body);
                body.extend_from_slice(&[0x10, 0, 0x1a, 0x42]);
                let packed = ((response_offset as u64) << 32) | response.len() as u64;
                push_signed_leb(packed as i64, &mut body);
                body.push(0x0b);
                body
            },
        ] {
            let mut full = body;
            if full.last() != Some(&0x0b) {
                full.push(0x0b);
            }
            push_unsigned_leb(full.len() as u32, &mut bodies);
            bodies.extend_from_slice(&full);
        }
        let mut code = vec![3];
        code.extend_from_slice(&bodies);
        push_section(&mut wasm, 10, &code);
        let mut data_section = vec![1, 0, 0x41, 0, 0x0b];
        push_unsigned_leb(data.len() as u32, &mut data_section);
        data_section.extend_from_slice(&data);
        push_section(&mut wasm, 11, &data_section);
        wasm
    }

    fn coding_agent_options(work_dir: PathBuf) -> CodingAgentOptions {
        CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir,
            session_file: None,
            system_prompt: SystemPromptConfig::default(),
        }
    }

    fn provider_tool_call(
        id: &str,
        name: &str,
        arguments: serde_json::Value,
    ) -> mypi_provider::openai::ToolCall {
        mypi_provider::openai::ToolCall {
            id: id.into(),
            r#type: "function".into(),
            function: mypi_provider::openai::ToolCallFunction {
                name: name.into(),
                arguments: arguments.to_string(),
            },
        }
    }

    #[tokio::test]
    async fn coding_agent_builds_configurable_structured_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Always add focused tests.").unwrap();
        let mut options = coding_agent_options(dir.path().to_path_buf());
        options.system_prompt = SystemPromptConfig {
            custom_prompt: Some("CUSTOM_BASE".into()),
            append_prompt: Some("APPENDED_RULE".into()),
            guidelines: Vec::new(),
        };

        let coding_agent = CodingAgent::new(options);
        let state = coding_agent.agent.get_state().await;

        assert!(state
            .system_prompt
            .starts_with("CUSTOM_BASE\n\nAPPENDED_RULE"));
        assert!(state.system_prompt.contains("<project_context>"));
        assert!(state.system_prompt.contains("Always add focused tests."));
        assert!(state.system_prompt.contains("Current working directory:"));
        assert_eq!(coding_agent.base_system_prompt(), state.system_prompt);
    }

    #[tokio::test]
    async fn coding_agent_advertises_and_executes_discovered_skills() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".mypi/skills/test-workflow");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: test-workflow\ndescription: Use for deterministic integration tests\n---\nBODY_SENTINEL",
        )
        .unwrap();

        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let state = coding_agent.agent.get_state().await;
        assert!(state.system_prompt.contains("`test-workflow`"));
        assert!(state
            .system_prompt
            .contains("Use for deterministic integration tests"));
        assert!(!state.system_prompt.contains("BODY_SENTINEL"));
        assert!(state.system_prompt.contains("- read_file:"));
        assert!(state.system_prompt.contains("- load_skill:"));

        let (chat, codex) = coding_agent.agent.loop_engine.build_api_payloads().await;
        assert!(chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["function"]["name"] == crate::skills::LOAD_SKILL_TOOL_NAME }));
        assert!(codex["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["name"] == crate::skills::LOAD_SKILL_TOOL_NAME }));

        let results = coding_agent
            .agent
            .loop_engine
            .execute_tools(&[provider_tool_call(
                "skill-call",
                crate::skills::LOAD_SKILL_TOOL_NAME,
                serde_json::json!({"name": "test-workflow"}),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error);
        assert!(results[0].content.contains("BODY_SENTINEL"));
    }

    #[tokio::test]
    async fn model_subagent_tool_returns_awaited_child_output() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join(".mypi/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("scout.md"),
            "---\nname: scout\ndescription: deterministic test scout\n---\nTest scout.",
        )
        .unwrap();
        let extension_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.mypi/extensions/subagent_ext.wasm");
        let extension_dir = dir.path().join(".mypi/extensions/subagent_ext");
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::copy(extension_path, extension_dir.join("extension.wasm")).unwrap();

        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        coding_agent.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));
        let (chat, codex) = coding_agent.agent.loop_engine.build_api_payloads().await;
        assert!(chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "subagent"));
        assert!(codex["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "subagent"));

        let results = coding_agent
            .agent
            .loop_engine
            .execute_tools(&[provider_tool_call(
                "subagent-call",
                "subagent",
                serde_json::json!({
                    "tasks": [{"agent": "scout", "task": "inspect the project"}],
                    "parallel": false
                }),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error, "{}", results[0].content);
        assert!(results[0]
            .content
            .contains("test subagent result (test-model)"));
        assert!(!results[0].content.contains("Running 1 subagent task"));
    }

    #[tokio::test]
    async fn malformed_model_subagent_tool_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let extension_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.mypi/extensions/subagent_ext.wasm");
        let extension_dir = dir.path().join(".mypi/extensions/subagent_ext");
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::copy(extension_path, extension_dir.join("extension.wasm")).unwrap();
        let coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));

        let results = coding_agent
            .agent
            .loop_engine
            .execute_tools(&[provider_tool_call(
                "invalid-subagent-call",
                "subagent",
                serde_json::json!({
                    "tasks": [{"agent": "scout", "task": ""}],
                    "parallel": false
                }),
            )])
            .await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(results[0].content.contains("Usage: /subagent"));
    }

    #[tokio::test]
    async fn standalone_extension_command_runs_scheduled_agent_work() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = queue_command_wasm();
        let extension_dir = dir.path().join(".mypi/extensions/queue_command_ext");
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::write(extension_dir.join("extension.wasm"), wasm).unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        assert!(coding_agent.wasi_extensions.has_command("queue"));
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.agent_work.set_test_observer(observed.clone());

        let output = coding_agent.handle_input("/queue").await;

        assert_eq!(output.as_deref(), Some("queued"));
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::QueueMessage("standalone queued work".into())]
        );
    }

    #[tokio::test]
    async fn generic_agent_run_inherits_parent_current_model_for_tasks_without_model() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join(".mypi/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("scout.md"),
            "---\nname: scout\ndescription: deterministic test scout\n---\nTest scout.",
        )
        .unwrap();
        let extension_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.mypi/extensions/subagent_ext.wasm");
        let extension_dir = dir.path().join(".mypi/extensions/subagent_ext");
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::copy(extension_path, extension_dir.join("extension.wasm")).unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        let observed = Arc::new(Mutex::new(Vec::new()));
        coding_agent.set_subagent_work_observer(observed.clone());
        coding_agent.agent.loop_engine.state.lock().await.model = "changed-model".into();

        let output = coding_agent
            .handle_input("/subagent inspect the project")
            .await;

        assert!(output
            .unwrap()
            .contains("test subagent result (changed-model)"));
        assert_eq!(
            *observed.lock().unwrap(),
            vec![AgentWork::QueueMessage("test subagent follow-up".into())]
        );
    }

    #[tokio::test]
    async fn subagent_command_delivers_agent_run_result_to_same_extension_next_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join(".mypi/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("scout.md"),
            "---\nname: scout\ndescription: deterministic test scout\n---\nTest scout.",
        )
        .unwrap();
        let extension_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.mypi/extensions/subagent_ext.wasm");
        let extension_dir = dir.path().join(".mypi/extensions/subagent_ext");
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::copy(extension_path, extension_dir.join("extension.wasm")).unwrap();
        let mut coding_agent = CodingAgent::new(coding_agent_options(dir.path().to_path_buf()));
        coding_agent.set_subagent_work_observer(Arc::new(Mutex::new(Vec::new())));

        let output = coding_agent
            .handle_input("/subagent inspect the project")
            .await
            .unwrap();
        assert!(output.contains("test subagent result"));

        let next = coding_agent
            .wasi_extensions
            .execute_command_with_effects("subagent", "inspect the project")
            .unwrap()
            .unwrap();
        assert!(next.events.iter().any(|event| {
            event.topic == "broker_response"
                && event.payload["capability"] == "agent"
                && event.payload["operation"] == "run"
        }));
    }

    #[test]
    fn generic_tool_policy_state_restores_by_session() {
        let dir = tempfile::tempdir().unwrap();
        let manager = WasiExtensionManager::for_project_session(dir.path(), "session-a");
        manager
            .set_host_state("tools.policy", Value::String("read_only".into()))
            .unwrap();

        let restored = WasiExtensionManager::for_project_session(dir.path(), "session-a");
        assert_eq!(
            restored.host_state("tools.policy"),
            Some(Value::String("read_only".into()))
        );
    }

    #[test]
    fn tool_policy_is_unchanged_when_host_state_persistence_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mypi"), "not a directory").unwrap();
        let policy = Arc::new(tokio::sync::Mutex::new(ToolPolicy::FullAccess));
        let tools = HostCapabilityHandler {
            tool_policy: Some(policy.clone()),
            extensions: Arc::new(WasiExtensionManager::for_project_session(
                dir.path(),
                "session-a",
            )),
            persist_tool_policy: true,
            ..handler("tools", dir.path().to_path_buf())
        };
        let request = BrokerRequest {
            api_version: BROKER_API_VERSION,
            capability: "tools".into(),
            operation: "set_policy".into(),
            arguments: serde_json::json!({"policy": "read_only"}),
        };

        assert_eq!(tools.handle(&request).unwrap_err().code, "host_error");
        assert_eq!(*policy.try_lock().unwrap(), ToolPolicy::FullAccess);
    }

    #[test]
    fn filesystem_rejects_paths_outside_work_dir() {
        let dir = tempfile::tempdir().unwrap();
        let request = BrokerRequest {
            api_version: 2,
            capability: "fs".into(),
            operation: "read_text".into(),
            arguments: serde_json::json!({"path": "../outside"}),
        };
        let error = handler("fs", dir.path().to_path_buf())
            .handle(&request)
            .unwrap_err();
        assert_eq!(error.code, "invalid_argument");
    }

    struct RecordingBrokerHandler {
        operations: Arc<Mutex<Vec<String>>>,
    }

    impl CapabilityHandler for RecordingBrokerHandler {
        fn handle(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
            self.operations
                .lock()
                .unwrap()
                .push(request.operation.clone());
            if request.operation == "fail" {
                Err(BrokerError {
                    code: "test_error".into(),
                    message: "expected test failure".into(),
                })
            } else {
                Ok(Value::Null)
            }
        }
    }

    #[tokio::test]
    async fn tool_broker_requests_dispatch_in_order_and_isolate_errors() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = CapabilityDispatcher::new();
        dispatcher.register(
            "tools",
            Arc::new(RecordingBrokerHandler {
                operations: operations.clone(),
            }),
        );
        let dispatcher = Arc::new(dispatcher);
        let requests = ["first", "fail", "last"]
            .into_iter()
            .map(|operation| crate::extension_broker::HostBrokerRequest {
                request: BrokerRequest {
                    api_version: BROKER_API_VERSION,
                    capability: "tools".into(),
                    operation: operation.into(),
                    arguments: Value::Null,
                },
                invoking_extension: "tool-ext".into(),
            })
            .collect();

        let extensions = WasiExtensionManager::new();
        dispatch_hook_requests_isolated(&dispatcher, &extensions, requests, "test broker error")
            .await;

        assert_eq!(*operations.lock().unwrap(), vec!["first", "fail", "last"]);
    }

    #[tokio::test]
    async fn process_pipes_output_and_timeout_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let mut request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "run".into(),
            arguments: serde_json::json!({
                "program": "sh",
                "args": ["-c", "printf stdout; printf stderr >&2"]
            }),
        };
        let output = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap();
        let output: Value = serde_json::from_str(output["message"].as_str().unwrap()).unwrap();
        assert_eq!(output["stdout"], "stdout");
        assert_eq!(output["stderr"], "stderr");

        request.arguments = serde_json::json!({"program": "sh", "args": ["-c", "sleep 10"]});
        let started = Instant::now();
        let error = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "timeout");
        assert!(started.elapsed() < StdDuration::from_secs(4));
    }

    #[tokio::test]
    async fn process_output_is_bounded_before_buffering() {
        let dir = tempfile::tempdir().unwrap();
        let process = handler("process", dir.path().to_path_buf());
        let request = BrokerRequest {
            api_version: 2,
            capability: "process".into(),
            operation: "run".into(),
            arguments: serde_json::json!({
                "program": "sh",
                "args": ["-c", format!("head -c {} /dev/zero", MAX_CAPABILITY_BUFFER_BYTES + 1)]
            }),
        };
        let error = process
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "process_output_too_large");
    }

    #[tokio::test]
    async fn network_response_is_bounded_before_buffering() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
            response.resize(MAX_CAPABILITY_BUFFER_BYTES + 1, b'x');
            tokio::io::AsyncWriteExt::write_all(&mut socket, &response)
                .await
                .unwrap();
        });
        let network = HostCapabilityHandler {
            allowed_hosts: Arc::new(HashSet::from(["127.0.0.1".to_string()])),
            ..handler("network", dir.path().to_path_buf())
        };
        let request = BrokerRequest {
            api_version: 2,
            capability: "network".into(),
            operation: "http".into(),
            arguments: serde_json::json!({
                "url": format!("http://127.0.0.1:{port}/"),
                "method": "GET",
                "body": ""
            }),
        };
        let error = network
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "network_response_too_large");
    }

    #[tokio::test]
    async fn network_io_timeout_is_bounded_after_connect() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let network = HostCapabilityHandler {
            allowed_hosts: Arc::new(HashSet::from(["127.0.0.1".to_string()])),
            ..handler("network", dir.path().to_path_buf())
        };
        let request = BrokerRequest {
            api_version: 2,
            capability: "network".into(),
            operation: "http".into(),
            arguments: serde_json::json!({
                "url": format!("http://127.0.0.1:{port}/"),
                "method": "GET",
                "body": ""
            }),
        };
        let started = Instant::now();
        let error = network
            .handle_for_extension_async(&request, "ext")
            .await
            .unwrap_err();
        assert_eq!(error.code, "timeout");
        assert!(started.elapsed() < StdDuration::from_secs(4));
    }
}
