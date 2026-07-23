use crate::agents::{discover_agents, AgentConfig, AgentScope};
use crate::commands::{execute_slash_command, parse_slash_command, CommandAction};
use crate::context::ProjectContext;
use crate::extension_broker::{
    BrokerError, BrokerRequest, CapabilityDispatcher, CapabilityHandler, BROKER_API_VERSION,
};
use crate::wasi_extension::{WasiExtensionManager, WasiSubagentTask};
use async_trait::async_trait;
use mypi_agent::{
    AfterToolCallHook, AfterToolCallResult, Agent, AgentEvent, AgentMessage, AgentState,
    AgentToolCall, AgentToolResult, BeforeToolCallHook, BeforeToolCallResult, SessionTree,
};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::{timeout, Duration};

const CAPABILITY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    FullAccess,
    ReadOnly,
}

struct HostCapabilityHandler {
    capability: &'static str,
    tool_policy: Option<Arc<tokio::sync::Mutex<ToolPolicy>>>,
    extensions: Arc<WasiExtensionManager>,
    work_dir: PathBuf,
    event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    allowed_hosts: Arc<HashSet<String>>,
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
                let mut current = policy
                    .try_lock()
                    .map_err(|_| internal_error("Tool policy is busy"))?;
                *current = match value {
                    "read_only" => ToolPolicy::ReadOnly,
                    "full" => ToolPolicy::FullAccess,
                    _ => return Err(invalid_argument("policy must be `read_only` or `full`")),
                };
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

    fn handle_agent(&self, request: &BrokerRequest) -> Result<Value, BrokerError> {
        match request.operation.as_str() {
            "request_turn" => Ok(serde_json::json!({
                "follow_up_prompt": string_argument(&request.arguments, "prompt")?
            })),
            "queue_message" => Ok(serde_json::json!({
                "queued_agent_prompt": string_argument(&request.arguments, "content")?
            })),
            _ => unknown_operation(self.capability, &request.operation),
        }
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
        let child = command.spawn().map_err(host_error)?;
        let output = timeout(CAPABILITY_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| timeout_error("process.run"))?
            .map_err(host_error)?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| invalid_argument("stdout was not UTF-8"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|_| invalid_argument("stderr was not UTF-8"))?;
        Ok(serde_json::json!({"message": serde_json::json!({
            "exit_code": output.status.code(), "stdout": stdout, "stderr": stderr
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
            let mut response = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut response)
                .await
                .map_err(host_error)?;
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
    pub enable_plan_mode: bool,
}

pub struct ExtensionBeforeToolHook {
    pub tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub extensions: Arc<WasiExtensionManager>,
    pub broker_dispatcher: Arc<tokio::sync::Mutex<CapabilityDispatcher>>,
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
                "write_file" | "edit_file" | "write" | "edit"
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
    pub broker_dispatcher: Arc<tokio::sync::Mutex<CapabilityDispatcher>>,
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

pub struct CodingAgent {
    pub agent: Agent,
    pub session_tree: SessionTree,
    pub project_context: ProjectContext,
    pub wasi_extensions: Arc<WasiExtensionManager>,
    pub tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    pub work_dir: PathBuf,
    broker_dispatcher: Arc<tokio::sync::Mutex<CapabilityDispatcher>>,
    base_system_prompt: String,
}

fn build_broker_dispatcher(
    tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    extensions: Arc<WasiExtensionManager>,
    work_dir: PathBuf,
    event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
) -> Arc<tokio::sync::Mutex<CapabilityDispatcher>> {
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
            }),
        );
    }
    Arc::new(tokio::sync::Mutex::new(dispatcher))
}

async fn dispatch_hook_requests(
    dispatcher: &Arc<tokio::sync::Mutex<CapabilityDispatcher>>,
    extensions: &WasiExtensionManager,
    requests: Vec<crate::extension_broker::HostBrokerRequest>,
) -> Result<(), BrokerError> {
    for request in requests {
        let dispatch = dispatcher
            .lock()
            .await
            .dispatch_envelopes(vec![request])
            .await?;
        extensions.enqueue_broker_results(dispatch.operation_results);
    }
    Ok(())
}

async fn dispatch_hook_requests_isolated(
    dispatcher: &Arc<tokio::sync::Mutex<CapabilityDispatcher>>,
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

        let mut wasi_extensions = WasiExtensionManager::for_project_session(
            &options.work_dir,
            session_tree.session_id.clone(),
        );
        let loaded_ext_count = wasi_extensions.discover_and_load(&options.work_dir);
        let restored_plan_mode = wasi_extensions
            .extension_state("plan_mode_ext")
            .and_then(|state| state.get("enabled").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        let tool_policy = Arc::new(tokio::sync::Mutex::new(
            if options.enable_plan_mode || restored_plan_mode {
                ToolPolicy::ReadOnly
            } else {
                ToolPolicy::FullAccess
            },
        ));

        let mut system_prompt = format!(
            "You are mypi, an AI coding agent with tool execution capability in workspace: {}.\n\
            Always use the provided tools (read_file, write_file, edit_file, list_dir, run_command) \
            to inspect code, modify files, and run tests. Be precise, concise, and double-check your work.",
            options.work_dir.display()
        );

        if loaded_ext_count > 0 {
            system_prompt.push_str(&format!(
                "\n\nLoaded {} WASI extensions into sandboxed execution environment.",
                loaded_ext_count
            ));
        }

        if !project_context.combined_instructions.is_empty() {
            system_prompt.push_str("\n\n=== Workspace Instructions ===");
            system_prompt.push_str(&project_context.combined_instructions);
        }

        let base_system_prompt = system_prompt.clone();
        let wasi_extensions = Arc::new(wasi_extensions);
        let broker_dispatcher = build_broker_dispatcher(
            tool_policy.clone(),
            wasi_extensions.clone(),
            options.work_dir.clone(),
            agent.loop_engine.event_tx.clone(),
        );
        agent.loop_engine.extension_manager = Some(wasi_extensions.clone());
        agent.loop_engine.work_dir = Some(options.work_dir.clone());

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
            state.tools.extend(wasi_extensions.get_tools());
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
            broker_dispatcher,
            base_system_prompt,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.agent.subscribe()
    }

    async fn run_subagent(&self, task: &WasiSubagentTask) -> Result<SubagentResult, String> {
        let config = discover_agents(&self.work_dir, AgentScope::Both)
            .agents
            .into_iter()
            .find(|candidate| candidate.name == task.agent)
            .ok_or_else(|| {
                format!(
                    "Unknown subagent '{}'. Add it to .mypi/agents or ~/.mypi/agents.",
                    task.agent
                )
            })?;
        run_subagent_task(
            config,
            task.task.clone(),
            self.agent.loop_engine.api_key.clone(),
            self.agent.loop_engine.account_id.clone(),
            self.agent.get_state().await.model,
            self.work_dir.clone(),
            self.wasi_extensions.clone(),
            self.agent.loop_engine.event_tx.clone(),
        )
        .await
    }

    async fn run_subagents(&mut self, tasks: Vec<WasiSubagentTask>, parallel: bool) -> String {
        let results = if parallel {
            let futures = tasks.iter().map(|task| self.run_subagent(task));
            futures::future::join_all(futures).await
        } else {
            let mut previous = String::new();
            let mut results = Vec::with_capacity(tasks.len());
            for task in &tasks {
                let task = WasiSubagentTask {
                    agent: task.agent.clone(),
                    task: task.task.replace("{previous}", &previous),
                };
                let result = self.run_subagent(&task).await;
                if let Ok(result) = &result {
                    previous = result.output.clone();
                }
                results.push(result);
            }
            results
        };

        for result in &results {
            if let Ok(result) = result {
                for thinking in &result.thinking {
                    self.session_tree.add_message(thinking.clone());
                }
            }
        }
        format_subagent_results(tasks, results)
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
                (Some(AgentMessage::User { content: state_content }),
                    Some(AgentMessage::User { content: persisted_content }))
                    if state_content == persisted_content
            ) {
                common_prefix + 1
            } else {
                common_prefix
            }
        } else if persisted_messages.len() == common_prefix + 1
            && matches!(
                state_messages.get(common_prefix),
                Some(AgentMessage::User { .. })
            )
        {
            // Skills and extensions store the visible command, then prompt
            // the model with a different, generated user message. Keep that
            // generated message so the restored provider history is exact.
            common_prefix
        } else {
            // A non-prefix means the session was changed independently. Do
            // not append a second, potentially duplicated conversation.
            return;
        };

        for message in state_messages.into_iter().skip(start_index) {
            if let AgentMessage::Assistant {
                content,
                tool_calls,
            } = &message
            {
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
        let restored_plan_mode = self
            .wasi_extensions
            .extension_state("plan_mode_ext")
            .and_then(|state| state.get("enabled").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        *self.tool_policy.lock().await = if restored_plan_mode {
            ToolPolicy::ReadOnly
        } else {
            ToolPolicy::FullAccess
        };
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

    pub async fn handle_input(&mut self, input: &str) -> Option<String> {
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

                let mut skill_mgr = crate::skills::SkillManager::new();
                skill_mgr.discover_skills(Some(&self.work_dir));
                match skill_mgr.get_skill_instructions(skill_name) {
                    Ok(instructions) => {
                        let prompt = format!(
                            "Use the following Skill instructions for '{}':\n\n{}",
                            skill_name, instructions
                        );
                        self.session_tree.add_message(AgentMessage::User {
                            content: input.to_string(),
                        });
                        self.agent.prompt(&prompt).await;
                        self.dispatch_assistant_message_hooks().await;
                        return Some(format!("Loaded skill '{}'", skill_name));
                    }
                    Err(err) => return Some(format!("Skill Error: {}", err)),
                }
            }

            if let Some(res) = self
                .wasi_extensions
                .execute_command_with_effects(cmd_name, &cmd_args)
            {
                self.session_tree.add_message(AgentMessage::User {
                    content: input.to_string(),
                });
                return match res {
                    Ok(result) => {
                        let message = if result.message.is_empty() {
                            None
                        } else {
                            Some(result.message)
                        };
                        let dispatch = match self
                            .broker_dispatcher
                            .lock()
                            .await
                            .dispatch_envelopes(result.host_broker_requests)
                            .await
                        {
                            Ok(dispatch) => dispatch,
                            Err(error) => {
                                return Some(format!("WASI Broker Error: {}", error.message))
                            }
                        };
                        self.wasi_extensions
                            .enqueue_broker_results(dispatch.operation_results);
                        for effect in result.effects {
                            match effect {
                                crate::wasi_extension::WasiExtensionEffect::SetToolPolicy {
                                    policy,
                                } => {
                                    let mut pol = self.tool_policy.lock().await;
                                    match policy.as_str() {
                                        "read_only" => *pol = ToolPolicy::ReadOnly,
                                        "full" => *pol = ToolPolicy::FullAccess,
                                        _ => continue,
                                    }
                                }
                                crate::wasi_extension::WasiExtensionEffect::RequestModelTurn {
                                    prompt,
                                } => {
                                    self.agent.prompt(&prompt).await;
                                    self.dispatch_assistant_message_hooks().await;
                                }
                                crate::wasi_extension::WasiExtensionEffect::RunSubagents {
                                    tasks,
                                    parallel,
                                } => {
                                    let output = self.run_subagents(tasks, parallel).await;
                                    self.session_tree.add_message(AgentMessage::Assistant {
                                        content: Some(output.clone()),
                                        tool_calls: None,
                                    });
                                    return Some(output);
                                }
                            }
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

        let msg = AgentMessage::User {
            content: effective_input.to_string(),
        };
        self.session_tree.add_message(msg);
        self.agent.prompt(effective_input).await;
        self.dispatch_assistant_message_hooks().await;

        None
    }
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
) -> Result<SubagentResult, String> {
    let model = config.model.clone().unwrap_or(parent_model);
    let mut agent = Agent::new(api_key, account_id, model);
    let system_prompt = format!(
        "{}\n\nYou are an isolated subagent working in {}. Complete only the assigned task and return a concise final report to your parent agent.",
        config.system_prompt,
        work_dir.display(),
    );
    agent.set_system_prompt(system_prompt).await;
    agent.loop_engine.work_dir = Some(work_dir.clone());
    agent.loop_engine.extension_manager = Some(extensions.clone());

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
    let broker_dispatcher = build_broker_dispatcher(
        policy.clone(),
        extensions.clone(),
        work_dir.clone(),
        agent.loop_engine.event_tx.clone(),
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
    tokio::spawn(async move {
        while let Ok(event) = ui_events.recv().await {
            if let Some(event) = subagent_ui_event(event) {
                let _ = parent_event_tx.send(event);
            }
        }
    });

    // Preserve provider and tool-loop errors in the command result as well.
    let mut events = agent.subscribe();
    agent.prompt(&task).await;
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

fn subagent_ui_event(event: AgentEvent) -> Option<AgentEvent> {
    match event {
        // Keep internal child prose out of the transcript: the labelled command
        // result below is the child’s final report. Reasoning remains visible.
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
        event => Some(event),
    }
}

struct SubagentResult {
    output: String,
    thinking: Vec<AgentMessage>,
}

fn format_subagent_results(
    tasks: Vec<WasiSubagentTask>,
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

    fn handler(capability: &'static str, work_dir: PathBuf) -> HostCapabilityHandler {
        let (event_tx, _) = broadcast::channel(4);
        HostCapabilityHandler {
            capability,
            tool_policy: None,
            extensions: Arc::new(WasiExtensionManager::new()),
            work_dir,
            event_tx,
            allowed_hosts: Arc::new(HashSet::new()),
        }
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
        let dispatcher = Arc::new(tokio::sync::Mutex::new(dispatcher));
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
