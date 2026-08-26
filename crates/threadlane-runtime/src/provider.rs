//! Provider adapter abstraction.
//!
//! Each LLM provider (OpenAI Chat Completions, OpenAI Codex Responses, etc.)
//! has its own message format and API payload shape. The [`ProviderAdapter`]
//! trait encapsulates these differences so the agent runtime can remain
//! provider-agnostic.
//!
//! The free functions `convert_to_llm` and `convert_to_codex_llm` live here
//! and are also re-exported from `loop_engine` for backward compatibility.

use crate::compaction::compaction_summary_text;
use crate::types::{AgentMessage, AgentToolDefinition, AgentToolResult, TokenUsage, TurnState};
use async_trait::async_trait;
use serde_json::Value;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    ChatCompletions,
    Codex,
}

/// Opaque provider-specific message representation.
///
/// Chat Completions providers receive `Vec<Value>` (array of message objects).
/// Codex Responses providers receive `(String, Vec<Value>)` (instructions + input items).
#[derive(Debug, Clone)]
pub enum ProviderMessages {
    ChatMessages(Vec<Value>),
    CodexMessages {
        instructions: String,
        input_items: Vec<Value>,
    },
}

/// Builds provider-specific API payloads from agent state.
///
/// Implementations encapsulate:
/// - Message format conversion (`AgentMessage` → provider messages)
/// - API payload structure (model, tools, streaming, reasoning, cache keys)
#[async_trait]
pub trait ProviderAdapter: fmt::Debug + Send + Sync {
    /// The [`PayloadFormat`] this adapter targets.
    fn format(&self) -> PayloadFormat;

    /// Converts agent messages into the provider's native message format.
    fn convert_messages(&self, messages: &[AgentMessage]) -> ProviderMessages;

    /// Builds a complete API payload from state, tool definitions, and an
    /// optional prompt cache key.
    ///
    /// The `state` is already locked by the caller; the adapter reads it
    /// but must not hold the lock across `.await`.
    fn build_payload(
        &self,
        state: &TurnState,
        tools: &[AgentToolDefinition],
        prompt_cache_key: Option<&str>,
    ) -> Value;
}

/// Chat Completions adapter (OpenAI, Antigravity, OpenCode).
#[derive(Debug, Clone, Default)]
pub struct ChatCompletionsAdapter;

#[async_trait]
impl ProviderAdapter for ChatCompletionsAdapter {
    fn format(&self) -> PayloadFormat {
        PayloadFormat::ChatCompletions
    }

    fn convert_messages(&self, messages: &[AgentMessage]) -> ProviderMessages {
        ProviderMessages::ChatMessages(convert_to_llm(messages))
    }

    fn build_payload(
        &self,
        state: &TurnState,
        tools: &[AgentToolDefinition],
        prompt_cache_key: Option<&str>,
    ) -> Value {
        let api_msgs = convert_to_llm(&state.messages);
        let tools: Vec<_> = tools
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
}

/// Codex Responses adapter.
#[derive(Debug, Clone, Default)]
pub struct CodexResponsesAdapter;

#[async_trait]
impl ProviderAdapter for CodexResponsesAdapter {
    fn format(&self) -> PayloadFormat {
        PayloadFormat::Codex
    }

    fn convert_messages(&self, messages: &[AgentMessage]) -> ProviderMessages {
        let (instructions, input_items) = convert_to_codex_llm(messages);
        ProviderMessages::CodexMessages {
            instructions,
            input_items,
        }
    }

    fn build_payload(
        &self,
        state: &TurnState,
        tools: &[AgentToolDefinition],
        prompt_cache_key: Option<&str>,
    ) -> Value {
        let (instructions, codex_msgs) = convert_to_codex_llm(&state.messages);
        let codex_tools: Vec<_> = tools
            .iter()
            .map(AgentToolDefinition::to_codex_responses_tool)
            .collect();
        let mut codex_payload = serde_json::json!({
            "model": state.model,
            "instructions": instructions,
            "store": false,
            "stream": true,
            "tools": codex_tools
        });
        // The Responses WebSocket protocol requires `input` on every
        // response.create event, including the first turn. Keep it as an
        // array even when the conversation currently has no input items.
        codex_payload["input"] = serde_json::Value::Array(codex_msgs);
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

/// A router that selects the correct [`ProviderAdapter`] for a given model.
///
/// # Example
///
/// ```ignore
/// let router = ProviderRouter::default();
/// let adapter = router.select_for_model("gpt-5.6-luna");
/// let payload = adapter.build_payload(&state, &tools, None);
/// ```
#[derive(Default)]
pub struct ProviderRouter {
    adapters: Vec<Arc<dyn ProviderAdapter>>,
}

impl fmt::Debug for ProviderRouter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRouter")
            .field("adapter_count", &self.adapters.len())
            .finish()
    }
}

impl Clone for ProviderRouter {
    fn clone(&self) -> Self {
        Self {
            adapters: self.adapters.clone(),
        }
    }
}

impl ProviderRouter {
    /// Creates a router with the default adapters (Chat Completions + Codex).
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Registers a custom adapter. Later registrations take priority over
    /// earlier ones when selecting by model.
    pub fn register(&mut self, adapter: Arc<dyn ProviderAdapter>) {
        self.adapters.push(adapter);
    }

    /// Returns the first adapter whose format matches the given format.
    #[allow(dead_code)]
    fn select(&self, format: PayloadFormat) -> Arc<dyn ProviderAdapter> {
        self.adapters
            .iter()
            .find(|adapter| adapter.format() == format)
            .cloned()
            .unwrap_or_else(|| default_adapter_for(format))
    }

    /// Builds a complete payload for the given format, reading state and tools
    /// from the caller.
    #[allow(dead_code)]
    pub(crate) fn build_payload(
        &self,
        format: PayloadFormat,
        state: &TurnState,
        tools: &[AgentToolDefinition],
        prompt_cache_key: Option<&str>,
    ) -> Value {
        self.select(format)
            .build_payload(state, tools, prompt_cache_key)
    }
}

#[allow(dead_code)]
fn default_adapter_for(format: PayloadFormat) -> Arc<dyn ProviderAdapter> {
    match format {
        PayloadFormat::ChatCompletions => Arc::new(ChatCompletionsAdapter),
        PayloadFormat::Codex => Arc::new(CodexResponsesAdapter),
    }
}

// ── Message conversion helpers ──────────────────────────────────────────

pub(crate) fn normalized_tool_call_id(id: &str, empty_index: usize) -> String {
    if id.is_empty() {
        format!("call_{empty_index}")
    } else {
        id.to_string()
    }
}

/// Converts agent messages into the standard Chat Completions message array.
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

/// Converts agent messages into the Codex Responses (instructions, input items) format.
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

// ── Recorder type aliases ───────────────────────────────────────────────

pub type ToolIntentRecorder = Arc<
    dyn Fn(&str, &str, &str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub type ToolCompletionRecorder = Arc<
    dyn Fn(&AgentToolResult) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub enum ToolExecutionTraceEvent {
    Started {
        tool_call_id: String,
        tool_name: String,
        executor_kind: String,
        effective_arguments: String,
        started_at_ms: u64,
    },
    Finished {
        tool_call_id: String,
        tool_name: String,
        executor_kind: String,
        started_at_ms: Option<u64>,
        duration_ms: Option<u64>,
        outcome: crate::harness::ToolExecutionOutcome,
        is_error: bool,
        terminate: bool,
        output_sha256: String,
        output_bytes: u64,
    },
}

pub type ToolExecutionTraceRecorder = Arc<
    dyn Fn(ToolExecutionTraceEvent) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct ProviderBoundaryRequest {
    pub attempt: u32,
    pub model: String,
    pub messages: Vec<AgentMessage>,
    pub tool_schema_json: Option<String>,
    pub overflow_recovery: bool,
}

#[derive(Clone, Debug)]
pub struct ProviderBoundaryResult {
    pub messages: Vec<AgentMessage>,
    pub context_limit: usize,
    pub context_limit_is_estimate: bool,
    pub compaction_generation: u64,
    pub provisional_estimated_tokens: Option<usize>,
    /// Durable sessions allocate these from the journal so a reopened driver
    /// cannot reuse process-local attempt/request identity.
    pub provider_attempt: Option<u32>,
    pub provider_request_id: Option<String>,
}

pub type ProviderBoundaryPreparer = Arc<
    dyn Fn(
            ProviderBoundaryRequest,
        ) -> Pin<Box<dyn Future<Output = Result<ProviderBoundaryResult, String>> + Send>>
        + Send
        + Sync,
>;
#[derive(Debug, Clone)]
pub enum ProviderTraceEvent {
    Started {
        attempt: u32,
        request_id: String,
        model: String,
        provider: String,
    },
    ContextManifest {
        attempt: u32,
        request_id: String,
        model: String,
        context_limit: Option<usize>,
        context_limit_is_estimate: bool,
        compaction_generation: u64,
        total_estimated_tokens: Option<u32>,
        items: Vec<crate::harness::ContextManifestItem>,
    },
    AssistantReady {
        attempt: u32,
        request_id: String,
        reasoning: Option<String>,
        message: AgentMessage,
    },
    Checkpoint {
        attempt: u32,
        request_id: String,
        checkpoint_index: u32,
        text: String,
        reasoning: Option<String>,
    },
    Finished {
        attempt: u32,
        request_id: String,
        outcome: crate::harness::ProviderOutcome,
        error: Option<crate::harness::ProviderErrorSummary>,
        duration_ms: u64,
        usage: Option<TokenUsage>,
    },
}

pub type ProviderTraceRecorder = Arc<
    dyn Fn(ProviderTraceEvent) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
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

pub type ModelContextRefresh = Arc<
    dyn Fn(
            Arc<tokio::sync::Mutex<TurnState>>,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ReasoningEffort;

    #[test]
    fn default_router_has_both_formats() {
        let router = ProviderRouter::default();
        assert!(
            router.select(PayloadFormat::ChatCompletions).format()
                == PayloadFormat::ChatCompletions
        );
        assert!(router.select(PayloadFormat::Codex).format() == PayloadFormat::Codex);
    }

    #[test]
    fn chat_adapter_builds_payload_with_reasoning() {
        let adapter = ChatCompletionsAdapter;
        let state = TurnState {
            system_prompt: "system".into(),
            messages: Vec::new(),
            model: "gpt-4o".into(),
            reasoning_effort: ReasoningEffort::High,
        };
        let payload = adapter.build_payload(&state, &[], None);
        assert_eq!(payload["model"], "gpt-4o");
        assert_eq!(payload["reasoning_effort"], "high");
        assert!(payload["stream"].as_bool().unwrap());
    }

    #[test]
    fn codex_adapter_builds_payload_with_reasoning() {
        let adapter = CodexResponsesAdapter;
        let state = TurnState {
            system_prompt: "system".into(),
            messages: Vec::new(),
            model: "gpt-5.6-luna".into(),
            reasoning_effort: ReasoningEffort::Low,
        };
        let payload = adapter.build_payload(&state, &[], None);
        assert_eq!(payload["model"], "gpt-5.6-luna");
        assert_eq!(payload["reasoning"]["effort"], "low");
        assert_eq!(payload["reasoning"]["summary"], "auto");
    }

    #[test]
    fn router_builds_correct_payload_per_format() {
        let router = ProviderRouter::default();
        let state = TurnState {
            system_prompt: "instructions".into(),
            messages: Vec::new(),
            model: "test-model".into(),
            reasoning_effort: ReasoningEffort::default(),
        };

        let chat = router.build_payload(PayloadFormat::ChatCompletions, &state, &[], None);
        assert!(chat.get("messages").is_some());

        let codex = router.build_payload(PayloadFormat::Codex, &state, &[], None);
        assert!(codex.get("instructions").is_some());
        // The WebSocket response.create envelope always includes input.
        assert_eq!(codex["input"], serde_json::json!([]));
        assert!(codex.get("prompt").is_none());
    }

    #[test]
    fn codex_adapter_uses_input_when_conversation_items_exist() {
        let router = ProviderRouter::default();
        let state = TurnState {
            system_prompt: "instructions".into(),
            messages: vec![
                AgentMessage::User {
                    content: "hello".into(),
                },
                AgentMessage::Assistant {
                    content: Some("hi there".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
                AgentMessage::User {
                    content: "second turn".into(),
                },
            ],
            model: "test-model".into(),
            reasoning_effort: ReasoningEffort::default(),
        };

        let codex = router.build_payload(PayloadFormat::Codex, &state, &[], None);
        let input = codex.get("input").and_then(|v| v.as_array());
        assert!(
            input.is_some(),
            "input array must be present for multi-turn conversations"
        );
        assert!(!input.unwrap().is_empty(), "input must not be empty");
    }

    #[test]
    fn codex_adapter_omits_input_when_only_system_message_exists() {
        let router = ProviderRouter::default();
        let state = TurnState {
            system_prompt: "instructions".into(),
            messages: vec![AgentMessage::System {
                content: "system prompt".into(),
            }],
            model: "test-model".into(),
            reasoning_effort: ReasoningEffort::default(),
        };

        let codex = router.build_payload(PayloadFormat::Codex, &state, &[], None);
        assert!(codex.get("instructions").is_some());
        // System messages produce an empty input array, which is still
        // required by the WebSocket response.create envelope.
        assert_eq!(codex["input"], serde_json::json!([]));
        assert!(codex.get("prompt").is_none());
    }
}

#[cfg(test)]
mod normalize_tool_arguments_tests {
    use super::*;
    use crate::turn_driver::ProviderStepAccumulator;
    use threadlane_provider::openai::{ProviderUsage, StreamEvent, ToolCall, ToolCallFunction};

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
}
