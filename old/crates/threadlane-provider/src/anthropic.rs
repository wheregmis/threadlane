//! Native Anthropic Messages API provider (`anthropic/<model>`).
//!
//! The router still builds a Chat Completions payload; this module translates
//! it (purely, see [`chat_payload_to_messages_request`]) into the Messages
//! wire format and maps the SSE stream back onto the shared `StreamEvent`
//! contract. Credentials are API keys only.

use crate::openai::{ProviderUsage, StreamEvent, ToolCall, ToolCallFunction};
use crate::router::{PayloadFormat, PayloadSource};
use crate::traits::ModelProvider;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const ANTHROPIC_MODEL_PREFIX: &str = "anthropic/";
/// `max_tokens` is required by the Messages API. Callers that set their own
/// limit on the payload win. Current models think adaptively and thinking
/// tokens count against this cap, so the default leaves generous room.
const DEFAULT_MAX_TOKENS: u64 = 32_000;

/// `ANTHROPIC_API_KEY` from the environment, if set and non-blank.
pub fn api_key_from_env() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

pub fn strip_anthropic_prefix(model: &str) -> &str {
    model.strip_prefix(ANTHROPIC_MODEL_PREFIX).unwrap_or(model)
}

#[derive(Debug, Clone, Default)]
pub struct AnthropicClient {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl AnthropicClient {
    pub(crate) fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: None,
        }
    }

    /// Injects an explicit API key (PR 2 wires stored keys); otherwise
    /// requests use `ANTHROPIC_API_KEY` from the environment.
    #[cfg(test)]
    pub(crate) fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        let key = api_key.into();
        self.api_key = (!key.trim().is_empty()).then_some(key);
        self
    }

    fn api_key(&self) -> Option<String> {
        self.api_key
            .clone()
            .filter(|key| !key.trim().is_empty())
            .or_else(api_key_from_env)
    }

    fn messages_url() -> String {
        let base = std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .map(|base| base.trim().to_string())
            .filter(|base| !base.is_empty())
            .unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE_URL.to_string());
        format!("{}/v1/messages", base.trim_end_matches('/'))
    }
}

// ---------------------------------------------------------------------------
// Request conversion (Chat Completions payload -> Messages request)
// ---------------------------------------------------------------------------

/// Anthropic tool ids must match `^[a-zA-Z0-9_-]+$`. Applied identically to
/// `tool_use.id` and `tool_result.tool_use_id` so pairs stay linked.
fn sanitize_tool_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "call_0".to_string()
    } else {
        cleaned
    }
}

fn text_block(text: &str) -> Option<Value> {
    (!text.trim().is_empty()).then(|| json!({"type": "text", "text": text}))
}

/// `data:<media>;base64,<data>` -> image block. Remote URLs are not fetched.
fn image_block(url: &str) -> Option<Value> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media_type = meta.strip_suffix(";base64")?;
    if media_type.is_empty() || data.is_empty() {
        return None;
    }
    Some(json!({
        "type": "image",
        "source": {"type": "base64", "media_type": media_type, "data": data}
    }))
}

/// Converts string-or-parts message content into Messages content blocks.
fn content_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => text_block(text).into_iter().collect(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => text_block(part.get("text").and_then(Value::as_str)?),
                Some("image_url") => {
                    let url = part
                        .get("image_url")
                        .and_then(|image| image.get("url").or(Some(image)))
                        .and_then(Value::as_str)?;
                    image_block(url)
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_tool_arguments(arguments: &Value, name: &str) -> Value {
    let parsed = match arguments {
        Value::String(raw) if raw.trim().is_empty() => None,
        Value::String(raw) => serde_json::from_str::<Value>(raw).ok(),
        Value::Object(_) => Some(arguments.clone()),
        _ => None,
    };
    match parsed {
        Some(value) if value.is_object() => value,
        _ => {
            if !matches!(arguments, Value::String(raw) if raw.trim().is_empty()) {
                tracing::warn!(tool = name, "invalid tool arguments; sending empty input");
            }
            json!({})
        }
    }
}

/// Appends `blocks` as a `role` message, merging into the previous message
/// when it has the same role (the API requires strict alternation).
fn push_message(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            if let Some(existing) = last.get_mut("content").and_then(Value::as_array_mut) {
                existing.extend(blocks);
                return;
            }
        }
    }
    messages.push(json!({"role": role, "content": blocks}));
}

/// `tool_result` blocks must precede any other content in a user message.
fn order_tool_results_first(messages: &mut [Value]) {
    for message in messages {
        if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
            blocks.sort_by_key(|block| {
                u8::from(block.get("type").and_then(Value::as_str) != Some("tool_result"))
            });
        }
    }
}

/// Pure translation of the router's Chat Completions payload into a streaming
/// Messages API request body.
pub fn chat_payload_to_messages_request(payload: &Value) -> Value {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(strip_anthropic_prefix)
        .unwrap_or_default();

    let mut system: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();

    for message in payload
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let content = message.get("content").unwrap_or(&Value::Null);
        match message.get("role").and_then(Value::as_str) {
            Some("system") | Some("developer") => {
                let text: Vec<String> = content_blocks(content)
                    .iter()
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect();
                if !text.is_empty() {
                    system.push(text.join("\n"));
                }
            }
            Some("user") => push_message(&mut messages, "user", content_blocks(content)),
            Some("assistant") => {
                // Replay the turn's thinking blocks unmodified, ahead of text and
                // tool_use, when the first tool call carries them.
                let mut blocks = message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .and_then(|calls| calls.first())
                    .and_then(|call| {
                        call.get("thoughtSignature")
                            .or_else(|| call.get("thought_signature"))
                    })
                    .and_then(Value::as_str)
                    .and_then(|signature| decode_thinking_blocks(signature, model))
                    .unwrap_or_default();
                blocks.extend(content_blocks(content));
                for call in message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    let function = call.get("function").unwrap_or(&Value::Null);
                    let name = function.get("name").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": sanitize_tool_id(id),
                        "name": name,
                        "input": parse_tool_arguments(
                            function.get("arguments").unwrap_or(&Value::Null),
                            name,
                        ),
                    }));
                }
                push_message(&mut messages, "assistant", blocks);
            }
            Some("tool") => {
                let id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let mut result = Map::new();
                result.insert("type".into(), "tool_result".into());
                result.insert("tool_use_id".into(), sanitize_tool_id(id).into());
                let blocks = content_blocks(content);
                // A tool_result always needs content so the model sees that the
                // call completed, even when the tool printed nothing.
                let body = if blocks.is_empty() {
                    Value::String("(no output)".to_string())
                } else if blocks
                    .iter()
                    .all(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                {
                    Value::String(
                        blocks
                            .iter()
                            .filter_map(|b| b.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                } else {
                    Value::Array(blocks)
                };
                result.insert("content".into(), body);
                push_message(&mut messages, "user", vec![Value::Object(result)]);
            }
            _ => {}
        }
    }
    order_tool_results_first(&mut messages);

    let max_tokens = payload
        .get("max_tokens")
        .or_else(|| payload.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .filter(|limit| *limit > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS);

    let mut request = Map::new();
    request.insert("model".into(), model.into());
    request.insert("max_tokens".into(), max_tokens.into());
    request.insert("stream".into(), true.into());
    if !system.is_empty() {
        request.insert("system".into(), system.join("\n\n").into());
    }
    request.insert("messages".into(), Value::Array(messages));
    // Documented top-level control (`output_config.effort`, GA on every model
    // in the catalog). Unrecognized values are omitted, which keeps the
    // model's own default instead of guessing.
    if let Some(effort) = payload
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(|effort| effort.trim().to_ascii_lowercase())
        .filter(|effort| matches!(effort.as_str(), "low" | "medium" | "high"))
    {
        request.insert("output_config".into(), json!({"effort": effort}));
    }

    let tools: Vec<Value> = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function")?;
            let name = function.get("name").and_then(Value::as_str)?;
            let mut out = Map::new();
            out.insert("name".into(), name.into());
            if let Some(description) = function.get("description").and_then(Value::as_str) {
                out.insert("description".into(), description.into());
            }
            out.insert(
                "input_schema".into(),
                function
                    .get("parameters")
                    .filter(|schema| schema.is_object())
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            );
            Some(Value::Object(out))
        })
        .collect();
    if !tools.is_empty() {
        request.insert("tools".into(), Value::Array(tools));
    }
    Value::Object(request)
}

// ---------------------------------------------------------------------------
// SSE parsing
// ---------------------------------------------------------------------------

struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// A `thinking` or `redacted_thinking` block being assembled from the stream.
struct PendingThinking {
    redacted: bool,
    thinking: String,
    signature: String,
    data: String,
}

impl PendingThinking {
    /// The block exactly as the API requires it back in a tool loop.
    fn into_block(self) -> Value {
        if self.redacted {
            json!({"type": "redacted_thinking", "data": self.data})
        } else {
            json!({"type": "thinking", "thinking": self.thinking, "signature": self.signature})
        }
    }
}

/// Serializes captured thinking blocks, tagged with the (bare) model that
/// produced them, into the string stored in the first tool call's
/// `thought_signature`. `None` when there is nothing to replay.
fn encode_thinking_blocks(model: &str, blocks: Vec<Value>) -> Option<String> {
    (!blocks.is_empty()).then(|| json!({"anthropic_model": model, "blocks": blocks}).to_string())
}

/// Parses a `thought_signature` written by [`encode_thinking_blocks`] and
/// returns its blocks only when they came from `model`: signatures are not
/// portable across models, so a mid-loop model switch drops them silently.
/// Anything else (a Gemini signature, the unreleased plain-array shape,
/// malformed JSON) is ignored too.
fn decode_thinking_blocks(signature: &str, model: &str) -> Option<Vec<Value>> {
    if !signature.trim_start().starts_with('{') {
        return None;
    }
    let stored = serde_json::from_str::<Value>(signature).ok()?;
    if stored.get("anthropic_model").and_then(Value::as_str)? != model {
        return None;
    }
    let blocks = stored.get("blocks")?.as_array()?.clone();
    let valid = !blocks.is_empty()
        && blocks.iter().all(|block| {
            let text = |key: &str| block.get(key).is_some_and(Value::is_string);
            match block.get("type").and_then(Value::as_str) {
                Some("thinking") => text("thinking") && text("signature"),
                Some("redacted_thinking") => text("data"),
                _ => false,
            }
        });
    valid.then_some(blocks)
}

/// True when `signature` is thinking data written by [`encode_thinking_blocks`]
/// (a JSON object with an `anthropic_model` key). Other providers must not
/// forward it: after a model switch it would otherwise reach them as if it
/// were their own opaque signature.
pub(crate) fn is_anthropic_thinking_signature(signature: &str) -> bool {
    serde_json::from_str::<Value>(signature)
        .ok()
        .is_some_and(|value| value.get("anthropic_model").is_some())
}

/// Incremental parser for the Messages SSE stream. Feed raw text with
/// [`SseParser::push`]; each call returns the events ready to emit. Terminal
/// state is reached on `message_stop` or an `error` event.
#[derive(Default)]
pub struct SseParser {
    /// Bare model id of the request, recorded with captured thinking blocks.
    model: String,
    buffer: String,
    /// Content block index -> in-flight tool call.
    tool_blocks: std::collections::BTreeMap<u64, PendingToolCall>,
    finished_calls: Vec<(u64, PendingToolCall)>,
    /// Thinking blocks in flight / completed, keyed by content block index so
    /// they replay in the order the model produced them.
    thinking_blocks: std::collections::BTreeMap<u64, PendingThinking>,
    usage: ProviderUsage,
    done: bool,
}

impl SseParser {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            ..Self::default()
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn push(&mut self, chunk: &str) -> Vec<StreamEvent> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();
        while let Some(line_end) = self.buffer.find('\n') {
            let line = self.buffer[..line_end].trim().to_string();
            self.buffer.drain(..=line_end);
            if self.done {
                continue;
            }
            // `event:` lines are redundant: every data payload carries `type`.
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
                continue;
            };
            self.handle_event(&value, &mut events);
        }
        events
    }

    fn handle_event(&mut self, event: &Value, out: &mut Vec<StreamEvent>) {
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(usage) = event.get("message").and_then(|m| m.get("usage")) {
                    self.read_usage(usage);
                }
            }
            Some("content_block_start") => {
                let block = event.get("content_block").unwrap_or(&Value::Null);
                let text = |key: &str| {
                    block
                        .get(key)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                if let kind @ ("thinking" | "redacted_thinking") = block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    let pending = PendingThinking {
                        redacted: kind == "redacted_thinking",
                        thinking: text("thinking"),
                        signature: text("signature"),
                        data: text("data"),
                    };
                    if !pending.thinking.is_empty() {
                        out.push(StreamEvent::ReasoningToken(pending.thinking.clone()));
                    }
                    self.thinking_blocks.insert(index, pending);
                } else if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    self.tool_blocks.insert(
                        index,
                        PendingToolCall {
                            id: block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            name: name.to_string(),
                            arguments: String::new(),
                        },
                    );
                    out.push(StreamEvent::ToolCallStart {
                        name: name.to_string(),
                    });
                }
            }
            Some("content_block_delta") => {
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                out.push(StreamEvent::ContentToken(text.to_string()));
                            }
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(chunk) = delta.get("partial_json").and_then(Value::as_str) {
                            if let Some(call) = self.tool_blocks.get_mut(&index) {
                                call.arguments.push_str(chunk);
                                if !chunk.is_empty() {
                                    out.push(StreamEvent::ToolCallArgsDelta {
                                        args_chunk: chunk.to_string(),
                                    });
                                }
                            }
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            if let Some(block) = self.thinking_blocks.get_mut(&index) {
                                block.thinking.push_str(text);
                            }
                            // Display "omitted" streams empty thinking; skip those.
                            if !text.is_empty() {
                                out.push(StreamEvent::ReasoningToken(text.to_string()));
                            }
                        }
                    }
                    Some("signature_delta") => {
                        if let (Some(signature), Some(block)) = (
                            delta.get("signature").and_then(Value::as_str),
                            self.thinking_blocks.get_mut(&index),
                        ) {
                            block.signature.push_str(signature);
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                if let Some(call) = self.tool_blocks.remove(&index) {
                    self.finished_calls.push((index, call));
                }
            }
            Some("message_delta") => {
                if let Some(usage) = event.get("usage") {
                    self.read_usage(usage);
                }
            }
            Some("message_stop") => {
                self.done = true;
                // Close any block the server never stopped explicitly.
                let open = std::mem::take(&mut self.tool_blocks);
                self.finished_calls.extend(open);
                let mut calls = std::mem::take(&mut self.finished_calls);
                calls.sort_by_key(|(index, _)| *index);
                // Thinking blocks must be returned complete and unmodified when
                // tool results are sent back, so they ride on the first tool
                // call's `thought_signature` through persistence and replay.
                let thinking = std::mem::take(&mut self.thinking_blocks)
                    .into_values()
                    .map(PendingThinking::into_block)
                    .collect::<Vec<_>>();
                let mut thinking_signature = encode_thinking_blocks(&self.model, thinking);
                let mut tool_calls: Vec<ToolCall> = calls
                    .into_iter()
                    .filter(|(_, call)| !call.name.is_empty())
                    .map(|(_, call)| ToolCall {
                        id: call.id,
                        r#type: "function".to_string(),
                        function: ToolCallFunction {
                            name: call.name,
                            arguments: if call.arguments.trim().is_empty() {
                                "{}".to_string()
                            } else {
                                call.arguments
                            },
                        },
                        thought_signature: None,
                    })
                    .collect();
                if let Some(first) = tool_calls.first_mut() {
                    first.thought_signature = thinking_signature.take();
                }
                let mut usage = self.usage;
                usage.total_tokens = usage
                    .input_tokens
                    .saturating_add(usage.cache_read_tokens)
                    .saturating_add(usage.cache_write_tokens)
                    .saturating_add(usage.output_tokens);
                out.push(StreamEvent::Finished { tool_calls, usage });
            }
            Some("error") => {
                self.done = true;
                let error = event.get("error").unwrap_or(&Value::Null);
                out.push(StreamEvent::Error(describe_api_error(
                    error.get("type").and_then(Value::as_str),
                    error.get("message").and_then(Value::as_str),
                )));
            }
            // `ping` and unknown future events are ignored.
            _ => {}
        }
    }

    /// Anthropic reports `input_tokens` excluding cache tokens, which matches
    /// the normalized `ProviderUsage` convention (uncached input) directly.
    /// `message_delta` usage is cumulative, so present fields overwrite.
    fn read_usage(&mut self, usage: &Value) {
        let field = |name: &str| usage.get(name).and_then(Value::as_u64);
        let clamp = |value: u64| value.min(u32::MAX as u64) as u32;
        if let Some(v) = field("input_tokens") {
            self.usage.input_tokens = clamp(v);
        }
        if let Some(v) = field("output_tokens") {
            self.usage.output_tokens = clamp(v);
        }
        if let Some(v) = field("cache_read_input_tokens") {
            self.usage.cache_read_tokens = clamp(v);
        }
        if let Some(v) = field("cache_creation_input_tokens") {
            self.usage.cache_write_tokens = clamp(v);
        }
    }
}

fn describe_api_error(kind: Option<&str>, message: Option<&str>) -> String {
    let detail = message.unwrap_or("no details");
    match kind {
        Some("authentication_error") => "Invalid Anthropic API key".to_string(),
        Some("rate_limit_error") => {
            format!("Anthropic rate limited, try again shortly ({detail})")
        }
        Some("overloaded_error") => {
            format!("Anthropic is overloaded, try again shortly ({detail})")
        }
        Some(kind) => format!("Anthropic API error ({kind}): {detail}"),
        None => format!("Anthropic API error: {detail}"),
    }
}

/// Maps a non-success HTTP response to a readable message. The key is never
/// echoed: only the status and the API's own error body are included.
pub fn describe_http_error(status: u16, body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let error = parsed.as_ref().and_then(|value| value.get("error"));
    let kind = error.and_then(|e| e.get("type")).and_then(Value::as_str);
    let message = error.and_then(|e| e.get("message")).and_then(Value::as_str);
    match status {
        401 => "Invalid Anthropic API key".to_string(),
        429 | 529 => format!(
            "Anthropic rate limited / overloaded ({status}), try again shortly{}",
            message.map(|m| format!(": {m}")).unwrap_or_default()
        ),
        _ if kind.is_some() || message.is_some() => {
            format!(
                "Anthropic API error ({status}): {}",
                describe_api_error(kind, message)
            )
        }
        _ => format!("Anthropic API error ({status})"),
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

#[async_trait]
impl ModelProvider for AnthropicClient {
    fn provider_id(&self) -> &'static str {
        "anthropic"
    }

    fn supports_model(&self, model: &str) -> bool {
        model.starts_with(ANTHROPIC_MODEL_PREFIX)
    }

    async fn stream_chat_completion(
        &self,
        payload_source: PayloadSource,
        _prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let Some(api_key) = self.api_key() else {
            let _ = event_tx
                .send(StreamEvent::Error(
                    "No Anthropic API key provided. Set the ANTHROPIC_API_KEY environment variable."
                        .to_string(),
                ))
                .await;
            return;
        };

        let chat_payload = payload_source.resolve(PayloadFormat::ChatCompletions).await;
        let body = chat_payload_to_messages_request(&chat_payload);

        // Retry transient failures only before a stream has started; replaying a
        // partially consumed stream could duplicate content or tool calls.
        let mut retries = 0;
        let response = loop {
            let response = match self
                .client
                .post(Self::messages_url())
                .header("x-api-key", &api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "text/event-stream")
                .header(USER_AGENT, "threadlane/1.0")
                .json(&body)
                .send()
                .await
            {
                Ok(res) => res,
                Err(err) => {
                    // reqwest errors can embed the URL but never request headers.
                    let _ = event_tx
                        .send(StreamEvent::Error(format!(
                            "Anthropic API request failed: {err}"
                        )))
                        .await;
                    return;
                }
            };
            if retries >= 2 || !matches!(response.status().as_u16(), 500 | 502 | 503 | 504) {
                break response;
            }
            tracing::warn!(
                status = %response.status(),
                retry = retries + 1,
                "retrying transient Anthropic server failure"
            );
            drop(response);
            tokio::select! {
                _ = event_tx.closed() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(1 << retries)) => {}
            }
            retries += 1;
        };

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            let _ = event_tx
                .send(StreamEvent::Error(describe_http_error(status, &text)))
                .await;
            return;
        }

        let mut parser = SseParser::new(
            body.get("model")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        let mut stream = response.bytes_stream();
        // Chunks can split a multi-byte character; decode only complete prefixes.
        let mut pending_bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(bytes) => bytes,
                Err(err) => {
                    let _ = event_tx
                        .send(StreamEvent::Error(format!(
                            "Error reading Anthropic stream: {err}"
                        )))
                        .await;
                    return;
                }
            };
            pending_bytes.extend_from_slice(&chunk);
            let valid_up_to = match std::str::from_utf8(&pending_bytes) {
                Ok(_) => pending_bytes.len(),
                Err(err) => err.valid_up_to(),
            };
            let text = String::from_utf8_lossy(&pending_bytes[..valid_up_to]).into_owned();
            pending_bytes.drain(..valid_up_to);
            for event in parser.push(&text) {
                if event_tx.send(event).await.is_err() {
                    return;
                }
            }
            if parser.is_done() {
                return;
            }
        }
        if !parser.is_done() {
            let _ = event_tx
                .send(StreamEvent::Error(
                    "Anthropic stream ended before the message completed".to_string(),
                ))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(parser: &mut SseParser, raw: &str) -> Vec<StreamEvent> {
        parser.push(raw)
    }

    fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| {
                format!(
                    "event: {}\ndata: {}\n\n",
                    e["type"].as_str().unwrap_or("x"),
                    e
                )
            })
            .collect()
    }

    // ---- conversion ----

    #[test]
    fn extracts_system_and_strips_prefix() {
        let request = chat_payload_to_messages_request(&json!({
            "model": "anthropic/claude-sonnet-x",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "system", "content": "be kind"},
                {"role": "user", "content": "hi"},
            ],
        }));
        assert_eq!(request["model"], "claude-sonnet-x");
        assert_eq!(request["system"], "be brief\n\nbe kind");
        assert_eq!(request["stream"], true);
        assert_eq!(request["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(
            request["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}])
        );
        assert!(request.get("tools").is_none());
    }

    #[test]
    fn maps_reasoning_effort_to_output_config() {
        let request_for = |effort: Value| {
            chat_payload_to_messages_request(&json!({
                "model": "anthropic/m", "reasoning_effort": effort, "messages": []
            }))
        };
        for level in ["low", "medium", "high"] {
            assert_eq!(
                request_for(json!(level))["output_config"],
                json!({"effort": level})
            );
        }
        assert_eq!(
            request_for(json!(" HIGH "))["output_config"],
            json!({"effort": "high"})
        );
        for ignored in [
            json!("off"),
            json!("minimal"),
            json!("ultra"),
            json!(7),
            json!(""),
        ] {
            assert!(request_for(ignored).get("output_config").is_none());
        }
        let absent = chat_payload_to_messages_request(&json!({
            "model": "anthropic/m", "messages": []
        }));
        assert!(absent.get("output_config").is_none());
    }

    #[test]
    fn detects_anthropic_thinking_signatures_only() {
        let ours =
            encode_thinking_blocks("m", vec![json!({"type": "redacted_thinking", "data": "d"})])
                .unwrap();
        assert!(is_anthropic_thinking_signature(&ours));
        for other in [
            "CiQBjz1rX-gemini-style-opaque",
            "",
            "[1,2]",
            "{\"blocks\":[]}",
            "not json {",
        ] {
            assert!(!is_anthropic_thinking_signature(other), "{other}");
        }
    }

    #[test]
    fn honors_payload_max_tokens() {
        let request = chat_payload_to_messages_request(&json!({
            "model": "anthropic/m", "max_tokens": 256, "messages": []
        }));
        assert_eq!(request["max_tokens"], 256);
    }

    #[test]
    fn merges_consecutive_same_role_and_drops_empty_text() {
        let request = chat_payload_to_messages_request(&json!({
            "model": "anthropic/m",
            "messages": [
                {"role": "user", "content": "a"},
                {"role": "user", "content": "   "},
                {"role": "user", "content": "b"},
                {"role": "assistant", "content": ""},
                {"role": "assistant", "content": "c"},
            ],
        }));
        assert_eq!(
            request["messages"],
            json!([
                {"role": "user", "content": [
                    {"type": "text", "text": "a"}, {"type": "text", "text": "b"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "c"}]},
            ])
        );
    }

    #[test]
    fn tool_call_round_trip_with_merged_results() {
        let request = chat_payload_to_messages_request(&json!({
            "model": "anthropic/m",
            "messages": [
                {"role": "user", "content": "list files"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "list_dir", "arguments": "{\"path\":\".\"}"}},
                    {"id": "call.2", "type": "function",
                     "function": {"name": "read_file", "arguments": ""}},
                ]},
                {"role": "tool", "tool_call_id": "call_1", "name": "list_dir", "content": "a.rs"},
                {"role": "tool", "tool_call_id": "call.2", "name": "read_file", "content": ""},
                {"role": "user", "content": "thanks"},
            ],
        }));
        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[1]["content"],
            json!([
                {"type": "tool_use", "id": "call_1", "name": "list_dir", "input": {"path": "."}},
                {"type": "tool_use", "id": "call_2", "name": "read_file", "input": {}},
            ])
        );
        // Both results plus the follow-up text share one user message, results first.
        assert_eq!(
            messages[2]["content"],
            json!([
                {"type": "tool_result", "tool_use_id": "call_1", "content": "a.rs"},
                {"type": "tool_result", "tool_use_id": "call_2", "content": "(no output)"},
                {"type": "text", "text": "thanks"},
            ])
        );
    }

    #[test]
    fn invalid_tool_arguments_become_empty_input() {
        let request = chat_payload_to_messages_request(&json!({
            "model": "anthropic/m",
            "messages": [{"role": "assistant", "tool_calls": [
                {"id": "c", "function": {"name": "t", "arguments": "{not json"}}]}],
        }));
        assert_eq!(request["messages"][0]["content"][0]["input"], json!({}));
    }

    #[test]
    fn maps_tool_schemas() {
        let request = chat_payload_to_messages_request(&json!({
            "model": "anthropic/m",
            "messages": [],
            "tools": [
                {"type": "function", "function": {
                    "name": "grep", "description": "search",
                    "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}}},
                {"type": "function", "function": {"name": "noop"}},
            ],
        }));
        assert_eq!(
            request["tools"],
            json!([
                {"name": "grep", "description": "search",
                 "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}}},
                {"name": "noop", "input_schema": {"type": "object", "properties": {}}},
            ])
        );
    }

    #[test]
    fn converts_data_url_images_and_skips_remote_urls() {
        let request = chat_payload_to_messages_request(&json!({
            "model": "anthropic/m",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA", "detail": "auto"}},
                {"type": "image_url", "image_url": {"url": "https://example.com/x.png"}},
            ]}],
        }));
        assert_eq!(
            request["messages"][0]["content"],
            json!([
                {"type": "text", "text": "look"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}},
            ])
        );
    }

    // ---- SSE ----

    #[test]
    fn parses_text_stream_with_usage() {
        let raw = sse(&[
            json!({"type": "message_start", "message": {"usage": {
                "input_tokens": 10, "cache_read_input_tokens": 5,
                "cache_creation_input_tokens": 2, "output_tokens": 1}}}),
            json!({"type": "ping"}),
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "Hel"}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "lo"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                   "usage": {"output_tokens": 7}}),
            json!({"type": "message_stop"}),
        ]);
        let mut parser = SseParser::default();
        let events = collect(&mut parser, &raw);
        assert!(parser.is_done());
        assert!(matches!(&events[0], StreamEvent::ContentToken(t) if t == "Hel"));
        assert!(matches!(&events[1], StreamEvent::ContentToken(t) if t == "lo"));
        match &events[2] {
            StreamEvent::Finished { tool_calls, usage } => {
                assert!(tool_calls.is_empty());
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 7);
                assert_eq!(usage.cache_read_tokens, 5);
                assert_eq!(usage.cache_write_tokens, 2);
                assert_eq!(usage.total_tokens, 24);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn parses_tool_call_with_split_json_across_chunks() {
        let raw = sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 3}}}),
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "ok"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1,
                   "content_block": {"type": "tool_use", "id": "toolu_1", "name": "list_dir", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1,
                   "delta": {"type": "input_json_delta", "partial_json": "{\"pa"}}),
            json!({"type": "content_block_delta", "index": 1,
                   "delta": {"type": "input_json_delta", "partial_json": "th\":\".\"}"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                   "usage": {"output_tokens": 4}}),
            json!({"type": "message_stop"}),
        ]);
        // Feed in awkward slices that split lines mid-way.
        let mut parser = SseParser::default();
        let mut events = Vec::new();
        for slice in raw.as_bytes().chunks(17) {
            events.extend(parser.push(std::str::from_utf8(slice).unwrap()));
        }
        assert!(matches!(&events[0], StreamEvent::ContentToken(t) if t == "ok"));
        assert!(matches!(&events[1], StreamEvent::ToolCallStart { name } if name == "list_dir"));
        assert!(
            matches!(&events[2], StreamEvent::ToolCallArgsDelta { args_chunk } if args_chunk == "{\"pa")
        );
        match events.last().unwrap() {
            StreamEvent::Finished { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "toolu_1");
                assert_eq!(tool_calls[0].r#type, "function");
                assert_eq!(tool_calls[0].function.name, "list_dir");
                assert_eq!(tool_calls[0].function.arguments, "{\"path\":\".\"}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_two_tool_calls_in_block_order() {
        let raw = sse(&[
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "tool_use", "id": "a", "name": "one", "input": {}}}),
            json!({"type": "content_block_start", "index": 1,
                   "content_block": {"type": "tool_use", "id": "b", "name": "two", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1,
                   "delta": {"type": "input_json_delta", "partial_json": "{\"x\":1}"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_stop"}),
        ]);
        let mut parser = SseParser::default();
        let events = collect(&mut parser, &raw);
        match events.last().unwrap() {
            StreamEvent::Finished { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 2);
                assert_eq!(tool_calls[0].function.name, "one");
                // No arguments streamed -> valid empty object.
                assert_eq!(tool_calls[0].function.arguments, "{}");
                assert_eq!(tool_calls[1].function.name, "two");
                assert_eq!(tool_calls[1].function.arguments, "{\"x\":1}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn error_event_is_terminal_and_readable() {
        let raw = sse(&[
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "partial"}}),
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
            json!({"type": "message_stop"}),
        ]);
        let mut parser = SseParser::default();
        let events = collect(&mut parser, &raw);
        assert!(parser.is_done());
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], StreamEvent::Error(m) if m.contains("overloaded")));
    }

    #[test]
    fn http_errors_are_mapped_without_leaking_keys() {
        assert_eq!(describe_http_error(401, "{}"), "Invalid Anthropic API key");
        assert!(describe_http_error(429, "").contains("rate limited"));
        assert!(describe_http_error(529, "").contains("overloaded"));
        let msg = describe_http_error(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#,
        );
        assert!(msg.contains("400") && msg.contains("bad"));
    }

    // ---- thinking ----

    fn thinking_stream() -> String {
        sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 4}}}),
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "thinking_delta", "thinking": "Let me "}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "thinking_delta", "thinking": "check."}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "signature_delta", "signature": "EuYB"}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "signature_delta", "signature": "sig=="}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1,
                   "content_block": {"type": "redacted_thinking", "data": "OPAQUE"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2,
                   "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 2,
                   "delta": {"type": "text_delta", "text": "Looking."}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "content_block_start", "index": 3,
                   "content_block": {"type": "tool_use", "id": "toolu_t", "name": "a", "input": {}}}),
            json!({"type": "content_block_stop", "index": 3}),
            json!({"type": "content_block_start", "index": 4,
                   "content_block": {"type": "tool_use", "id": "toolu_u", "name": "b", "input": {}}}),
            json!({"type": "content_block_stop", "index": 4}),
            json!({"type": "message_stop"}),
        ])
    }

    #[test]
    fn captures_thinking_blocks_on_the_first_tool_call_in_order() {
        let mut parser = SseParser::new("m");
        let events = parser.push(&thinking_stream());
        let reasoning: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ReasoningToken(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reasoning, ["Let me ", "check."]);
        let StreamEvent::Finished { tool_calls, .. } = events.last().unwrap() else {
            panic!("expected Finished");
        };
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(
            decode_thinking_blocks(tool_calls[0].thought_signature.as_deref().unwrap(), "m"),
            Some(vec![
                json!({"type": "thinking", "thinking": "Let me check.", "signature": "EuYBsig=="}),
                json!({"type": "redacted_thinking", "data": "OPAQUE"}),
            ])
        );
        assert!(tool_calls[1].thought_signature.is_none());
    }

    #[test]
    fn thinking_without_tool_calls_is_not_stored() {
        let raw = sse(&[
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "signature_delta", "signature": "S"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_stop"}),
        ]);
        let events = SseParser::default().push(&raw);
        // Omitted display: no reasoning text to stream.
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], StreamEvent::Finished { tool_calls, .. } if tool_calls.is_empty())
        );
    }

    fn assistant_with_signature(signature: Value) -> Value {
        assistant_for_model("anthropic/m", signature)
    }

    fn assistant_for_model(model: &str, signature: Value) -> Value {
        json!({
            "model": model,
            "messages": [
                {"role": "user", "content": "go"},
                {"role": "assistant", "content": "Looking.", "tool_calls": [
                    {"id": "c1", "type": "function", "thoughtSignature": signature,
                     "function": {"name": "a", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "name": "a", "content": "ok"},
            ],
        })
    }

    #[test]
    fn conversion_replays_thinking_blocks_first_and_unmodified() {
        let mut parser = SseParser::new("m");
        let events = parser.push(&thinking_stream());
        let StreamEvent::Finished { tool_calls, .. } = events.last().unwrap() else {
            panic!("expected Finished");
        };
        let signature = tool_calls[0].thought_signature.clone().unwrap();
        let stored: Value = serde_json::from_str(&signature).unwrap();
        assert_eq!(stored["anthropic_model"], "m");
        let request = chat_payload_to_messages_request(&assistant_with_signature(json!(signature)));
        assert_eq!(
            request["messages"][1]["content"],
            json!([
                {"type": "thinking", "thinking": "Let me check.", "signature": "EuYBsig=="},
                {"type": "redacted_thinking", "data": "OPAQUE"},
                {"type": "text", "text": "Looking."},
                {"type": "tool_use", "id": "c1", "name": "a", "input": {}},
            ])
        );
    }

    #[test]
    fn thinking_is_dropped_when_the_request_model_differs() {
        let signature = SseParser::new("claude-opus-5-5")
            .push(&thinking_stream())
            .into_iter()
            .find_map(|event| match event {
                StreamEvent::Finished { tool_calls, .. } => tool_calls[0].thought_signature.clone(),
                _ => None,
            })
            .unwrap();
        let replayed = |model: &str| {
            chat_payload_to_messages_request(&assistant_for_model(model, json!(signature)))
                ["messages"][1]["content"]
                .clone()
        };
        // Same model: replayed first.
        assert_eq!(replayed("anthropic/claude-opus-5-5")[0]["type"], "thinking");
        assert_eq!(
            replayed("anthropic/claude-opus-5-5")
                .as_array()
                .unwrap()
                .len(),
            4
        );
        // Different Anthropic model: dropped silently, request still valid.
        assert_eq!(
            replayed("anthropic/claude-sonnet-5-5"),
            json!([
                {"type": "text", "text": "Looking."},
                {"type": "tool_use", "id": "c1", "name": "a", "input": {}},
            ])
        );
    }

    #[test]
    fn legacy_plain_array_signatures_are_ignored() {
        let legacy = json!([{"type": "thinking", "thinking": "x", "signature": "S"}]).to_string();
        let request = chat_payload_to_messages_request(&assistant_with_signature(json!(legacy)));
        assert_eq!(request["messages"][1]["content"][0]["type"], "text");
    }

    #[test]
    fn foreign_or_malformed_signatures_are_ignored() {
        for signature in [
            json!("CiQBjz1rX-gemini-style-opaque"),
            json!("[1, 2]"),
            json!("{\"anthropic_model\":\"m\",\"blocks\":[]}"),
            json!("{\"anthropic_model\":\"m\",\"blocks\":[1]}"),
            json!("{\"anthropic_model\":\"m\"}"),
            json!("{\"blocks\":[{\"type\":\"redacted_thinking\",\"data\":\"d\"}]}"),
            json!("[]"),
            json!("[{\"type\":\"thinking\",\"thinking\":\"x\"}]"),
            json!("[{\"type\":\"text\",\"text\":\"x\"}]"),
            json!("{\"type\":\"thinking\"}"),
            json!(42),
        ] {
            let request = chat_payload_to_messages_request(&assistant_with_signature(signature));
            assert_eq!(
                request["messages"][1]["content"],
                json!([
                    {"type": "text", "text": "Looking."},
                    {"type": "tool_use", "id": "c1", "name": "a", "input": {}},
                ])
            );
        }
    }

    // ---- client ----

    #[test]
    fn client_supports_only_prefixed_models() {
        let client = AnthropicClient::new();
        assert!(client.supports_model("anthropic/claude-sonnet-x"));
        assert!(!client.supports_model("claude-sonnet-x"));
        assert!(!client.supports_model("opencode-go/kimi-k3"));
        assert_eq!(client.provider_id(), "anthropic");
    }

    #[test]
    fn explicit_key_wins_and_blank_is_ignored() {
        let client = AnthropicClient::new().with_api_key("sk-test");
        assert_eq!(client.api_key().as_deref(), Some("sk-test"));
        assert!(AnthropicClient::new().with_api_key("  ").api_key.is_none());
    }

    // ---- local mock server ----

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serializes tests that point `ANTHROPIC_BASE_URL` at a mock server.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Serves one canned HTTP response and returns the raw request it saw.
    async fn serve_once(
        status_line: &'static str,
        content_type: &'static str,
        body_parts: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request).into_owned();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= split + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let head = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            for part in body_parts {
                socket.write_all(part.as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            socket.shutdown().await.ok();
            String::from_utf8_lossy(&request).into_owned()
        });
        (base, handle)
    }

    async fn run_client(base: &str, payload: Value) -> Vec<StreamEvent> {
        std::env::set_var("ANTHROPIC_BASE_URL", base);
        let client = AnthropicClient::new().with_api_key("sk-ant-test-key");
        let (tx, mut rx) = mpsc::channel(64);
        client
            .stream_chat_completion(PayloadSource::ChatCompletions(payload), None, tx)
            .await;
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn streams_a_tool_turn_from_a_local_mock_server() {
        let _guard = ENV_LOCK.lock().await;
        let body = sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 9}}}),
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "signature_delta", "signature": "EqQBsig"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1,
                   "content_block": {"type": "tool_use", "id": "toolu_9", "name": "list_dir", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1,
                   "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\".\"}"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 2}}),
            json!({"type": "message_stop"}),
        ]);
        // Split mid-stream to exercise buffering over a real socket.
        let mid = body.len() / 2;
        let (base, server) = serve_once(
            "200 OK",
            "text/event-stream",
            vec![body[..mid].to_string(), body[mid..].to_string()],
        )
        .await;
        let events = run_client(
            &base,
            json!({
                "model": "anthropic/claude-test",
                "messages": [{"role": "user", "content": "list files"}],
                "tools": [{"type": "function", "function": {"name": "list_dir", "parameters": {"type": "object"}}}],
            }),
        )
        .await;
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /v1/messages "), "{request}");
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("x-api-key: sk-ant-test-key"));
        assert!(lower.contains("anthropic-version: 2023-06-01"));
        assert!(request.contains("\"model\":\"claude-test\""));
        assert!(request.contains("\"input_schema\""));
        assert!(matches!(&events[0], StreamEvent::ToolCallStart { name } if name == "list_dir"));
        match events.last().unwrap() {
            StreamEvent::Finished { tool_calls, usage } => {
                assert_eq!(tool_calls[0].id, "toolu_9");
                assert_eq!(tool_calls[0].function.arguments, "{\"path\":\".\"}");
                // Omitted-display thinking: empty text, signature preserved.
                assert_eq!(
                    decode_thinking_blocks(
                        tool_calls[0].thought_signature.as_deref().unwrap(),
                        "claude-test"
                    ),
                    Some(vec![
                        json!({"type": "thinking", "thinking": "", "signature": "EqQBsig"})
                    ])
                );
                assert_eq!(usage.input_tokens, 9);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Full production path: AgentMessage JSON -> ProviderClient routing ->
    /// convert_to_llm -> Messages request -> mock server -> StreamEvents.
    #[tokio::test]
    async fn provider_client_replays_a_real_tool_history() {
        use threadlane_protocol::{ProviderPort, RuntimeRequest};
        let _guard = ENV_LOCK.lock().await;
        let body = sse(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 5}}}),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "Two files."}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
            json!({"type": "message_stop"}),
        ]);
        let (base, server) = serve_once("200 OK", "text/event-stream", vec![body]).await;
        std::env::set_var("ANTHROPIC_BASE_URL", &base);
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-env-key");

        let request = RuntimeRequest {
            model: "anthropic/claude-sonnet-5-5".into(),
            messages: json!([
                {"role": "system", "content": "You are Threadlane."},
                {"role": "user", "content": "list the files"},
                {"role": "assistant", "content": "Checking.", "tool_calls": [
                    {"id": "call_a", "type": "function",
                     "thoughtSignature": json!({
                         "anthropic_model": "claude-sonnet-5-5",
                         "blocks": [
                             {"type": "thinking", "thinking": "plan", "signature": "SIG1"},
                             {"type": "redacted_thinking", "data": "ENC"}]}).to_string(),
                     "function": {"name": "list_dir", "arguments": "{\"path\":\".\"}"}},
                    {"id": "call_b", "type": "function",
                     "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}}]},
                {"role": "tool", "tool_call_id": "call_a", "name": "list_dir", "content": "a.rs\nb.rs"},
                {"role": "tool", "tool_call_id": "call_b", "name": "read_file", "content": "fn main() {}"},
            ]),
            tools: json!([{"type": "function", "function": {
                "name": "list_dir", "description": "list", "parameters": {"type": "object"}}}]),
            prompt_cache_key: Some("ignored".into()),
            reasoning_effort: Some("medium".into()),
        };
        let client = crate::router::ProviderClient::new("unused-openai-key".to_string(), None);
        let (tx, mut rx) = mpsc::channel(64);
        client.stream_request(request, tx).await;
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        std::env::remove_var("ANTHROPIC_API_KEY");

        let raw = server.await.unwrap();
        let lower = raw.to_ascii_lowercase();
        assert!(lower.contains("x-api-key: sk-ant-env-key"));
        assert!(
            !lower.contains("authorization:"),
            "must not send a bearer token"
        );
        assert!(lower.contains("\"output_config\":{\"effort\":\"medium\"}"));
        let sent: Value = serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(sent["model"], "claude-sonnet-5-5");
        assert_eq!(sent["system"], "You are Threadlane.");
        assert!(sent.get("reasoning_effort").is_none() && sent.get("stream_options").is_none());
        let messages = sent["messages"].as_array().unwrap();
        let roles: Vec<_> = messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
        let results = messages[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r["type"] == "tool_result"));
        // Thinking blocks come back first and unmodified, then text, then tool_use.
        assert_eq!(
            messages[1]["content"][0],
            json!({"type": "thinking", "thinking": "plan", "signature": "SIG1"})
        );
        assert_eq!(
            messages[1]["content"][1],
            json!({"type": "redacted_thinking", "data": "ENC"})
        );
        assert_eq!(messages[1]["content"][2]["type"], "text");
        assert_eq!(messages[1]["content"][3]["type"], "tool_use");
        assert_eq!(messages[1]["content"][4]["type"], "tool_use");
        assert!(matches!(&events[0], StreamEvent::ContentToken(t) if t == "Two files."));
        assert!(matches!(
            events.last().unwrap(),
            StreamEvent::Finished { .. }
        ));
    }

    #[tokio::test]
    async fn maps_http_401_without_leaking_the_key() {
        let _guard = ENV_LOCK.lock().await;
        let (base, server) = serve_once(
            "401 Unauthorized",
            "application/json",
            vec![r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#.to_string()],
        )
        .await;
        let events = run_client(
            &base,
            json!({"model": "anthropic/claude-test", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
        server.await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Error(message) => {
                assert_eq!(message, "Invalid Anthropic API key");
                assert!(!message.contains("sk-ant-test-key"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn truncated_stream_reports_an_error() {
        let _guard = ENV_LOCK.lock().await;
        let body = sse(&[json!({"type": "content_block_delta", "index": 0,
                                "delta": {"type": "text_delta", "text": "hi"}})]);
        let (base, server) = serve_once("200 OK", "text/event-stream", vec![body]).await;
        let events = run_client(
            &base,
            json!({"model": "anthropic/claude-test", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
        server.await.unwrap();
        assert!(matches!(&events[0], StreamEvent::ContentToken(t) if t == "hi"));
        assert!(
            matches!(events.last().unwrap(), StreamEvent::Error(m) if m.contains("ended before"))
        );
    }
}
