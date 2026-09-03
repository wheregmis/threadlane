use super::broker::{
    HostCapabilityHandler, ManagedProcessRegistry, MAX_BROKER_CONTINUATION_ROUNDS,
};
use super::cancellation::AgentRunTask;
use super::scheduler::AgentWorkScheduler;
use super::subagents::{AgentRunner, MAX_SUBAGENT_TASKS};
use crate::agents::{discover_agents, AgentScope};
use crate::extension_broker::{
    BrokerError, CapabilityDispatcher, HostBrokerRequest, BROKER_API_VERSION,
};
use crate::permission::{PermissionHandle, PermissionManager};
use crate::plan::{SessionPlanStore, UpdatePlanToolExecutor};
use crate::policy::ToolPolicy;
use async_trait::async_trait;
use log::warn;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use threadlane_mcp::McpManager;
use threadlane_runtime::harness::{HookContext, HookEffect, HookHandler, HookKind};
use threadlane_runtime::Capability;
use threadlane_runtime::{AgentEvent, AgentToolCall, AgentToolDefinition, ToolExecutor};
use threadlane_skills::{LoadSkillToolExecutor, SkillRegistry};
use threadlane_wasi::WasiExtensionManager;
use tokio::sync::broadcast;

const SUBAGENT_TOOL_NAME: &str = "subagent";
pub(crate) const PREWALK_HANDOFF_TOOL_NAME: &str = "complete_prewalk";

// ── Capability implementations ─────────────────────────────────────────
// Each wraps a subsystem and implements [`crate::capability_registry::Capability`]
// so tools and hooks can be registered declaratively.

pub(crate) struct SkillCapability {
    pub(crate) skills: Arc<SkillRegistry>,
}
impl Capability for SkillCapability {
    fn id(&self) -> &str {
        "skills"
    }
    fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        vec![Arc::new(LoadSkillToolExecutor::new(self.skills.clone()))]
    }
}

pub(crate) struct SubagentCapability {
    pub(crate) agent_runner: AgentRunner,
}
impl Capability for SubagentCapability {
    fn id(&self) -> &str {
        "subagent"
    }
    fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        vec![Arc::new(SubagentToolExecutor::new(
            self.agent_runner.clone(),
        ))]
    }
}

pub(crate) struct PlanCapability {
    pub(crate) plan_store: SessionPlanStore,
    pub(crate) event_tx: broadcast::Sender<AgentEvent>,
}

pub(crate) struct PrewalkCapability;

impl Capability for PrewalkCapability {
    fn id(&self) -> &str {
        "prewalk"
    }

    fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        vec![Arc::new(PrewalkToolExecutor)]
    }
}

struct PrewalkToolExecutor;

#[async_trait]
impl ToolExecutor for PrewalkToolExecutor {
    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        vec![AgentToolDefinition {
            name: PREWALK_HANDOFF_TOOL_NAME.into(),
            description: Some(
                "Signal that the foundational prewalk change is implemented and verified.".into(),
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            strict: None,
        }]
        .into()
    }

    async fn execute_tool(&self, name: &str, _args: &str) -> Option<Result<String, String>> {
        (name == PREWALK_HANDOFF_TOOL_NAME).then(|| Ok("Prewalk handoff requested.".into()))
    }
}
impl Capability for PlanCapability {
    fn id(&self) -> &str {
        "plan"
    }
    fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        vec![Arc::new(UpdatePlanToolExecutor::new(
            self.plan_store.clone(),
            self.event_tx.clone(),
        ))]
    }
}

pub(crate) struct WasiCapability {
    pub(crate) extensions: Arc<WasiExtensionManager>,
    pub(crate) broker_dispatcher: Arc<CapabilityDispatcher>,
    pub(crate) tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
}
impl Capability for WasiCapability {
    fn id(&self) -> &str {
        "wasi"
    }
    fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        vec![Arc::new(BrokerAwareWasiToolExecutor {
            extensions: self.extensions.clone(),
            broker_dispatcher: self.broker_dispatcher.clone(),
        })]
    }
    fn hooks(&self) -> Vec<(HookKind, &str, HookHandler)> {
        vec![
            (
                HookKind::BeforeTool,
                "extension-before-tool",
                extension_before_tool_hook_handler(
                    self.tool_policy.clone(),
                    self.extensions.clone(),
                    self.broker_dispatcher.clone(),
                ),
            ),
            (
                HookKind::AfterTool,
                "extension-after-tool",
                create_after_tool_hook_handler(
                    self.extensions.clone(),
                    self.broker_dispatcher.clone(),
                ),
            ),
        ]
    }
}

pub(crate) struct McpCapability {
    pub(crate) mcp_manager: Arc<McpManager>,
}
impl Capability for McpCapability {
    fn id(&self) -> &str {
        "mcp"
    }
    fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        vec![self.mcp_manager.clone()]
    }
}

#[derive(Clone)]
pub struct SubagentToolExecutor {
    runner: AgentRunner,
}

impl SubagentToolExecutor {
    fn new(runner: AgentRunner) -> Self {
        Self { runner }
    }
}

fn subagent_tool_definition() -> AgentToolDefinition {
    AgentToolDefinition {
        name: SUBAGENT_TOOL_NAME.to_string(),
        description: Some(
            "Delegate one or more tasks to subagents in parallel or sequentially. Choose the role, task, instructions, and tools; project settings control child model and reasoning.".to_string(),
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "Ordered subagent tasks to run.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": {
                                "type": "string",
                                "description": "Subagent role or preset name (e.g. scout, worker, reviewer, planner, code_editor)."
                            },
                            "task": {
                                "type": "string",
                                "description": "Task description / prompt. In sequential mode, {previous} is replaced with the prior result."
                            },
                            "instructions": {
                                "type": "string",
                                "description": "Optional dynamic system instructions/prompt generated by the model for this subagent."
                            },
                            "tools": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional whitelist of tool names exposed to this subagent (e.g. ['read_file', 'edit_file_hashline'])."
                            },
                            "model": {
                                "type": "string",
                                "description": "Deprecated compatibility field. Accepted but ignored; project settings select the child model, falling back to the parent session model."
                            }
                        },
                        "required": ["agent", "task"]
                    }
                },
                "parallel": {
                    "type": "boolean",
                    "description": "Set to true to run tasks concurrently in parallel, false for a sequential chain."
                }
            },
            "required": ["tasks"]
        }),
        strict: None,
    }
}

#[cfg(test)]
mod subagent_definition_tests {
    use super::*;

    #[test]
    fn legacy_model_argument_is_explicitly_ignored() {
        let definition = subagent_tool_definition();
        let description = definition.parameters["properties"]["tasks"]["items"]["properties"]
            ["model"]["description"]
            .as_str()
            .unwrap();
        assert!(description.contains("Accepted but ignored"));
        assert!(description.contains("project settings"));
    }
}

impl SubagentToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.subagent"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        vec![subagent_tool_definition()].into()
    }

    async fn execute_tool_impl(
        &self,
        name: &str,
        args: &str,
        tool_call_id: Option<String>,
    ) -> Option<Result<String, String>> {
        if name != SUBAGENT_TOOL_NAME {
            return None;
        }

        let parsed: Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(err) => return Some(Err(format!("Invalid subagent tool arguments: {err}"))),
        };

        let tasks_val = match parsed.get("tasks").and_then(Value::as_array) {
            Some(arr) => arr,
            None => return Some(Err("Missing required argument `tasks`".into())),
        };
        if tasks_val.is_empty() {
            return Some(Err("`subagent` requires at least one task".into()));
        }
        if tasks_val.len() > MAX_SUBAGENT_TASKS {
            return Some(Err(format!(
                "`subagent` accepts at most {MAX_SUBAGENT_TASKS} tasks"
            )));
        }

        let mut tasks = Vec::new();
        for val in tasks_val {
            let agent = match val
                .get("agent")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(a) => a,
                None => return Some(Err("Each subagent task requires a non-empty `agent`".into())),
            };
            let task = match val
                .get("task")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(t) => t,
                None => return Some(Err("Each subagent task requires a non-empty `task`".into())),
            };
            let instructions = val
                .get("instructions")
                .or_else(|| val.get("system_prompt"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            let tools = val.get("tools").and_then(Value::as_array).map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            });
            let model = val
                .get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);

            tasks.push(AgentRunTask {
                agent: agent.to_string(),
                task: task.to_string(),
                instructions,
                tools,
                model,
            });
        }

        let parallel = parsed
            .get("parallel")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        match (self.runner)(tasks, parallel, tool_call_id).await {
            Ok(val) => {
                let msg = val
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Subagents completed successfully.");
                Some(Ok(msg.to_string()))
            }
            Err(err) => Some(Err(err)),
        }
    }
}

#[async_trait]
impl ToolExecutor for SubagentToolExecutor {
    fn executor_id(&self) -> &str {
        SubagentToolExecutor::executor_id(self)
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        SubagentToolExecutor::tool_definitions(self)
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        self.execute_tool_impl(name, args, None).await
    }

    async fn execute_tool_with_call(
        &self,
        call: &AgentToolCall,
        args: &str,
    ) -> Option<Result<String, String>> {
        self.execute_tool_impl(&call.name, args, Some(call.id.clone()))
            .await
    }
}

pub(crate) fn render_agent_catalog(work_dir: &Path) -> String {
    let mut agents = discover_agents(work_dir, AgentScope::Both).agents;
    agents.sort_by(|left, right| left.name.cmp(&right.name));
    agents.truncate(32);

    let mut catalog = String::from(
        "=== Subagent Task Execution ===\nYou have access to the native `subagent` tool to spin off subagents in parallel or sequentially. You can use preset agent roles or spin dynamic subagents on the fly by providing custom `instructions` (system prompt), a whitelist of `tools`, and an optional `model` override for each task.\n\nPrefer delegating subtasks (research, searching, edits, tests, code reviews) to subagents so work finishes faster in parallel.\n",
    );
    if !agents.is_empty() {
        catalog.push_str("\nAvailable Preset Agent Roles:\n");
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
    }
    catalog
}

pub(crate) fn restored_tool_policy(extensions: &WasiExtensionManager) -> ToolPolicy {
    match extensions
        .host_state("tools.policy")
        .and_then(|value| value.as_str().map(str::to_owned))
        .as_deref()
    {
        Some("read_only") => ToolPolicy::ReadOnly,
        _ => ToolPolicy::FullAccess,
    }
}

pub(crate) fn build_broker_dispatcher(
    tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    extensions: Arc<WasiExtensionManager>,
    persist_tool_policy: bool,
    work_dir: PathBuf,
    event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    agent_work: AgentWorkScheduler,
    agent_runner: Option<AgentRunner>,
    session_file: Option<PathBuf>,
) -> (
    Arc<CapabilityDispatcher>,
    ManagedProcessRegistry,
    PermissionHandle,
) {
    let allowed_hosts: Arc<HashSet<String>> = Arc::new(
        std::env::var("THREADLANE_NETWORK_ALLOW_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_ascii_lowercase)
            .collect(),
    );
    let permissions = Arc::new(PermissionManager::new(work_dir.clone(), event_tx.clone()));
    let permission_handle = permissions.handle();
    let mut dispatcher = CapabilityDispatcher::new();
    let managed_processes = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
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
                permissions: Some(permissions.clone()),
                agent_work: agent_work.clone(),
                agent_runner: agent_runner.clone(),
                session_file: session_file.clone(),
                persist_tool_policy,
                managed_processes: managed_processes.clone(),
            }),
        );
    }
    (Arc::new(dispatcher), managed_processes, permission_handle)
}

pub(crate) async fn dispatch_hook_requests(
    dispatcher: &Arc<CapabilityDispatcher>,
    extensions: &WasiExtensionManager,
    requests: Vec<HostBrokerRequest>,
) -> Result<(), BrokerError> {
    for request in requests {
        let dispatch = dispatcher.dispatch_envelopes(vec![request]).await?;
        extensions.enqueue_broker_results(dispatch.operation_results);
    }
    Ok(())
}

pub(crate) async fn dispatch_hook_requests_isolated(
    dispatcher: &Arc<CapabilityDispatcher>,
    extensions: &WasiExtensionManager,
    requests: Vec<HostBrokerRequest>,
    label: &str,
) {
    for request in requests {
        if let Err(error) = dispatch_hook_requests(dispatcher, extensions, vec![request]).await {
            warn!("{label}: {}", error.message);
        }
    }
}

pub fn extension_before_tool_hook_handler(
    tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    extensions: Arc<WasiExtensionManager>,
    broker_dispatcher: Arc<CapabilityDispatcher>,
) -> HookHandler {
    Arc::new(move |context: HookContext| {
        let tool_policy = tool_policy.clone();
        let extensions = extensions.clone();
        let broker_dispatcher = broker_dispatcher.clone();
        Box::pin(async move {
            let policy = *tool_policy.lock().await;
            let tool_name = context.tool_name.as_deref().unwrap_or("");
            if policy == ToolPolicy::ReadOnly
                && matches!(
                    tool_name,
                    "write_file"
                        | "edit_file"
                        | "edit_file_hashline"
                        | "edit_files_hashline"
                        | "apply_workspace_edit_plan"
                        | "write"
                        | "edit"
                        | "run_command"
                )
            {
                return Err(format!(
                    "Tool `{tool_name}` is blocked because read-only tool policy is ACTIVE."
                ));
            }

            let arguments = serde_json::json!({
                "tool_name": tool_name,
                "tool_arguments": context.tool_arguments.as_deref().unwrap_or(""),
            });
            let hook_responses = extensions
                .execute_hook_with_broker_requests("before_tool_call", &arguments.to_string());
            for resp in hook_responses {
                let res = match resp {
                    Ok(res) => res,
                    Err(error) => {
                        return Err(format!("Extension hook error: {error}"));
                    }
                };
                if let Err(error) = dispatch_hook_requests(
                    &broker_dispatcher,
                    &extensions,
                    res.host_broker_requests,
                )
                .await
                {
                    return Err(format!("Extension broker error: {}", error.message));
                }
                let api_version = res.api_version;
                let response = res.response;
                if api_version == BROKER_API_VERSION {
                    if let Some(middleware) = response.middleware {
                        if middleware.block == Some(true) {
                            return Err(middleware.reason.unwrap_or_else(|| "blocked".into()));
                        }
                    }
                } else if api_version == 1 {
                    if let Some(msg) = response.message {
                        if msg.contains("blocked") {
                            return Err(msg);
                        }
                    }
                }
            }

            Ok(HookEffect::default())
        })
    })
}

pub(crate) fn create_after_tool_hook_handler(
    extensions: Arc<WasiExtensionManager>,
    broker_dispatcher: Arc<CapabilityDispatcher>,
) -> HookHandler {
    Arc::new(move |context: HookContext| {
        let extensions = extensions.clone();
        let broker_dispatcher = broker_dispatcher.clone();
        Box::pin(async move {
            let arguments = serde_json::json!({
                "tool_name": context.tool_name.as_deref().unwrap_or(""),
                "tool_arguments": context.tool_arguments.as_deref().unwrap_or(""),
                "result": context.tool_result_content.as_deref().unwrap_or(""),
                "is_error": context.tool_result_is_error.unwrap_or(false),
            });
            // Tool requests are queued by ToolExecutor; dispatch them first so the
            // tool's effects precede the deterministic, name-sorted after hooks.
            dispatch_hook_requests_isolated(
                &broker_dispatcher,
                &extensions,
                extensions.take_pending_broker_requests(),
                "WASI tool broker error",
            )
            .await;
            let mut effect = HookEffect::default();
            let tool_name = context.tool_name.as_deref().unwrap_or("");
            let is_successful_rust_write = !context.tool_result_is_error.unwrap_or(false)
                && matches!(
                    tool_name,
                    "write_file"
                        | "edit_file_hashline"
                        | "edit_files_hashline"
                        | "apply_workspace_edit_plan"
                )
                && serde_json::from_str::<Value>(context.tool_arguments.as_deref().unwrap_or("{}"))
                    .ok()
                    .and_then(|value| value.get("path").and_then(Value::as_str).map(str::to_owned))
                    .is_some_and(|path| path.ends_with(".rs"));
            if is_successful_rust_write {
                let path = serde_json::from_str::<Value>(
                    context.tool_arguments.as_deref().unwrap_or("{}"),
                )
                .ok()
                .and_then(|value| value.get("path").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
                if let Some(result) =
                    run_lsp_diagnostics_after_write(&extensions, &broker_dispatcher, &path).await
                {
                    match result {
                        Ok(diagnostics) => {
                            effect.append_content =
                                Some(format!("[LSP Diagnostics]\n{diagnostics}"))
                        }
                        Err(error) => warn!("post-write lsp diagnostics failed: {error}"),
                    }
                }
            }
            for response in extensions
                .execute_hook_with_broker_requests("after_tool_call", &arguments.to_string())
            {
                match response {
                    Ok(response) => {
                        match broker_dispatcher
                            .dispatch_envelopes(response.host_broker_requests)
                            .await
                        {
                            Ok(dispatch) => {
                                extensions.enqueue_broker_results(dispatch.operation_results);
                            }
                            Err(error) => {
                                warn!("WASI after-tool hook broker error: {}", error.message)
                            }
                        }
                    }
                    Err(error) => warn!("WASI after-tool hook error: {error}"),
                }
            }
            Ok(effect)
        })
    })
}

pub(crate) async fn run_lsp_diagnostics_after_write(
    extensions: &WasiExtensionManager,
    broker_dispatcher: &Arc<CapabilityDispatcher>,
    path: &str,
) -> Option<Result<String, String>> {
    let args = serde_json::json!({ "path": path }).to_string();
    let mut continuation_rounds = 0;
    loop {
        let invocation = extensions.execute_tool_with_broker_requests("lsp_diagnostics", &args)?;
        let invocation = match invocation {
            Ok(invocation) => invocation,
            Err(error) => return Some(Err(error)),
        };
        if let Some(error) = invocation.response.error {
            return Some(Err(error));
        }
        let continue_after_broker = invocation.response.continue_after_broker;
        let message = invocation.response.message.unwrap_or_default();
        if invocation.host_broker_requests.is_empty() {
            return Some(Ok(message));
        }
        if continuation_rounds >= MAX_BROKER_CONTINUATION_ROUNDS {
            return Some(Err(
                "lsp_diagnostics exceeded broker continuation limit".into()
            ));
        }
        let dispatch = match broker_dispatcher
            .dispatch_envelopes(invocation.host_broker_requests)
            .await
        {
            Ok(dispatch) => dispatch,
            Err(error) => return Some(Err(error.message)),
        };
        extensions.enqueue_broker_results(dispatch.operation_results);
        if !continue_after_broker {
            return Some(Ok(message));
        }
        continuation_rounds += 1;
    }
}

pub(crate) struct BrokerAwareWasiToolExecutor {
    pub(crate) extensions: Arc<WasiExtensionManager>,
    pub(crate) broker_dispatcher: Arc<CapabilityDispatcher>,
}

#[async_trait]
impl ToolExecutor for BrokerAwareWasiToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.wasi_broker_tools"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        self.extensions.tool_definitions()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        let mut continuation_rounds = 0;
        loop {
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
            let continue_after_broker = invocation.response.continue_after_broker;
            let immediate_message = invocation.response.message.unwrap_or_default();
            let requests = invocation.host_broker_requests;
            if requests.is_empty() {
                return Some(Ok(immediate_message));
            }
            if continue_after_broker && continuation_rounds >= MAX_BROKER_CONTINUATION_ROUNDS {
                return Some(Err(format!(
                    "WASI tool `{name}` exceeded the broker continuation limit of \
                     {MAX_BROKER_CONTINUATION_ROUNDS} rounds; clear `continue_after_broker` after \
                     processing `broker_response` events"
                )));
            }

            let dispatch = match self.broker_dispatcher.dispatch_envelopes(requests).await {
                Ok(dispatch) => dispatch,
                Err(error) => return Some(Err(error.message)),
            };
            let operation_results = dispatch.operation_results;
            self.extensions
                .enqueue_broker_results(operation_results.clone());

            if continue_after_broker {
                continuation_rounds += 1;
                continue;
            }

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
            return Some(Ok(broker_message.unwrap_or(immediate_message)));
        }
    }
}
