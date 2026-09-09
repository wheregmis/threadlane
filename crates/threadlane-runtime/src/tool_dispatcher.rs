//! Tool execution dispatcher.
//!
//! Owns the executor registry, hook pipeline, and parallel/sequential dispatch logic.
//! Independently testable.

use crate::error::AgentError;
use crate::events::AgentEvent;
use crate::harness::{HookContext, HookRegistry};
use crate::loop_engine::AbortOnDrop;
use crate::tool_executor::{builtin_tool_executor, ToolExecutor};
use crate::types::{
    AgentToolCall, AgentToolDefinition, AgentToolResult, ImageAttachment, ToolExecutionMode,
    ToolOutput,
};
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
    repetition: RepetitionCacheHandle,
    /// Replay re-executes safe tools for verification and must observe live
    /// state, never cached results.
    skip_repetition_cache: bool,
}

/// Deduplicates identical tool calls within one turn: the 37×-identical-read
/// failure mode. A global version counter invalidates every entry whenever a
/// potentially-mutating tool runs, so cached reads can never go stale.
/// Cloned dispatchers share one cache through the `Arc`.
#[derive(Clone, Default)]
struct RepetitionCacheHandle {
    inner: Arc<std::sync::Mutex<RepetitionCache>>,
}

#[derive(Default)]
struct RepetitionCache {
    version: u64,
    entries: std::collections::HashMap<(String, String), CachedToolResult>,
}

struct CachedToolResult {
    version: u64,
    content: String,
    is_error: bool,
    images: Vec<ImageAttachment>,
}

/// Tools pure enough to serve from cache: deterministic reads whose inputs
/// cannot change without a mutating tool running first (which bumps the
/// version). Everything else always executes. Live or mutating tools
/// (`browser_snapshot`, screenshots, `evaluate`, all writes/commands) stay
/// out even though that costs re-execution: correctness first.
const CACHEABLE_TOOLS: &[&str] = &[
    "read_file",
    "grep_search",
    "list_dir",
    "get_repo_map",
    "computer_status",
    "computer_windows",
    "browser_current_url",
    "load_skill",
];

const REPETITION_CACHE_CAP: usize = 64;

const REPETITION_NOTE: &str = "Repeated invocation: identical arguments already ran earlier this turn and produced this same result (served from cache, not re-executed). If you need different information, change the arguments or use another tool.";

impl RepetitionCacheHandle {
    /// Returns the cached content (with steering note), images, and the
    /// original error flag for an identical call in the current version.
    fn lookup(&self, name: &str, args: &str) -> Option<(ToolOutput, bool)> {
        if !CACHEABLE_TOOLS.contains(&name) {
            return None;
        }
        let guard = self.inner.lock().ok()?;
        let entry = guard
            .entries
            .get(&(name.to_string(), args.to_string()))?;
        (entry.version == guard.version).then(|| {
            (
                ToolOutput {
                    content: format!("{}\n\n[{REPETITION_NOTE}]", entry.content),
                    images: entry.images.clone(),
                },
                entry.is_error,
            )
        })
    }

    fn store(&self, name: &str, args: &str, output: &ToolOutput, is_error: bool) {
        if !CACHEABLE_TOOLS.contains(&name) {
            // Unknown or mutating tools invalidate everything cached.
            if let Ok(mut guard) = self.inner.lock() {
                guard.version = guard.version.wrapping_add(1);
            }
            return;
        }
        // Errors cache too: identical error loops are worth short-circuiting,
        // and any later mutation invalidates by version.
        if let Ok(mut guard) = self.inner.lock() {
            if guard.entries.len() >= REPETITION_CACHE_CAP {
                guard.entries.clear();
            }
            let version = guard.version;
            guard.entries.insert(
                (name.to_string(), args.to_string()),
                CachedToolResult {
                    version,
                    content: output.content.clone(),
                    is_error,
                    images: output.images.clone(),
                },
            );
        }
    }

    fn clear(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.entries.clear();
            guard.version = guard.version.wrapping_add(1);
        }
    }
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
    repetition: RepetitionCacheHandle,
}

const CORE_TOOL_NAMES: &[&str] = &[
    "read_file",
    "edit_file_hashline",
    "edit_files_hashline",
    "write_file",
    "run_command",
    "subagent",
    // Embedded browser panel (threadlane-session/src/browser.rs). The tools
    // report a helpful error when no panel is attached, so they are safe to
    // advertise unconditionally.
    "browser_navigate",
    "browser_back",
    "browser_reload",
    "browser_current_url",
    "browser_snapshot",
    "browser_act",
    "browser_evaluate_script",
    "browser_screenshot",
    "browser_console_logs",
    "browser_wait",
    // Native computer use (threadlane-session/src/computer.rs). Every
    // screenshot and input action re-prompts for approval, so the schemas
    // are safe to advertise; unattended sessions deny at execution.
    "computer_status",
    "computer_windows",
    "computer_screenshot",
    "computer_act",
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
            repetition: RepetitionCacheHandle::default(),
        }
    }

    /// Drops all cached repetition results and invalidates outstanding ones.
    /// Called once per turn so cached reads can never leak across turns.
    pub(crate) fn clear_repetition_cache(&self) {
        self.repetition.clear();
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
        self.execute_tools_with_options(tool_calls, self.tool_intent_recorder.clone(), false, false)
            .await
    }

    /// Executes tools without recording intents (e.g., replay).
    #[cfg(test)]
    async fn execute_tools_without_intent_recording(
        &self,
        tool_calls: &[ToolCall],
    ) -> Vec<AgentToolResult> {
        self.execute_tools_with_options(tool_calls, None, false, false)
            .await
    }

    /// Replays already-intended safe tools. The before hook is intentionally
    /// skipped: the durable ToolStarted record is the clearance boundary.
    /// The repetition cache is skipped as well: replay must observe live
    /// state for verification, never cached results.
    pub(crate) async fn execute_tools_for_replay(
        &self,
        tool_calls: &[ToolCall],
    ) -> Vec<AgentToolResult> {
        self.execute_tools_with_options(tool_calls, None, true, true)
            .await
    }

    async fn execute_tools_with_options(
        &self,
        tool_calls: &[ToolCall],
        intent_recorder: Option<ToolIntentRecorder>,
        skip_before_hook: bool,
        skip_repetition_cache: bool,
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
                        skip_repetition_cache,
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
                    repetition: self.repetition.clone(),
                    skip_repetition_cache,
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
                            images: Vec::new(),
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
        skip_repetition_cache: bool,
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
                repetition: self.repetition.clone(),
                skip_repetition_cache,
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
                    images: Vec::new(),
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
                    images: nested_result.images,
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
                images: Vec::new(),
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
                    images: Vec::new(),
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
                    images: Vec::new(),
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
                    images: Vec::new(),
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
        // Error flag for cache hits: the cached content already carries the
        // original error text, so it must not be re-prefixed below.
        let mut cached_is_error = false;
        if !context.skip_repetition_cache {
            if let Some((cached, was_error)) =
                context.repetition.lookup(&tc.function.name, &arguments)
            {
                execution_result = Some(Ok(cached));
                cached_is_error = was_error;
            }
        }
        for route in context.tool_routes {
            if execution_result.is_some() {
                break;
            }
            if !route.tool_names.contains(&tc.function.name) {
                continue;
            }
            if let Some(result) = route
                .executor
                .execute_tool_with_output_in_workspace(
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
        let (content, is_error, images) = match execution_result {
            Ok(output) => (output.content, cached_is_error, output.images),
            Err(error) => (
                format!("Tool executor error: {error}"),
                true,
                Vec::new(),
            ),
        };
        if !context.skip_repetition_cache && !cached_is_error {
            // Record fresh executions for identical-call dedup, including
            // fresh errors: identical error loops are worth short-circuiting,
            // and any later mutation invalidates by version. Cache hits never
            // re-store (that would nest steering notes).
            context.repetition.store(
                &tc.function.name,
                &arguments,
                &ToolOutput {
                    content: content.clone(),
                    images: images.clone(),
                },
                is_error,
            );
        }
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
            images,
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

    struct CountingExecutor {
        id: String,
        tools: Vec<AgentToolDefinition>,
        result: String,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for CountingExecutor {
        fn executor_id(&self) -> &str {
            &self.id
        }

        fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
            self.tools.clone().into()
        }

        async fn execute_tool(&self, _name: &str, _args: &str) -> Option<Result<String, String>> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(Ok(self.result.clone()))
        }
    }

    fn counting_dispatcher(
        tools: &[(&str, &str)],
    ) -> (ToolDispatcher, std::collections::HashMap<String, Arc<std::sync::atomic::AtomicUsize>>) {
        let (event_tx, _) = broadcast::channel(8);
        let mut dispatcher = ToolDispatcher::new(event_tx, HookRegistry::default());
        let mut counters = std::collections::HashMap::new();
        for (index, (name, result)) in tools.iter().enumerate() {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            counters.insert(name.to_string(), calls.clone());
            dispatcher
                .register_tool_executor(Arc::new(CountingExecutor {
                    id: format!("stub-{index}"),
                    tools: vec![stub_tool(name)],
                    result: result.to_string(),
                    calls,
                }))
                .expect("register stub");
        }
        (dispatcher, counters)
    }

    fn tool_call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            r#type: "function".into(),
            function: threadlane_protocol::RuntimeToolCallFunction {
                name: name.into(),
                arguments: args.into(),
            },
            thought_signature: None,
        }
    }

    fn call_count(
        counters: &std::collections::HashMap<String, Arc<std::sync::atomic::AtomicUsize>>,
        name: &str,
    ) -> usize {
        counters[name].load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn repetition_cache_serves_identical_reads_once() {
        // computer_windows is cacheable and unclaimed by the builtin
        // executor, so the stub owns its schema without conflicts.
        let (dispatcher, counters) =
            counting_dispatcher(&[("computer_windows", "win1"), ("browser_act", "ok")]);
        let call = tool_call("call-1", "computer_windows", "{}");
        let first = dispatcher.execute_tools(&[call.clone()]).await;
        let second = dispatcher.execute_tools(&[call]).await;
        assert_eq!(call_count(&counters, "computer_windows"), 1);
        assert_eq!(first[0].content, "win1");
        assert!(
            second[0].content.contains("served from cache"),
            "cache hit must steer the model: {}",
            second[0].content
        );
        assert!(!second[0].is_error);
    }

    #[tokio::test]
    async fn mutation_busts_the_repetition_cache() {
        let (dispatcher, counters) =
            counting_dispatcher(&[("computer_windows", "win1"), ("browser_act", "ok")]);
        let read = tool_call("call-1", "computer_windows", "{}");
        let write = tool_call("call-2", "browser_act", "{}");
        dispatcher.execute_tools(&[read.clone()]).await;
        dispatcher.execute_tools(&[write]).await;
        dispatcher.execute_tools(&[read]).await;
        assert_eq!(call_count(&counters, "computer_windows"), 2);
        assert_eq!(call_count(&counters, "browser_act"), 1);
    }

    #[tokio::test]
    async fn mutating_tools_always_execute() {
        let (dispatcher, counters) = counting_dispatcher(&[("browser_act", "ok")]);
        let call = tool_call("call-1", "browser_act", "{}");
        dispatcher.execute_tools(&[call.clone()]).await;
        dispatcher.execute_tools(&[call]).await;
        assert_eq!(call_count(&counters, "browser_act"), 2);
    }

    #[tokio::test]
    async fn clearing_resets_the_repetition_cache() {
        let (dispatcher, counters) = counting_dispatcher(&[("computer_windows", "win1")]);
        let call = tool_call("call-1", "computer_windows", "{}");
        dispatcher.execute_tools(&[call.clone()]).await;
        dispatcher.clear_repetition_cache();
        dispatcher.execute_tools(&[call]).await;
        assert_eq!(call_count(&counters, "computer_windows"), 2);
    }

    #[tokio::test]
    async fn replay_skips_the_repetition_cache() {
        let (dispatcher, counters) = counting_dispatcher(&[("computer_windows", "win1")]);
        let call = tool_call("call-1", "computer_windows", "{}");
        dispatcher.execute_tools(&[call.clone()]).await;
        dispatcher.execute_tools_for_replay(&[call]).await;
        assert_eq!(call_count(&counters, "computer_windows"), 2);
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
