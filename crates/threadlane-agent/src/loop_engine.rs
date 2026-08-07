use crate::compaction::{
    compact_messages_to_token_budget, compaction_summary_text, is_context_overflow_error,
    should_auto_compact, AUTO_COMPACTION_KEEP_RECENT_TOKENS,
};
use crate::events::AgentEvent;
use crate::hooks::{
    AfterToolCallHook, BeforeToolCallHook, ShouldStopAfterTurnHook, ToolExecutor,
    TransformContextHook,
};
use crate::queue::PendingMessageQueue;
use crate::types::{
    AgentMessage, AgentState, AgentToolCall, AgentToolDefinition, AgentToolResult, QueueMode,
    TokenUsage, ToolExecutionMode,
};
use futures::FutureExt;
use serde_json::Value;
use std::collections::HashSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use threadlane_provider::openai::{clamp_prompt_cache_key, ProviderUsage, StreamEvent, ToolCall};
use threadlane_provider::router::{PayloadFormat, PayloadSource, ProviderClient};
use threadlane_tools::{
    execute_tool, execute_tool_in_workspace, get_available_tools, get_codex_tools,
};
use tokio::sync::{broadcast, mpsc, Mutex};

struct AbortOnDrop<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        let result = self.handle.as_mut().expect("task handle missing").await;
        self.handle = None;
        result
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

fn normalized_tool_call_id(id: &str, empty_index: usize) -> String {
    if id.is_empty() {
        format!("call_{empty_index}")
    } else {
        id.to_string()
    }
}

/// Removes an assistant tool-call turn that was interrupted before every call
/// received a tool result. Provider APIs reject replaying such incomplete turns.
pub fn repair_interrupted_tool_turn(messages: &mut Vec<AgentMessage>) -> bool {
    let mut index = 0;
    while index < messages.len() {
        let AgentMessage::Assistant {
            tool_calls: Some(tool_calls),
            ..
        } = &messages[index]
        else {
            index += 1;
            continue;
        };
        if tool_calls.is_empty() {
            index += 1;
            continue;
        }

        let expected_ids: HashSet<String> = tool_calls
            .iter()
            .enumerate()
            .map(|(idx, call)| normalized_tool_call_id(&call.id, idx))
            .collect();
        let mut completed_ids = HashSet::new();
        let mut next = index + 1;
        let mut tool_index = 0;
        while let Some(AgentMessage::Tool { tool_call_id, .. }) = messages.get(next) {
            let id = normalized_tool_call_id(tool_call_id, tool_index);
            tool_index += 1;
            completed_ids.insert(id);
            next += 1;
        }

        if expected_ids.is_subset(&completed_ids) {
            index = next;
            continue;
        }

        let truncate_at = index.checked_sub(1).filter(|previous| {
            matches!(
                &messages[*previous],
                AgentMessage::Custom { custom_type, .. } if custom_type == "thinking"
            )
        });
        messages.truncate(truncate_at.unwrap_or(index));
        return true;
    }
    false
}

fn token_usage_from_provider(usage: ProviderUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        total_tokens: usage.total_tokens,
    }
}

#[derive(Debug, Clone)]
pub struct ProviderStepResult {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
}

#[derive(Default)]
pub struct ProviderStepAccumulator {
    text: String,
    reasoning: String,
    result: Option<ProviderStepResult>,
}

impl ProviderStepAccumulator {
    pub fn push(&mut self, event: &StreamEvent) -> Result<Option<ProviderStepResult>, String> {
        match event {
            StreamEvent::ContentToken(token) => self.text.push_str(token),
            StreamEvent::ReasoningToken(token) => self.reasoning.push_str(token),
            StreamEvent::ToolCallStart { .. } | StreamEvent::ToolCallArgsDelta { .. } => {}
            StreamEvent::Finished { tool_calls, usage } => {
                let result = ProviderStepResult {
                    text: self.text.clone(),
                    reasoning: self.reasoning.clone(),
                    tool_calls: tool_calls.clone(),
                    usage: token_usage_from_provider(*usage),
                };
                self.result = Some(result.clone());
                return Ok(Some(result));
            }
            StreamEvent::Error(error) => return Err(error.clone()),
        }
        Ok(None)
    }

    pub fn finish(&self) -> Result<ProviderStepResult, String> {
        self.result
            .clone()
            .ok_or_else(|| "provider stream ended without a final response".into())
    }
}

pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Value> {
    let messages = normalize_tool_call_ids(messages);
    messages
        .iter()
        .filter_map(|msg| match msg {
            AgentMessage::System { content } => Some(serde_json::json!({
                "role": "system",
                "content": content
            })),
            AgentMessage::User { content } => Some(serde_json::json!({
                "role": "user",
                "content": content
            })),
            AgentMessage::UserWithImages { content, images } => {
                let mut parts = Vec::new();
                if !content.trim().is_empty() {
                    parts.push(serde_json::json!({
                        "type": "text",
                        "text": content
                    }));
                }
                parts.extend(images.iter().map(|image| {
                    serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": image.data_url,
                            "detail": "auto"
                        }
                    })
                }));
                Some(serde_json::json!({
                    "role": "user",
                    "content": parts
                }))
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut map = serde_json::Map::new();
                map.insert("role".into(), "assistant".into());
                if let Some(c) = content {
                    map.insert("content".into(), c.clone().into());
                }
                if let Some(t) = tool_calls {
                    map.insert(
                        "tool_calls".into(),
                        serde_json::to_value(t).unwrap_or_default(),
                    );
                }
                Some(Value::Object(map))
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                ..
            } => {
                let id_str = if tool_call_id.is_empty() {
                    "call_0"
                } else {
                    tool_call_id
                };
                Some(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id_str,
                    "name": name,
                    "content": content
                }))
            }
            AgentMessage::Custom { .. } => compaction_summary_text(msg).map(|summary| {
                serde_json::json!({
                    "role": "user",
                    "content": format!("<context-checkpoint>\n{summary}\n</context-checkpoint>")
                })
            }),
        })
        .collect()
}

pub fn convert_to_codex_llm(messages: &[AgentMessage]) -> (String, Vec<Value>) {
    let messages = normalize_tool_call_ids(messages);
    let mut instructions = String::new();
    let mut items = Vec::new();

    for msg in &messages {
        match msg {
            AgentMessage::System { content } => {
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(content);
            }
            AgentMessage::User { content } => {
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": content }]
                }));
            }
            AgentMessage::UserWithImages { content, images } => {
                let mut parts = Vec::new();
                if !content.trim().is_empty() {
                    parts.push(serde_json::json!({
                        "type": "input_text",
                        "text": content
                    }));
                }
                parts.extend(images.iter().map(|image| {
                    serde_json::json!({
                        "type": "input_image",
                        "image_url": image.data_url,
                        "detail": "auto"
                    })
                }));
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": parts
                }));
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if let Some(c) = content {
                    if !c.trim().is_empty() {
                        items.push(serde_json::json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": c }]
                        }));
                    }
                }
                if let Some(t_calls) = tool_calls {
                    for tc in t_calls {
                        items.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.function.name,
                            "arguments": tc.function.arguments
                        }));
                    }
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                content,
                ..
            } => {
                items.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": content
                }));
            }
            AgentMessage::Custom { .. } => {
                if let Some(summary) = compaction_summary_text(msg) {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": format!("<context-checkpoint>\n{summary}\n</context-checkpoint>")
                        }]
                    }));
                }
            }
        }
    }

    (instructions, items)
}

fn normalize_tool_call_ids(messages: &[AgentMessage]) -> Vec<AgentMessage> {
    let mut tool_index = 0;
    messages
        .iter()
        .map(|message| match message {
            AgentMessage::Assistant {
                content,
                tool_calls: Some(tool_calls),
                stop_reason,
                deferred_handle,
            } => {
                tool_index = 0;
                AgentMessage::Assistant {
                    content: content.clone(),
                    tool_calls: Some(
                        tool_calls
                            .iter()
                            .enumerate()
                            .map(|(idx, call)| {
                                let mut call = call.clone();
                                call.id = normalized_tool_call_id(&call.id, idx);
                                call
                            })
                            .collect(),
                    ),
                    stop_reason: stop_reason.clone(),
                    deferred_handle: deferred_handle.clone(),
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                terminate,
            } => {
                let normalized = normalized_tool_call_id(tool_call_id, tool_index);
                tool_index += 1;
                AgentMessage::Tool {
                    tool_call_id: normalized,
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    terminate: *terminate,
                }
            }
            other => {
                tool_index = 0;
                other.clone()
            }
        })
        .collect()
}

#[derive(Clone)]
struct ToolExecutorRoute {
    executor: Arc<dyn ToolExecutor>,
    tool_names: HashSet<String>,
}

pub type ToolIntentRecorder = Arc<
    dyn Fn(&str, &str, &str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub type ToolCompletionRecorder = Arc<
    dyn Fn(&str, bool) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

pub type ProviderUsageRecorder = Arc<
    dyn Fn(TokenUsage) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

pub type ProviderDiscardedUsageRecorder = ProviderUsageRecorder;

pub type StreamingStateRecorder = Arc<
    dyn Fn(
            crate::harness::StreamingState,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub type ProviderHookRecorder = Arc<
    dyn Fn(
            crate::harness::HookKind,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send>>
        + Send
        + Sync,
>;

pub type AssistantMessageRecorder = Arc<
    dyn Fn(AgentMessage) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

async fn run_provider_hook(
    recorder: Option<&ProviderHookRecorder>,
    kind: crate::harness::HookKind,
) -> Result<(), String> {
    let Some(recorder) = recorder else {
        return Ok(());
    };
    for failure in recorder(kind).await? {
        eprintln!("provider {:?} hook failed: {failure}", kind);
    }
    Ok(())
}

#[derive(Clone)]
struct ToolRunContext {
    before_hook: Option<Arc<dyn BeforeToolCallHook>>,
    after_hook: Option<Arc<dyn AfterToolCallHook>>,
    intent_recorder: Option<ToolIntentRecorder>,
    event_tx: broadcast::Sender<AgentEvent>,
    state: Arc<Mutex<AgentState>>,
    tool_routes: Vec<ToolExecutorRoute>,
    allowed_tool_names: Option<HashSet<String>>,
    work_dir: Option<PathBuf>,
    skip_before_hook: bool,
}

struct PreparedToolCall {
    tc: ToolCall,
    arguments: String,
    agent_tool_call: AgentToolCall,
    context: ToolRunContext,
}

pub struct AgentLoop {
    pub state: Arc<Mutex<AgentState>>,
    pub api_key: String,
    pub account_id: Option<String>,
    provider_client: ProviderClient,
    prompt_cache_key: Option<String>,
    pub tool_execution_mode: ToolExecutionMode,
    allowed_tool_names: Option<HashSet<String>>,
    steering_queue: PendingMessageQueue,
    follow_up_queue: PendingMessageQueue,
    pub before_tool_call_hook: Option<Arc<dyn BeforeToolCallHook>>,
    pub after_tool_call_hook: Option<Arc<dyn AfterToolCallHook>>,
    pub tool_intent_recorder: Option<ToolIntentRecorder>,
    pub tool_completion_recorder: Option<ToolCompletionRecorder>,
    pub provider_usage_recorder: Option<ProviderUsageRecorder>,
    pub provider_discarded_usage_recorder: Option<ProviderDiscardedUsageRecorder>,
    pub streaming_state_recorder: Option<StreamingStateRecorder>,
    pub provider_hook_recorder: Option<ProviderHookRecorder>,
    pub assistant_message_recorder: Option<AssistantMessageRecorder>,
    pub tool_message_recorder: Option<AssistantMessageRecorder>,
    transform_context_hook: Option<Arc<dyn TransformContextHook>>,
    should_stop_hook: Option<Arc<dyn ShouldStopAfterTurnHook>>,
    pub event_tx: broadcast::Sender<AgentEvent>,
    tool_executors: Vec<Arc<dyn ToolExecutor>>,
    /// Compatibility slot for existing callers. New code should use
    /// `register_tool_executor` so ordering and schema conflicts are validated.
    extension_manager: Option<Arc<dyn ToolExecutor>>,
    pub work_dir: Option<PathBuf>,
    stream_rules: Vec<(crate::rules::StreamRule, regex::Regex)>,
}

impl AgentLoop {
    pub fn new(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(500);
        let state = Arc::new(Mutex::new(AgentState::new(
            model,
            "You are threadlane AI coding agent.",
        )));
        let api_key = api_key.into();
        let provider_client = ProviderClient::new(api_key.clone(), account_id.clone());

        Self {
            state,
            api_key,
            account_id,
            provider_client,
            prompt_cache_key: None,
            tool_execution_mode: ToolExecutionMode::Parallel,
            allowed_tool_names: None,
            steering_queue: PendingMessageQueue::new(QueueMode::All),
            follow_up_queue: PendingMessageQueue::new(QueueMode::All),
            before_tool_call_hook: None,
            after_tool_call_hook: None,
            tool_intent_recorder: None,
            tool_completion_recorder: None,
            provider_usage_recorder: None,
            provider_discarded_usage_recorder: None,
            streaming_state_recorder: None,
            provider_hook_recorder: None,
            assistant_message_recorder: None,
            tool_message_recorder: None,
            transform_context_hook: None,
            should_stop_hook: None,
            event_tx,
            tool_executors: Vec::new(),
            extension_manager: None,
            work_dir: None,
            stream_rules: Vec::new(),
        }
    }

    pub fn set_prompt_cache_key(&mut self, key: Option<String>) {
        self.prompt_cache_key = key
            .map(|key| clamp_prompt_cache_key(&key))
            .filter(|key| !key.is_empty());
    }

    pub fn set_credentials(&mut self, api_key: impl Into<String>, account_id: Option<String>) {
        let api_key = api_key.into();
        self.provider_client = ProviderClient::new(api_key.clone(), account_id.clone());
        self.api_key = api_key;
        self.account_id = account_id;
    }

    /// Restricts both advertised and executable tools. `None` restores the
    /// default behavior where all registered, state, and core tools are available.
    pub fn set_allowed_tool_names(&mut self, allowed_tool_names: Option<HashSet<String>>) {
        self.allowed_tool_names = allowed_tool_names;
    }

    pub fn set_stream_rules(&mut self, rules: Vec<crate::rules::StreamRule>) {
        self.stream_rules = rules
            .into_iter()
            .filter_map(|rule| regex::Regex::new(&rule.pattern).ok().map(|re| (rule, re)))
            .collect();
    }

    /// Returns the core and registered executor schemas in provider order,
    /// after conflict deduplication and the active allowlist are applied.
    pub fn configured_tool_definitions(&self) -> Vec<AgentToolDefinition> {
        let mut definitions = collect_tool_definitions(
            &[],
            &self.tool_executors,
            self.compatibility_executor().as_ref(),
        );
        if let Some(allowed_tool_names) = &self.allowed_tool_names {
            definitions.retain(|definition| allowed_tool_names.contains(&definition.name));
        }
        definitions
    }

    pub fn register_tool_executor(
        &mut self,
        executor: Arc<dyn ToolExecutor>,
    ) -> Result<(), String> {
        let executor_id = executor.executor_id().trim();
        if executor_id.is_empty() {
            return Err("Tool executor id must not be empty".into());
        }
        if self
            .ordered_tool_executors()
            .iter()
            .any(|registered| registered.executor_id() == executor_id)
        {
            return Err(format!(
                "Tool executor '{executor_id}' is already registered"
            ));
        }

        let mut known_names: HashSet<String> = core_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        for registered in self.ordered_tool_executors() {
            known_names.extend(
                registered
                    .tool_definitions()
                    .into_iter()
                    .map(|definition| definition.name),
            );
        }
        for definition in executor.tool_definitions() {
            if definition.name.trim().is_empty() {
                return Err(format!(
                    "Tool executor '{executor_id}' provided an empty tool name"
                ));
            }
            if !known_names.insert(definition.name.clone()) {
                return Err(format!(
                    "Tool schema '{}' from executor '{executor_id}' conflicts with an existing schema",
                    definition.name
                ));
            }
        }

        self.tool_executors.push(executor);
        Ok(())
    }

    pub fn tool_executor_count(&self) -> usize {
        self.ordered_tool_executors().len()
    }

    fn compatibility_executor(&self) -> Option<Arc<dyn ToolExecutor>> {
        self.extension_manager.clone().filter(|compatibility| {
            !self
                .tool_executors
                .iter()
                .any(|registered| registered.executor_id() == compatibility.executor_id())
        })
    }

    fn ordered_tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        self.tool_executors
            .iter()
            .cloned()
            .chain(self.compatibility_executor())
            .collect()
    }

    async fn tool_execution_routes(&self) -> Vec<ToolExecutorRoute> {
        let state_tools = self.state.lock().await.tools.clone();
        let mut claimed_names: HashSet<String> = core_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        let mut routes = Vec::new();

        for executor in &self.tool_executors {
            let tool_names = executor
                .tool_definitions()
                .into_iter()
                .filter_map(|definition| {
                    claimed_names
                        .insert(definition.name.clone())
                        .then_some(definition.name)
                })
                .collect();
            routes.push(ToolExecutorRoute {
                executor: executor.clone(),
                tool_names,
            });
        }

        if let Some(executor) = self.compatibility_executor() {
            let tool_names = executor
                .tool_definitions()
                .into_iter()
                .map(|definition| definition.name)
                .chain(state_tools.iter().filter_map(|schema| {
                    AgentToolDefinition::from_provider_schema(schema)
                        .ok()
                        .map(|definition| definition.name)
                }))
                .filter(|name| claimed_names.insert(name.clone()))
                .collect();
            routes.push(ToolExecutorRoute {
                executor,
                tool_names,
            });
        }

        routes
    }

    async fn build_payload_helper(
        state_mutex: &Arc<Mutex<AgentState>>,
        tool_executors: &[Arc<dyn ToolExecutor>],
        allowed_tool_names: Option<&HashSet<String>>,
        compatibility_executor: Option<&Arc<dyn ToolExecutor>>,
        prompt_cache_key: Option<&str>,
        format: PayloadFormat,
    ) -> Value {
        let mut state = state_mutex.lock().await.clone();
        repair_interrupted_tool_turn(&mut state.messages);

        let mut definitions =
            collect_tool_definitions(&state.tools, tool_executors, compatibility_executor);
        if let Some(allowed_tool_names) = allowed_tool_names {
            definitions.retain(|definition| allowed_tool_names.contains(&definition.name));
        }

        match format {
            PayloadFormat::ChatCompletions => {
                let api_msgs = convert_to_llm(&state.messages);
                let tools: Vec<_> = definitions
                    .iter()
                    .map(AgentToolDefinition::to_chat_completions_tool)
                    .collect();
                let mut chat_payload = serde_json::json!({
                    "model": state.model,
                    "messages": api_msgs,
                    "tools": tools,
                    "stream": true,
                    "stream_options": { "include_usage": true }
                });
                if let Some(key) = prompt_cache_key {
                    chat_payload["prompt_cache_key"] = key.into();
                }
                if let Some(effort) = state.reasoning_effort.as_api_str() {
                    chat_payload["reasoning_effort"] = effort.into();
                }
                chat_payload
            }
            PayloadFormat::Codex => {
                let (instructions, codex_msgs) = convert_to_codex_llm(&state.messages);
                let codex_tools: Vec<_> = definitions
                    .iter()
                    .map(AgentToolDefinition::to_codex_responses_tool)
                    .collect();
                let mut codex_payload = serde_json::json!({
                    "model": state.model,
                    "instructions": instructions,
                    "input": codex_msgs,
                    "store": false,
                    "stream": true,
                    "tools": codex_tools
                });
                if let Some(key) = prompt_cache_key {
                    codex_payload["prompt_cache_key"] = key.into();
                }
                if let Some(effort) = state.reasoning_effort.as_api_str() {
                    codex_payload["reasoning"] = serde_json::json!({
                        "effort": effort,
                        "summary": "auto"
                    });
                }
                codex_payload
            }
        }
    }

    async fn build_chat_payload(&self) -> Value {
        Self::build_payload_helper(
            &self.state,
            &self.tool_executors,
            self.allowed_tool_names.as_ref(),
            self.compatibility_executor().as_ref(),
            self.prompt_cache_key.as_deref(),
            PayloadFormat::ChatCompletions,
        )
        .await
    }

    async fn build_codex_payload(&self) -> Value {
        Self::build_payload_helper(
            &self.state,
            &self.tool_executors,
            self.allowed_tool_names.as_ref(),
            self.compatibility_executor().as_ref(),
            self.prompt_cache_key.as_deref(),
            PayloadFormat::Codex,
        )
        .await
    }

    /// Builds both provider payloads without making a network request.
    pub async fn build_api_payloads(&self) -> (Value, Value) {
        (
            self.build_chat_payload().await,
            self.build_codex_payload().await,
        )
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    pub(crate) fn steer(&mut self, message: AgentMessage) {
        self.steering_queue.enqueue(message);
    }

    pub(crate) fn follow_up(&mut self, message: AgentMessage) {
        self.follow_up_queue.enqueue(message);
    }

    pub(crate) async fn run_prompt(&mut self, prompt: &str) {
        self.run_prompt_message(AgentMessage::User {
            content: prompt.to_string(),
        })
        .await;
    }

    /// Runs a complete user message, preserving multimodal attachments exactly.
    ///
    /// Panics if `message` is not a user message.
    pub(crate) async fn run_prompt_message(&mut self, message: AgentMessage) {
        assert!(message.is_user(), "prompt message must have a user role");
        {
            let mut state = self.state.lock().await;
            repair_interrupted_tool_turn(&mut state.messages);
            state.messages.push(message);
        }
        self.run_queued_turns().await;
    }

    /// Runs messages already placed in the follow-up queue without adding an
    /// artificial prompt. This lets host schedulers start queued work while
    /// the agent is idle.
    pub(crate) async fn run_follow_up(&mut self) {
        if !self.follow_up_queue.has_items() {
            return;
        }
        let items = self.follow_up_queue.drain();
        let mut state = self.state.lock().await;
        repair_interrupted_tool_turn(&mut state.messages);
        state.messages.extend(items);
        drop(state);
        self.run_queued_turns().await;
    }

    pub(crate) async fn run_steer(&mut self) {
        if !self.steering_queue.has_items() {
            return;
        }
        let items = self.steering_queue.drain();
        let mut state = self.state.lock().await;
        repair_interrupted_tool_turn(&mut state.messages);
        state.messages.extend(items);
        drop(state);
        self.run_queued_turns().await;
    }

    async fn run_queued_turns(&mut self) {
        let _ = self.event_tx.send(AgentEvent::AgentStart);
        let mut turn_number = 0;
        let mut overflow_recovery_attempted = false;
        let mut total_usage = TokenUsage::default();

        'turn_loop: loop {
            turn_number += 1;

            // Drain steering queue items
            if self.steering_queue.has_items() {
                let items = self.steering_queue.drain();
                let mut state = self.state.lock().await;
                state.messages.extend(items);
            }

            if let Err(error) = run_provider_hook(
                self.provider_hook_recorder.as_ref(),
                crate::harness::HookKind::BeforeContext,
            )
            .await
            {
                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                return;
            }

            // Apply context transformation hook if set
            if let Some(ref hook) = self.transform_context_hook {
                let msgs = {
                    let state = self.state.lock().await;
                    state.messages.clone()
                };
                let transformed = hook.transform_context(msgs).await;
                let mut state = self.state.lock().await;
                state.messages = transformed;
            }

            {
                let mut state = self.state.lock().await;
                if should_auto_compact(&state.messages) {
                    state.messages = compact_messages_to_token_budget(
                        &state.messages,
                        AUTO_COMPACTION_KEEP_RECENT_TOKENS,
                    );
                }
            }

            let _ = self.event_tx.send(AgentEvent::TurnStart { turn_number });

            let model = {
                let state = self.state.lock().await;
                state.model.clone()
            };

            let state = self.state.clone();
            let tool_executors = self.tool_executors.clone();
            let allowed_tool_names = self.allowed_tool_names.clone();
            let compatibility_executor = self.compatibility_executor();
            let pc_key = self.prompt_cache_key.clone();
            let provider_hook_recorder = self.provider_hook_recorder.clone();

            let payload_source = PayloadSource::lazy(model, move |format| {
                let state = state.clone();
                let tool_executors = tool_executors.clone();
                let allowed_tool_names = allowed_tool_names.clone();
                let compatibility_executor = compatibility_executor.clone();
                let pc_key = pc_key.clone();
                let provider_hook_recorder = provider_hook_recorder.clone();
                Box::pin(async move {
                    if let Err(error) = run_provider_hook(
                        provider_hook_recorder.as_ref(),
                        crate::harness::HookKind::BeforePayload,
                    )
                    .await
                    {
                        eprintln!("provider payload hook failed: {error}");
                    }
                    let payload = Self::build_payload_helper(
                        &state,
                        &tool_executors,
                        allowed_tool_names.as_ref(),
                        compatibility_executor.as_ref(),
                        pc_key.as_deref(),
                        format,
                    )
                    .await;
                    if let Err(error) = run_provider_hook(
                        provider_hook_recorder.as_ref(),
                        crate::harness::HookKind::AfterPayload,
                    )
                    .await
                    {
                        eprintln!("provider payload hook failed: {error}");
                    }
                    payload
                })
            });

            let (stream_tx, mut stream_rx) = mpsc::channel(100);
            let client = self.provider_client.clone();
            let prompt_cache_key = self.prompt_cache_key.clone();

            if let Err(error) = run_provider_hook(
                self.provider_hook_recorder.as_ref(),
                crate::harness::HookKind::BeforeRequest,
            )
            .await
            {
                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                return;
            }

            let _stream_task = AbortOnDrop::new(tokio::spawn(async move {
                client
                    .stream_chat_completion(payload_source, prompt_cache_key, stream_tx)
                    .await;
            }));

            if let Err(error) = run_provider_hook(
                self.provider_hook_recorder.as_ref(),
                crate::harness::HookKind::BeforeResponse,
            )
            .await
            {
                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                return;
            }

            let _ = self.event_tx.send(AgentEvent::MessageStart {
                role: "assistant".into(),
            });

            if let Some(recorder) = &self.streaming_state_recorder {
                if let Err(error) = recorder(crate::harness::StreamingState::default()).await {
                    let _ = self.event_tx.send(AgentEvent::AgentError { error });
                    return;
                }
            }

            let mut current_turn_text = String::new();
            let mut current_turn_reasoning = String::new();
            let mut captured_tool_calls: Vec<ToolCall> = Vec::new();
            let mut provider_step = ProviderStepAccumulator::default();

            let mut stream_monitor =
                crate::rules::StreamRuleMonitor::new(self.stream_rules.clone());
            let mut rule_triggered = None;

            while let Some(evt) = stream_rx.recv().await {
                let accumulated = provider_step.push(&evt);
                match evt {
                    StreamEvent::ContentToken(token) => {
                        current_turn_text.push_str(&token);
                        if let Some(rule_match) = stream_monitor.push_chunk(&token) {
                            rule_triggered = Some(rule_match);
                            break;
                        }
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: Some(token),
                            reasoning_delta: None,
                            tool_call_name: None,
                        });
                        if let Some(recorder) = &self.streaming_state_recorder {
                            if let Err(error) = recorder(crate::harness::StreamingState {
                                assistant_text: current_turn_text.clone(),
                                reasoning: current_turn_reasoning.clone(),
                                ..Default::default()
                            })
                            .await
                            {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                    }
                    StreamEvent::ReasoningToken(token) => {
                        current_turn_reasoning.push_str(&token);
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: Some(token),
                            tool_call_name: None,
                        });
                        if let Some(recorder) = &self.streaming_state_recorder {
                            if let Err(error) = recorder(crate::harness::StreamingState {
                                assistant_text: current_turn_text.clone(),
                                reasoning: current_turn_reasoning.clone(),
                                ..Default::default()
                            })
                            .await
                            {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                    }
                    StreamEvent::ToolCallStart { name, .. } => {
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: None,
                            tool_call_name: Some(name),
                        });
                    }
                    StreamEvent::ToolCallArgsDelta { .. } => {}
                    StreamEvent::Finished { .. } => {
                        let Some(step) = accumulated.ok().flatten() else {
                            let _ = self.event_tx.send(AgentEvent::AgentError {
                                error: "provider step did not produce a final response".into(),
                            });
                            return;
                        };
                        let usage = step.usage.clone();
                        if let Some(recorder) = &self.provider_usage_recorder {
                            if let Err(error) = recorder(usage.clone()).await {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                        if let Some(hook) = &self.provider_hook_recorder {
                            match hook(crate::harness::HookKind::AfterResponse).await {
                                Ok(failures) => {
                                    for failure in failures {
                                        eprintln!("provider after-response hook failed: {failure}");
                                    }
                                }
                                Err(error) => {
                                    let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                    return;
                                }
                            }
                        }
                        captured_tool_calls = step.tool_calls;
                        if let Some(recorder) = &self.streaming_state_recorder {
                            if let Err(error) = recorder(crate::harness::StreamingState {
                                assistant_text: current_turn_text.clone(),
                                reasoning: current_turn_reasoning.clone(),
                                tool_call_ids: captured_tool_calls
                                    .iter()
                                    .map(|call| call.id.clone())
                                    .collect(),
                                ..Default::default()
                            })
                            .await
                            {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                        total_usage.accumulate(&usage);
                        break;
                    }
                    StreamEvent::Error(err) => {
                        if let Some(recorder) = &self.provider_discarded_usage_recorder {
                            if let Err(error) = recorder(TokenUsage::default()).await {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                        if !overflow_recovery_attempted && is_context_overflow_error(&err) {
                            let mut state = self.state.lock().await;
                            let compacted = compact_messages_to_token_budget(
                                &state.messages,
                                AUTO_COMPACTION_KEEP_RECENT_TOKENS,
                            );
                            if compacted.len() < state.messages.len() {
                                state.messages = compacted;
                                overflow_recovery_attempted = true;
                                drop(state);
                                continue 'turn_loop;
                            }
                        }
                        let _ = self
                            .event_tx
                            .send(AgentEvent::AgentError { error: err.clone() });
                        return;
                    }
                }
            }

            if let Err(error) = run_provider_hook(
                self.provider_hook_recorder.as_ref(),
                crate::harness::HookKind::AfterRequest,
            )
            .await
            {
                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                return;
            }

            if rule_triggered.is_none() && provider_step.finish().is_err() {
                let _ = self.event_tx.send(AgentEvent::AgentError {
                    error: "provider stream ended without a final response".into(),
                });
                return;
            }

            if let Some(rule_match) = rule_triggered {
                let _ = self.event_tx.send(AgentEvent::StreamRuleTriggered {
                    rule_id: rule_match.rule_id.clone(),
                    rule_name: rule_match.rule_name.clone(),
                    matched_text: rule_match.matched_text.clone(),
                    reminder: rule_match.reminder.clone(),
                });

                let reminder_msg = AgentMessage::System {
                    content: format!(
                        "⚠ STREAM RULE INJECTION [{}: {}]: Matched invalid pattern '{}'. Reminder: {}. Please adjust your output and try again.",
                        rule_match.rule_id, rule_match.rule_name, rule_match.matched_text, rule_match.reminder
                    ),
                };

                let mut state = self.state.lock().await;
                state.messages.push(reminder_msg);
                drop(state);

                continue 'turn_loop;
            }

            let assistant_msg = AgentMessage::Assistant {
                content: if current_turn_text.is_empty() {
                    None
                } else {
                    Some(current_turn_text.clone())
                },
                tool_calls: if captured_tool_calls.is_empty() {
                    None
                } else {
                    Some(captured_tool_calls.clone())
                },
                stop_reason: None,
                deferred_handle: None,
            };

            if !current_turn_reasoning.trim().is_empty() {
                let thinking = AgentMessage::Custom {
                    custom_type: "thinking".into(),
                    payload: serde_json::json!({ "text": current_turn_reasoning }),
                };
                if let Some(recorder) = &self.assistant_message_recorder {
                    if let Err(error) = recorder(thinking.clone()).await {
                        let _ = self.event_tx.send(AgentEvent::AgentError { error });
                        return;
                    }
                }
                self.state.lock().await.messages.push(thinking);
            }
            self.state.lock().await.messages.push(assistant_msg.clone());

            if let Some(recorder) = &self.assistant_message_recorder {
                if let Err(error) = recorder(assistant_msg.clone()).await {
                    let _ = self.event_tx.send(AgentEvent::AgentError { error });
                    return;
                }
            }

            let _ = self.event_tx.send(AgentEvent::MessageEnd {
                message: assistant_msg,
            });

            if captured_tool_calls.is_empty() {
                if let Some(recorder) = &self.streaming_state_recorder {
                    if let Err(error) = recorder(crate::harness::StreamingState::default()).await {
                        let _ = self.event_tx.send(AgentEvent::AgentError { error });
                        return;
                    }
                }
                let _ = self.event_tx.send(AgentEvent::TurnEnd {
                    turn_number,
                    tool_results: Vec::new(),
                });

                if self.follow_up_queue.has_items() {
                    let items = self.follow_up_queue.drain();
                    let mut state = self.state.lock().await;
                    state.messages.extend(items);
                    continue;
                }
                break;
            }

            // Tool Execution
            let tool_results = self.execute_tools(&captured_tool_calls).await;

            let should_terminate = tool_results.iter().any(|r| r.terminate);

            let mut state = self.state.lock().await;
            for r in &tool_results {
                state.messages.push(AgentMessage::Tool {
                    tool_call_id: r.tool_call_id.clone(),
                    name: r.name.clone(),
                    content: r.content.clone(),
                    is_error: r.is_error,
                    terminate: r.terminate,
                });
            }
            drop(state);

            if let Some(recorder) = &self.tool_message_recorder {
                for result in &tool_results {
                    let message = AgentMessage::Tool {
                        tool_call_id: result.tool_call_id.clone(),
                        name: result.name.clone(),
                        content: result.content.clone(),
                        is_error: result.is_error,
                        terminate: result.terminate,
                    };
                    if let Err(error) = recorder(message).await {
                        let _ = self.event_tx.send(AgentEvent::AgentError { error });
                        return;
                    }
                }
            }

            if let Some(recorder) = &self.streaming_state_recorder {
                if let Err(error) = recorder(crate::harness::StreamingState::default()).await {
                    let _ = self.event_tx.send(AgentEvent::AgentError { error });
                    return;
                }
            }

            let _ = self.event_tx.send(AgentEvent::TurnEnd {
                turn_number,
                tool_results: tool_results.clone(),
            });

            if let Some(ref hook) = self.should_stop_hook {
                let state = self.state.lock().await.clone();
                if hook
                    .should_stop_after_turn(turn_number, &tool_results, &state)
                    .await
                {
                    break;
                }
            }

            if should_terminate {
                break;
            }
        }

        let _ = self
            .event_tx
            .send(AgentEvent::AgentEnd { usage: total_usage });
    }

    pub(crate) async fn resume_pending_turn(&mut self) {
        self.run_queued_turns().await;
    }

    pub async fn fetch_deferred(
        &self,
        model: &str,
        handle_id: &str,
    ) -> Result<threadlane_provider::DeferredResponse, String> {
        self.provider_client.fetch_deferred(model, handle_id).await
    }

    pub async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String> {
        self.provider_client.cancel_deferred(model, handle_id).await
    }

    pub async fn execute_tools(&self, tool_calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.execute_tools_with_options(tool_calls, self.tool_intent_recorder.clone(), false)
            .await
    }

    pub async fn execute_tools_without_intent_recording(
        &self,
        tool_calls: &[ToolCall],
    ) -> Vec<AgentToolResult> {
        self.execute_tools_with_options(tool_calls, None, false)
            .await
    }

    /// Replays already-intended safe tools. The before hook is intentionally
    /// not rerun: the durable ToolStarted record is the clearance boundary.
    pub async fn execute_tools_for_replay(&self, tool_calls: &[ToolCall]) -> Vec<AgentToolResult> {
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
            // Prepare and persist every intent in source order. Only the
            // external execution phase is parallel.
            let mut slots: Vec<Option<AgentToolResult>> = vec![None; tool_calls.len()];
            let mut prepared = Vec::new();
            for (index, tc) in tool_calls.iter().enumerate() {
                let context = ToolRunContext {
                    before_hook: self.before_tool_call_hook.clone(),
                    after_hook: self.after_tool_call_hook.clone(),
                    intent_recorder: intent_recorder.clone(),
                    event_tx: self.event_tx.clone(),
                    state: self.state.clone(),
                    tool_routes: tool_routes.clone(),
                    allowed_tool_names: allowed_tool_names.clone(),
                    work_dir: self.work_dir.clone(),
                    skip_before_hook,
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
                    if let Err(error) = recorder(&result.tool_call_id, result.terminate).await {
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
                before_hook: self.before_tool_call_hook.clone(),
                after_hook: self.after_tool_call_hook.clone(),
                intent_recorder,
                event_tx: self.event_tx.clone(),
                state: self.state.clone(),
                tool_routes,
                allowed_tool_names,
                work_dir: self.work_dir.clone(),
                skip_before_hook,
            },
        ))
        .catch_unwind()
        .await;

        match result {
            Ok(mut result) => {
                if let Some(recorder) = &self.tool_completion_recorder {
                    if let Err(error) = recorder(&result.tool_call_id, result.terminate).await {
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
                // A tool is untrusted session work. A panic must become a tool
                // result so the model can see the failure and retry or choose
                // another approach; it must not abort the entire agent loop.
                let result = AgentToolResult {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    content: format!("Tool '{}' failed: the tool panicked during execution. Please retry the tool or use another approach.", tc.function.name),
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
            if let Some(ref hook) = context.before_hook {
                let state_snapshot = context.state.lock().await.clone();
                let check = hook
                    .before_tool_call(&agent_tool_call, &state_snapshot)
                    .await;
                if check.block {
                    let res = AgentToolResult {
                        tool_call_id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        content: check
                            .reason
                            .unwrap_or_else(|| "Tool execution blocked by hook".into()),
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
                .execute_tool_with_call(&agent_tool_call, &arguments)
                .await
            {
                execution_result = Some(result);
                break;
            }
        }
        let execution_result = execution_result.unwrap_or_else(|| {
            Ok(match context.work_dir.as_deref() {
                Some(dir) => execute_tool_in_workspace(&tc.function.name, &arguments, dir),
                None => execute_tool(&tc.function.name, &arguments),
            })
        });
        let (content, is_error) = match execution_result {
            Ok(content) => (content, false),
            Err(error) => (format!("Tool executor error: {error}"), true),
        };
        let mut final_result = AgentToolResult {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            content,
            is_error,
            terminate: false,
        };

        if let Some(ref hook) = context.after_hook {
            let state_snapshot = context.state.lock().await.clone();
            let override_res = hook
                .after_tool_call(&agent_tool_call, &final_result, &state_snapshot)
                .await;
            if let Some(c) = override_res.override_content {
                final_result.content = c;
            }
            if let Some(err) = override_res.override_is_error {
                final_result.is_error = err;
            }
            if let Some(term) = override_res.terminate {
                final_result.terminate = term;
            }
        }

        final_result
    }
}

fn core_tool_definitions() -> Vec<AgentToolDefinition> {
    let mut seen = HashSet::new();
    get_available_tools()
        .into_iter()
        .chain(get_codex_tools())
        .filter_map(|schema| AgentToolDefinition::from_provider_schema(&schema).ok())
        .filter(|definition| seen.insert(definition.name.clone()))
        .collect()
}

fn collect_tool_definitions(
    state_tools: &[Value],
    registered_executors: &[Arc<dyn ToolExecutor>],
    compatibility_executor: Option<&Arc<dyn ToolExecutor>>,
) -> Vec<AgentToolDefinition> {
    let mut seen = HashSet::new();
    let mut definitions = Vec::new();

    for definition in core_tool_definitions()
        .into_iter()
        .chain(
            registered_executors
                .iter()
                .flat_map(|executor| executor.tool_definitions()),
        )
        .chain(
            compatibility_executor
                .into_iter()
                .flat_map(|executor| executor.tool_definitions()),
        )
        .chain(
            state_tools
                .iter()
                .filter_map(|schema| AgentToolDefinition::from_provider_schema(schema).ok()),
        )
    {
        if seen.insert(definition.name.clone()) {
            definitions.push(definition);
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

#[cfg(test)]
mod normalize_tool_arguments_tests {
    use super::*;
    use crate::types::{AfterToolCallResult, AgentState, AgentToolCall, BeforeToolCallResult};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use threadlane_provider::openai::{ToolCall, ToolCallFunction};

    struct CountingBeforeHook(Arc<AtomicUsize>);

    #[async_trait]
    impl BeforeToolCallHook for CountingBeforeHook {
        async fn before_tool_call(
            &self,
            _tool_call: &AgentToolCall,
            _state: &AgentState,
        ) -> BeforeToolCallResult {
            self.0.fetch_add(1, Ordering::SeqCst);
            BeforeToolCallResult::default()
        }
    }

    struct CountingAfterHook(Arc<AtomicUsize>);

    #[async_trait]
    impl AfterToolCallHook for CountingAfterHook {
        async fn after_tool_call(
            &self,
            _tool_call: &AgentToolCall,
            _result: &AgentToolResult,
            _state: &AgentState,
        ) -> AfterToolCallResult {
            self.0.fetch_add(1, Ordering::SeqCst);
            AfterToolCallResult::default()
        }
    }

    #[test]
    fn provider_step_accumulator_returns_one_stateless_result() {
        let mut step = ProviderStepAccumulator::default();
        step.push(&StreamEvent::ContentToken("answer".into()))
            .unwrap();
        step.push(&StreamEvent::ReasoningToken("thought".into()))
            .unwrap();
        let result = step
            .push(&StreamEvent::Finished {
                tool_calls: Vec::new(),
                usage: ProviderUsage {
                    input_tokens: 2,
                    output_tokens: 3,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    total_tokens: 5,
                },
            })
            .unwrap()
            .unwrap();
        assert_eq!(result.text, "answer");
        assert_eq!(result.reasoning, "thought");
        assert_eq!(result.usage.total_tokens, 5);
        let finished = step.finish().unwrap();
        assert_eq!(finished.text, result.text);
        assert_eq!(finished.reasoning, result.reasoning);
        assert_eq!(finished.usage, result.usage);
    }

    #[test]
    fn provider_step_accumulator_preserves_stream_errors() {
        let mut step = ProviderStepAccumulator::default();
        assert_eq!(
            step.push(&StreamEvent::Error("temporary failure".into()))
                .unwrap_err(),
            "temporary failure"
        );
        assert!(step.finish().is_err());
    }

    #[test]
    fn provider_step_accumulator_rejects_incomplete_streams() {
        let mut step = ProviderStepAccumulator::default();
        step.push(&StreamEvent::ContentToken("partial".into()))
            .unwrap();
        assert_eq!(
            step.finish().unwrap_err(),
            "provider stream ended without a final response"
        );
    }

    #[test]
    fn fills_missing_file_paths_from_the_workspace() {
        let arguments =
            normalize_tool_arguments("read_file", "{}", Some(std::path::Path::new("/workspace")));

        assert_eq!(arguments, r#"{"path":"/workspace"}"#);
    }

    #[test]
    fn normalizes_empty_tool_ids_by_tool_index() {
        let messages = vec![
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![
                    ToolCall {
                        id: String::new(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "read_file".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    },
                    ToolCall {
                        id: String::new(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "list_dir".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    },
                ]),
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::Tool {
                tool_call_id: String::new(),
                name: "read_file".into(),
                content: "one".into(),
                is_error: false,
                terminate: false,
            },
            AgentMessage::Tool {
                tool_call_id: String::new(),
                name: "list_dir".into(),
                content: "two".into(),
                is_error: false,
                terminate: false,
            },
        ];

        let chat = convert_to_llm(&messages);
        assert_eq!(chat[1]["tool_call_id"], "call_0");
        assert_eq!(chat[2]["tool_call_id"], "call_1");

        let (_, codex) = convert_to_codex_llm(&messages);
        assert_eq!(codex[2]["call_id"], "call_0");
        assert_eq!(codex[3]["call_id"], "call_1");
    }

    #[test]
    fn normalizes_empty_tool_ids_after_explicit_ids() {
        let messages = vec![
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![
                    ToolCall {
                        id: "provider-call".into(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "read_file".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    },
                    ToolCall {
                        id: String::new(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "list_dir".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    },
                ]),
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::Tool {
                tool_call_id: "provider-call".into(),
                name: "read_file".into(),
                content: "one".into(),
                is_error: false,
                terminate: false,
            },
            AgentMessage::Tool {
                tool_call_id: String::new(),
                name: "list_dir".into(),
                content: "two".into(),
                is_error: false,
                terminate: false,
            },
        ];

        let chat = convert_to_llm(&messages);
        assert_eq!(chat[2]["tool_call_id"], "call_1");
    }

    #[tokio::test]
    async fn tool_intent_recorder_sees_normalized_arguments_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let mut agent = AgentLoop::new("", None, "test");
        agent.work_dir = Some(dir.path().to_path_buf());
        let recorded = Arc::new(StdMutex::new(None));
        let recorded_for_callback = recorded.clone();
        agent.tool_intent_recorder = Some(Arc::new(move |id, name, arguments| {
            let recorded = recorded_for_callback.clone();
            let value = (id.to_string(), name.to_string(), arguments.to_string());
            Box::pin(async move {
                *recorded.lock().unwrap() = Some(value);
                Ok(())
            })
        }));

        let results = agent
            .execute_tools(&[ToolCall {
                id: "call-1".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "list_dir".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert_eq!(
            recorded.lock().unwrap().as_ref(),
            Some(&(
                "call-1".into(),
                "list_dir".into(),
                format!(r#"{{"path":"{}"}}"#, dir.path().display())
            ))
        );
        assert!(!results[0].is_error);
    }

    #[tokio::test]
    async fn tool_intent_recorder_failure_prevents_execution() {
        let mut agent = AgentLoop::new("", None, "test");
        agent.tool_intent_recorder = Some(Arc::new(|_, _, _| {
            Box::pin(async { Err("intent append failed".into()) })
        }));
        let mut events = agent.event_tx.subscribe();

        let results = agent
            .execute_tools(&[ToolCall {
                id: "call-1".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "list_dir".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert!(results[0].is_error);
        assert_eq!(results[0].content, "intent append failed");
        assert!(matches!(
            events.try_recv(),
            Ok(AgentEvent::ToolExecutionEnd { .. })
        ));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn safe_replay_skips_before_tool_hook() {
        let mut agent = AgentLoop::new("", None, "test");
        let before_calls = Arc::new(AtomicUsize::new(0));
        let after_calls = Arc::new(AtomicUsize::new(0));
        agent.before_tool_call_hook = Some(Arc::new(CountingBeforeHook(before_calls.clone())));
        agent.after_tool_call_hook = Some(Arc::new(CountingAfterHook(after_calls.clone())));
        let call = ToolCall {
            id: "call-1".into(),
            r#type: "function".into(),
            function: ToolCallFunction {
                name: "list_dir".into(),
                arguments: "{}".into(),
            },
            thought_signature: None,
        };

        let normal = agent.execute_tools(std::slice::from_ref(&call)).await;
        assert!(!normal[0].is_error);
        assert_eq!(before_calls.load(Ordering::SeqCst), 1);

        let replay = agent.execute_tools_for_replay(&[call]).await;
        assert!(!replay[0].is_error);
        assert_eq!(before_calls.load(Ordering::SeqCst), 1);
        assert_eq!(after_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn parallel_tool_intents_are_recorded_in_source_order() {
        let mut agent = AgentLoop::new("", None, "test");
        let recorded = Arc::new(StdMutex::new(Vec::new()));
        let recorded_for_callback = recorded.clone();
        agent.tool_intent_recorder = Some(Arc::new(move |id, _, _| {
            let id = id.to_owned();
            let recorded = recorded_for_callback.clone();
            Box::pin(async move {
                if id == "call-1" {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                recorded.lock().unwrap().push(id);
                Ok(())
            })
        }));

        let calls = ["call-1", "call-2"]
            .into_iter()
            .map(|id| ToolCall {
                id: id.into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "list_dir".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            })
            .collect::<Vec<_>>();
        let results = agent.execute_tools(&calls).await;

        assert!(results.iter().all(|result| !result.is_error));
        assert_eq!(recorded.lock().unwrap().as_slice(), ["call-1", "call-2"]);
    }

    #[tokio::test]
    async fn tool_completion_recorder_runs_after_execution() {
        let mut agent = AgentLoop::new("", None, "test");
        let completed = Arc::new(StdMutex::new(Vec::new()));
        let completed_for_callback = completed.clone();
        agent.tool_completion_recorder = Some(Arc::new(move |id, terminate| {
            let completed = completed_for_callback.clone();
            let id = id.to_owned();
            Box::pin(async move {
                completed.lock().unwrap().push((id, terminate));
                Ok(())
            })
        }));

        let results = agent
            .execute_tools(&[ToolCall {
                id: "call-1".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "list_dir".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert!(!results[0].is_error);
        assert_eq!(
            completed.lock().unwrap().as_slice(),
            [("call-1".into(), false)]
        );
    }

    #[test]
    fn set_credentials_updates_provider_routing() {
        let mut agent = AgentLoop::new("sk-openai", None, "test");
        assert_eq!(
            agent.provider_client.determine_format("gpt-5"),
            threadlane_provider::router::PayloadFormat::ChatCompletions
        );

        agent.set_credentials("codex-token", Some("account-id".into()));

        assert_eq!(agent.api_key, "codex-token");
        assert_eq!(agent.account_id.as_deref(), Some("account-id"));
        assert_eq!(
            agent.provider_client.determine_format("gpt-5"),
            threadlane_provider::router::PayloadFormat::Codex
        );
    }
}
