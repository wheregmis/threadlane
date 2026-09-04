//! Tool execution dispatcher.
//!
//! Owns the executor registry, hook pipeline, and parallel/sequential dispatch logic.
//! Independently testable.

use crate::error::AgentError;
use crate::events::AgentEvent;
use crate::harness::{HookContext, HookRegistry};
use crate::loop_engine::AbortOnDrop;
use crate::tool_executor::{builtin_tool_executor, ToolExecutor};
use crate::types::{AgentToolCall, AgentToolDefinition, AgentToolResult, ToolExecutionMode};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use threadlane_protocol::RuntimeToolCall as ToolCall;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use futures::FutureExt;

/// Callback invoked after a tool intent is recorded and before execution.
fn dyn_tool_call(run_command_arguments: &str) -> Option<(String, String)> {
    let arguments: Value = serde_json::from_str(run_command_arguments).ok()?;
    let command = arguments.get("command")?.as_str()?.trim();
    let input = command.strip_prefix("dyn ")?.trim();
    let mut parts = input.split_whitespace();
    let tool_name = parts.next()?;
    let remaining = input[tool_name.len()..].trim();
    if tool_name.starts_with('-') || remaining == "--help" || remaining == "-h" {
        return None;
    }
    let tool_arguments = if remaining.is_empty() {
        "{}".to_owned()
    } else if remaining.starts_with('{')
        && serde_json::from_str::<Value>(remaining)
            .ok()
            .is_some_and(|value| value.is_object())
    {
        remaining.to_owned()
    } else {
        return None;
    };
    Some((tool_name.to_owned(), tool_arguments))
}
pub type ToolIntentRecorder = crate::provider::ToolIntentRecorder;
/// Callback invoked after tool execution completes.
pub type ToolCompletionRecorder = crate::provider::ToolCompletionRecorder;

#[derive(Clone)]
struct ToolExecutorRoute {
    executor: Arc<dyn ToolExecutor>,
    tool_names: HashSet<String>,
}

struct ToolRunContext {
    hooks: HookRegistry,
    intent_recorder: Option<ToolIntentRecorder>,
    execution_trace_recorder: Option<crate::provider::ToolExecutionTraceRecorder>,
    event_tx: broadcast::Sender<AgentEvent>,
    tool_routes: Vec<ToolExecutorRoute>,
    allowed_tool_names: Option<HashSet<String>>,
    work_dir: Option<PathBuf>,
    skip_before_hook: bool,
    session_id: String,
}

struct PreparedToolCall {
    tc: ToolCall,
    arguments: String,
    agent_tool_call: AgentToolCall,
    context: ToolRunContext,
}

/// Owns the tool executor registry, hook pipeline, and dispatch logic.
///
/// All tool execution methods use `&self` (they clone shared state internally),
/// so the dispatcher can be shared behind an `Arc`.
#[derive(Clone)]
pub struct ToolDispatcher {
    pub(crate) tool_execution_mode: ToolExecutionMode,
    hook_registry: HookRegistry,
    pub tool_intent_recorder: Option<ToolIntentRecorder>,
    pub tool_completion_recorder: Option<ToolCompletionRecorder>,
    pub tool_execution_trace_recorder: Option<crate::provider::ToolExecutionTraceRecorder>,
    pub(crate) allowed_tool_names: Option<HashSet<String>>,
    pub(crate) core_tool_schema_mode: bool,
    pub(crate) work_dir: Option<PathBuf>,
    pub(crate) session_id: String,

    tool_executors: Vec<Arc<dyn ToolExecutor>>,
    event_tx: broadcast::Sender<AgentEvent>,
}

const CORE_TOOL_NAMES: &[&str] = &[
    "read_file",
    "edit_file_hashline",
    "edit_files_hashline",
    "write_file",
    "run_command",
    "subagent",
];

impl ToolDispatcher {
    /// Creates a dispatcher backed by the given event channel and hook registry.
    pub(crate) fn new(event_tx: broadcast::Sender<AgentEvent>, hooks: HookRegistry) -> Self {
        Self {
            tool_execution_mode: ToolExecutionMode::Parallel,
            hook_registry: hooks,
            tool_intent_recorder: None,
            tool_completion_recorder: None,
            tool_execution_trace_recorder: None,
            allowed_tool_names: None,
            core_tool_schema_mode: true,
            work_dir: None,
            session_id: String::new(),
            tool_executors: vec![builtin_tool_executor()],
            event_tx,
        }
    }

    // ── Executor registry ─────────────────────────────────────────────

    /// Returns the core and registered executor schemas in provider order,
    /// after conflict deduplication and the active allowlist are applied.
    pub(crate) fn configured_tool_definitions(&self) -> Vec<AgentToolDefinition> {
        let mut definitions = collect_tool_definitions(&self.tool_executors);
        if let Some(allowed) = &self.allowed_tool_names {
            definitions.retain(|d| allowed.contains(&d.name));
        }
        if self.core_tool_schema_mode {
            definitions.retain(|d| CORE_TOOL_NAMES.contains(&d.name.as_str()));
        }
        definitions
    }

    pub(crate) fn register_tool_executor(
        &mut self,
        executor: Arc<dyn ToolExecutor>,
    ) -> Result<(), AgentError> {
        let executor_id = executor.executor_id().trim();
        if executor_id.is_empty() {
            return Err(AgentError::ToolRegistration(
                "Tool executor id must not be empty".into(),
            ));
        }
        if self
            .ordered_tool_executors()
            .iter()
            .any(|registered| registered.executor_id() == executor_id)
        {
            return Err(AgentError::ToolRegistration(format!(
                "Tool executor '{executor_id}' is already registered"
            )));
        }

        let mut known_names = HashSet::new();
        for registered in self.ordered_tool_executors() {
            known_names.extend(
                registered
                    .tool_definitions()
                    .iter()
                    .map(|definition| definition.name.clone()),
            );
        }
        for definition in executor.tool_definitions().iter() {
            if definition.name.trim().is_empty() {
                return Err(AgentError::ToolRegistration(format!(
                    "Tool executor '{executor_id}' provided an empty tool name"
                )));
            }
            if !known_names.insert(definition.name.clone()) {
                return Err(AgentError::ToolRegistration(format!(
                    "Tool schema '{}' from executor '{executor_id}' conflicts with an existing schema",
                    definition.name
                )));
            }
        }

        self.tool_executors.push(executor);
        Ok(())
    }

    pub(crate) fn tool_executor_count(&self) -> usize {
        self.ordered_tool_executors().len()
    }

    fn ordered_tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        self.tool_executors.clone()
    }

    // ── Tool execution ────────────────────────────────────────────────

    /// Executes tools and returns results. Intents are recorded before execution.
    pub(crate) async fn execute_tools(&self, tool_calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.execute_tools_with_options(tool_calls, self.tool_intent_recorder.clone(), false)
            .await
    }

    /// Executes tools without recording intents (e.g., replay).
    #[cfg(test)]
    async fn execute_tools_without_intent_recording(
        &self,
        tool_calls: &[ToolCall],
    ) -> Vec<AgentToolResult> {
        self.execute_tools_with_options(tool_calls, None, false)
            .await
    }

    /// Replays already-intended safe tools. The before hook is intentionally
    /// skipped: the durable ToolStarted record is the clearance boundary.
    pub(crate) async fn execute_tools_for_replay(
        &self,
        tool_calls: &[ToolCall],
    ) -> Vec<AgentToolResult> {
        self.execute_tools_with_options(tool_calls, None, true)
            .await
    }

    async fn execute_tools_with_options(
        &self,
        tool_calls: &[ToolCall],
        intent_recorder: Option<ToolIntentRecorder>,
        skip_before_hook: bool,
    ) -> Vec<AgentToolResult> {
        let mut results = Vec::new();
        let tool_routes = self.tool_execution_routes().await;
        let allowed_tool_names = self.allowed_tool_names.clone();

        if self.tool_execution_mode == ToolExecutionMode::Sequential {
            for tc in tool_calls {
                let res = self
                    .execute_single_tool(
                        tc,
                        tool_routes.clone(),
                        allowed_tool_names.clone(),
                        intent_recorder.clone(),
                        skip_before_hook,
                    )
                    .await;
                results.push(res);
            }
        } else {
            let mut slots: Vec<Option<AgentToolResult>> = vec![None; tool_calls.len()];
            let mut prepared = Vec::new();
            for (index, tc) in tool_calls.iter().enumerate() {
                let context = ToolRunContext {
                    hooks: self.hook_registry.clone(),
                    intent_recorder: intent_recorder.clone(),
                    execution_trace_recorder: self.tool_execution_trace_recorder.clone(),
                    event_tx: self.event_tx.clone(),
                    tool_routes: tool_routes.clone(),
                    allowed_tool_names: allowed_tool_names.clone(),
                    work_dir: self.work_dir.clone(),
                    skip_before_hook,
                    session_id: self.session_id.clone(),
                };
                match Self::prepare_tool_call(tc.clone(), context).await {
                    Ok(call) => prepared.push((index, call)),
                    Err(result) => slots[index] = Some(result),
                }
            }

            let mut handles = Vec::new();
            let mut executed_indices = Vec::new();
            for (index, call) in prepared {
                let fallback_call = call.tc.clone();
                let handle = AbortOnDrop::new(tokio::spawn(async move {
                    Self::execute_prepared_tool(call).await
                }));
                handles.push((index, fallback_call, handle));
                executed_indices.push(index);
            }

            for (index, tool_call, handle) in handles {
                match handle.join().await {
                    Ok(result) => slots[index] = Some(result),
                    Err(error) => {
                        let result = AgentToolResult {
                            tool_call_id: tool_call.id.clone(),
                            name: tool_call.function.name.clone(),
                            content: format!("Tool execution task failed: {error}"),
                            is_error: true,
                            terminate: false,
                        };
                        slots[index] = Some(result);
                    }
                }
            }
            if let Some(recorder) = &self.tool_completion_recorder {
                for &index in &executed_indices {
                    let Some(result) = slots[index].as_mut() else {
                        continue;
                    };
                    if let Err(error) = recorder(result).await {
                        result.content = error;
                        result.is_error = true;
                    }
                }
            }
            for index in executed_indices {
                if let Some(result) = &slots[index] {
                    let _ = self.event_tx.send(AgentEvent::ToolExecutionEnd {
                        tool_call_id: result.tool_call_id.clone(),
                        name: result.name.clone(),
                        result: result.clone(),
                    });
                }
            }
            results.extend(slots.into_iter().flatten());
        }

        results
    }

    async fn execute_single_tool(
        &self,
        tc: &ToolCall,
        tool_routes: Vec<ToolExecutorRoute>,
        allowed_tool_names: Option<HashSet<String>>,
        intent_recorder: Option<ToolIntentRecorder>,
        skip_before_hook: bool,
    ) -> AgentToolResult {
        let result = AssertUnwindSafe(Self::run_tool_with_hooks(
            tc.clone(),
            ToolRunContext {
                hooks: self.hook_registry.clone(),
                intent_recorder,
                execution_trace_recorder: self.tool_execution_trace_recorder.clone(),
                event_tx: self.event_tx.clone(),
                tool_routes,
                allowed_tool_names,
                work_dir: self.work_dir.clone(),
                skip_before_hook,
                session_id: self.session_id.clone(),
            },
        ))
        .catch_unwind()
        .await;

        match result {
            Ok(mut result) => {
                if let Some(recorder) = &self.tool_completion_recorder {
                    if let Err(error) = recorder(&result).await {
                        result.content = error;
                        result.is_error = true;
                    }
                }
                let _ = self.event_tx.send(AgentEvent::ToolExecutionEnd {
                    tool_call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    result: result.clone(),
                });
                result
            }
            Err(_) => {
                let result = AgentToolResult {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    content: format!(
                        "Tool '{}' failed: the tool panicked during execution. \
                         Please retry the tool or use another approach.",
                        tc.function.name
                    ),
                    is_error: true,
                    terminate: false,
                };
                let _ = self.event_tx.send(AgentEvent::ToolExecutionEnd {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    result: result.clone(),
                });
                result
            }
        }
    }

    async fn run_tool_with_hooks(tc: ToolCall, context: ToolRunContext) -> AgentToolResult {
        if tc.function.name == "run_command" {
            if let Some((tool_name, tool_arguments)) = dyn_tool_call(&tc.function.arguments) {
                let nested = ToolCall {
                    id: format!("{}:dyn", tc.id),
                    r#type: "function".into(),
                    function: threadlane_protocol::RuntimeToolCallFunction {
                        name: tool_name,
                        arguments: tool_arguments,
                    },
                    thought_signature: None,
                };
                let nested_result = Box::pin(Self::run_tool_with_hooks(nested, context)).await;
                return AgentToolResult {
                    tool_call_id: tc.id,
                    name: tc.function.name,
                    content: if nested_result.is_error {
                        nested_result.content
                    } else {
                        format!(
                            "Exit Status: exit status: 0\n--- STDOUT ---\n{}\n--- STDERR ---",
                            nested_result.content
                        )
                    },
                    is_error: nested_result.is_error,
                    terminate: nested_result.terminate,
                };
            }
        }
        match Self::prepare_tool_call(tc, context).await {
            Ok(call) => Self::execute_prepared_tool(call).await,
            Err(result) => result,
        }
    }

    async fn prepare_tool_call(
        tc: ToolCall,
        context: ToolRunContext,
    ) -> Result<PreparedToolCall, AgentToolResult> {
        let arguments = normalize_tool_arguments(
            &tc.function.name,
            &tc.function.arguments,
            context.work_dir.as_deref(),
        );
        let agent_tool_call = AgentToolCall {
            id: tc.id.clone(),
            name: tc.function.name.clone(),
            arguments: arguments.clone(),
        };

        if context
            .allowed_tool_names
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&tc.function.name))
        {
            let result = AgentToolResult {
                tool_call_id: tc.id.clone(),
                name: tc.function.name.clone(),
                content: format!(
                    "Tool '{}' is not allowed by the current agent policy",
                    tc.function.name
                ),
                is_error: true,
                terminate: false,
            };
            let _ = context.event_tx.send(AgentEvent::ToolExecutionEnd {
                tool_call_id: tc.id,
                name: tc.function.name,
                result: result.clone(),
            });
            return Err(result);
        }

        if !context.skip_before_hook {
            let hook_ctx = HookContext {
                session_id: context.session_id.clone(),
                lane: "main".into(),
                run_id: None,
                resume_data: None,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.function.name.clone()),
                tool_arguments: Some(arguments.clone()),
                tool_result_content: None,
                tool_result_is_error: None,
            };
            if let Err(failures) = context.hooks.run_before_tool(&hook_ctx).await {
                let reason = failures
                    .into_iter()
                    .map(|f| format!("{}: {}", f.id, f.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                let res = AgentToolResult {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    content: reason,
                    is_error: true,
                    terminate: false,
                };
                let _ = context.event_tx.send(AgentEvent::ToolExecutionEnd {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    result: res.clone(),
                });
                return Err(res);
            }
        }

        if let Some(recorder) = &context.intent_recorder {
            if let Err(error) = recorder(&tc.id, &tc.function.name, &arguments).await {
                let result = AgentToolResult {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    content: error,
                    is_error: true,
                    terminate: false,
                };
                let _ = context.event_tx.send(AgentEvent::ToolExecutionEnd {
                    tool_call_id: tc.id,
                    name: tc.function.name,
                    result: result.clone(),
                });
                return Err(result);
            }
        }

        Ok(PreparedToolCall {
            tc,
            arguments,
            agent_tool_call,
            context,
        })
    }

    async fn execute_prepared_tool(call: PreparedToolCall) -> AgentToolResult {
        let PreparedToolCall {
            tc,
            arguments,
            agent_tool_call,
            context,
        } = call;
        let start_time = std::time::Instant::now();
        let started_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let executor_kind = context
            .tool_routes
            .iter()
            .find(|route| route.tool_names.contains(&tc.function.name))
            .map(|route| route.executor.executor_id().to_string())
            .unwrap_or_else(|| "unregistered".to_string());
        if let Some(recorder) = &context.execution_trace_recorder {
            if let Err(error) = recorder(crate::provider::ToolExecutionTraceEvent::Started {
                tool_call_id: tc.id.clone(),
                tool_name: tc.function.name.clone(),
                executor_kind: executor_kind.clone(),
                effective_arguments: arguments.clone(),
                started_at_ms,
            })
            .await
            {
                return AgentToolResult {
                    tool_call_id: tc.id,
                    name: tc.function.name,
                    content: format!("Failed to persist tool execution start: {error}"),
                    is_error: true,
                    terminate: false,
                };
            }
        }
        debug!(
            "Tool execution started: '{}' (call_id: {})",
            tc.function.name, tc.id
        );
        let _ = context.event_tx.send(AgentEvent::ToolExecutionStart {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            arguments: arguments.clone(),
        });

        let mut execution_result = None;
        for route in context.tool_routes {
            if !route.tool_names.contains(&tc.function.name) {
                continue;
            }
            if let Some(result) = route
                .executor
                .execute_tool_in_workspace(
                    &agent_tool_call.name,
                    &arguments,
                    context.work_dir.as_deref(),
                )
                .await
            {
                execution_result = Some(result);
                break;
            }
        }
        let execution_result = execution_result.unwrap_or_else(|| {
            Err(format!(
                "No registered executor handles tool '{}'. If this is an auxiliary capability, run it via: dyn {} [args]",
                tc.function.name, tc.function.name
            ))
        });
        let (content, is_error) = match execution_result {
            Ok(content) => (content, false),
            Err(error) => (format!("Tool executor error: {error}"), true),
        };
        let duration_ms = start_time.elapsed().as_millis();
        if is_error {
            warn!(
                "Tool execution failed: '{}' (call_id: {}) after {}ms: {}",
                tc.function.name, tc.id, duration_ms, content
            );
        } else {
            debug!(
                "Tool execution completed: '{}' (call_id: {}) in {}ms",
                tc.function.name, tc.id, duration_ms
            );
        }
        let mut final_result = AgentToolResult {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            content,
            is_error,
            terminate: false,
        };

        let hook_ctx = HookContext {
            session_id: context.session_id.clone(),
            lane: "main".into(),
            run_id: None,
            resume_data: None,
            tool_call_id: Some(tc.id.clone()),
            tool_name: Some(tc.function.name.clone()),
            tool_arguments: Some(arguments.clone()),
            tool_result_content: Some(final_result.content.clone()),
            tool_result_is_error: Some(final_result.is_error),
        };
        let hook_run = context.hooks.run_after_tool(&hook_ctx).await;
        for failure in hook_run.failures {
            warn!("after-tool hook {} failed: {}", failure.id, failure.message);
        }
        if let Some(content) = hook_run.effect.override_content {
            final_result.content = content;
        }
        if let Some(content) = hook_run.effect.append_content {
            if !content.trim().is_empty() {
                final_result.content.push_str("\n\n");
                final_result.content.push_str(&content);
            }
        }
        if let Some(is_error) = hook_run.effect.override_is_error {
            final_result.is_error = is_error;
        }
        if let Some(terminate) = hook_run.effect.terminate {
            final_result.terminate = terminate;
        }

        if let Some(recorder) = &context.execution_trace_recorder {
            if let Err(error) = recorder(crate::provider::ToolExecutionTraceEvent::Finished {
                tool_call_id: final_result.tool_call_id.clone(),
                tool_name: final_result.name.clone(),
                executor_kind: executor_kind.into(),
                started_at_ms,
                duration_ms: start_time.elapsed().as_millis() as u64,
                is_error: final_result.is_error,
                terminate: final_result.terminate,
                output_sha256: format!("{:x}", Sha256::digest(final_result.content.as_bytes())),
                output_bytes: final_result.content.len() as u64,
            })
            .await
            {
                final_result.content = format!("Failed to persist tool execution finish: {error}");
                final_result.is_error = true;
            }
        }

        final_result
    }

    async fn tool_execution_routes(&self) -> Vec<ToolExecutorRoute> {
        let mut claimed_names = HashSet::new();
        self.tool_executors
            .iter()
            .map(|executor| ToolExecutorRoute {
                executor: executor.clone(),
                tool_names: executor
                    .tool_definitions()
                    .iter()
                    .filter_map(|definition| {
                        (!definition.name.trim().is_empty()).then(|| definition.name.clone())
                    })
                    .filter(|name| claimed_names.insert(name.clone()))
                    .collect(),
            })
            .collect()
    }
}

// ── Free functions ───────────────────────────────────────────────────

fn collect_tool_definitions(
    registered_executors: &[Arc<dyn ToolExecutor>],
) -> Vec<AgentToolDefinition> {
    let mut seen = HashSet::new();
    let mut definitions = Vec::new();

    for executor in registered_executors {
        for definition in executor.tool_definitions().iter() {
            if seen.insert(definition.name.clone()) {
                definitions.push(definition.clone());
            }
        }
    }

    definitions
}

fn normalize_tool_arguments(
    name: &str,
    arguments: &str,
    work_dir: Option<&std::path::Path>,
) -> String {
    let Some(work_dir) = work_dir else {
        return arguments.to_string();
    };
    let Ok(mut value) = serde_json::from_str::<Value>(arguments) else {
        return arguments.to_string();
    };
    let workspace = work_dir.to_string_lossy().to_string();
    match (name, value.as_object_mut()) {
        ("read_file" | "write_file" | "edit_file" | "list_dir", Some(object))
            if object
                .get("path")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty) =>
        {
            object.insert("path".into(), Value::String(workspace));
        }
        ("run_command", Some(object))
            if object
                .get("cwd")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty) =>
        {
            object.insert("cwd".into(), Value::String(workspace));
        }
        _ => {}
    }

    serde_json::to_string(&value).unwrap_or_else(|_| arguments.to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HookKind;

    struct StubExecutor {
        id: String,
        tools: Vec<AgentToolDefinition>,
        result: Option<String>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for StubExecutor {
        fn executor_id(&self) -> &str {
            &self.id
        }

        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            self.tools.clone().into()
        }

        async fn execute_tool(&self, _name: &str, _args: &str) -> Option<Result<String, String>> {
            self.result.clone().map(Ok)
        }

        async fn execute_tool_with_call(
            &self,
            call: &AgentToolCall,
            _args: &str,
        ) -> Option<Result<String, String>> {
            // Match by name for the stub.
            if self.tools.iter().any(|d| d.name == call.name) {
                self.result.clone().map(Ok)
            } else {
                None
            }
        }
    }

    #[test]
    fn fills_missing_file_paths_from_the_workspace() {
        let arguments =
            normalize_tool_arguments("read_file", "{}", Some(std::path::Path::new("/workspace")));
        assert_eq!(arguments, r#"{"path":"/workspace"}"#);
    }

    fn stub_tool(name: &str) -> AgentToolDefinition {
        AgentToolDefinition::new(
            name,
            "",
            serde_json::json!({"type": "object", "properties": {}}),
        )
    }

    #[tokio::test]
    async fn dispatcher_executes_registered_tool() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        dispatcher
            .register_tool_executor(Arc::new(StubExecutor {
                id: "stub".into(),
                tools: vec![stub_tool("hello")],
                result: Some("world".into()),
            }))
            .unwrap();

        let results = dispatcher
            .execute_tools_without_intent_recording(&[ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "hello".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "world");
        assert!(!results[0].is_error);
    }

    #[tokio::test]
    async fn dispatcher_records_physical_execution_envelope_once() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        dispatcher
            .register_tool_executor(Arc::new(StubExecutor {
                id: "stub".into(),
                tools: vec![stub_tool("hello")],
                result: Some("world".into()),
            }))
            .unwrap();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder_observed = observed.clone();
        dispatcher.tool_execution_trace_recorder = Some(Arc::new(move |event| {
            let observed = recorder_observed.clone();
            Box::pin(async move {
                observed.lock().unwrap().push(event);
                Ok(())
            })
        }));

        let results = dispatcher
            .execute_tools_without_intent_recording(&[ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "hello".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert!(!results[0].is_error);
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 2);
        assert!(matches!(
            &observed[0],
            crate::provider::ToolExecutionTraceEvent::Started {
                tool_call_id,
                executor_kind,
                ..
            } if tool_call_id == "call_1" && executor_kind == "stub"
        ));
        assert!(matches!(
            &observed[1],
            crate::provider::ToolExecutionTraceEvent::Finished {
                tool_call_id,
                output_bytes: 5,
                output_sha256,
                ..
            } if tool_call_id == "call_1" && output_sha256.len() == 64
        ));
    }

    #[tokio::test]
    async fn builtins_are_registered_in_the_unified_executor_registry() {
        let (event_tx, _) = broadcast::channel(8);
        let dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());

        assert_eq!(dispatcher.tool_executor_count(), 1);
        assert!(dispatcher
            .configured_tool_definitions()
            .iter()
            .any(|definition| definition.name == "read_file"));
    }

    #[tokio::test]
    async fn dispatcher_rejects_duplicate_registration() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        let exec = Arc::new(StubExecutor {
            id: "dup".into(),
            tools: vec![stub_tool("a")],
            result: None,
        });
        dispatcher.register_tool_executor(exec.clone()).unwrap();
        assert!(dispatcher.register_tool_executor(exec).is_err());
    }

    #[tokio::test]
    async fn before_tool_hook_can_block_execution() {
        let (event_tx, _) = broadcast::channel(8);
        let hooks = HookRegistry::default();
        hooks
            .replace(
                HookKind::BeforeTool,
                "blocker",
                Arc::new(|_ctx| Box::pin(async move { Err("blocked by test".into()) })),
            )
            .unwrap();

        let mut dispatcher = ToolDispatcher::new(event_tx, hooks);
        dispatcher
            .register_tool_executor(Arc::new(StubExecutor {
                id: "stub".into(),
                tools: vec![stub_tool("stub_write")],
                result: Some("written".into()),
            }))
            .unwrap();

        let results = dispatcher
            .execute_tools_without_intent_recording(&[ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "stub_write".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(results[0].content.contains("blocked by test"));
    }

    #[tokio::test]
    async fn unknown_tool_is_rejected_without_a_registered_route() {
        let (event_tx, _) = broadcast::channel(8);
        let dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        let results = dispatcher
            .execute_tools_without_intent_recording(&[ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: threadlane_protocol::RuntimeToolCallFunction {
                    name: "nonexistent_tool_xyz".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(results[0]
            .content
            .contains("No registered executor handles tool"));
    }

    #[test]
    fn test_core_tool_schema_mode_filters_definitions() {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());

        // Default has core_tool_schema_mode: true
        assert!(dispatcher.core_tool_schema_mode);
        let defs = dispatcher.configured_tool_definitions();
        for def in &defs {
            assert!(
                CORE_TOOL_NAMES.contains(&def.name.as_str()),
                "tool '{}' should be in CORE_TOOL_NAMES",
                def.name
            );
        }
        // Auxiliary tools like list_dir or grep_search should be excluded from configured schemas
        assert!(!defs.iter().any(|d| d.name == "list_dir"));
        assert!(!defs.iter().any(|d| d.name == "grep_search"));
        assert!(!defs.iter().any(|d| d.name == "manage_memory"));

        // When core_tool_schema_mode is disabled, all registered tools appear
        dispatcher.core_tool_schema_mode = false;
        let all_defs = dispatcher.configured_tool_definitions();
        assert!(all_defs.iter().any(|d| d.name == "list_dir"));
        assert!(all_defs.iter().any(|d| d.name == "grep_search"));
    }
}
