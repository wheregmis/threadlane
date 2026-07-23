use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::HeaderValue, Request},
        Message,
    },
    MaybeTlsStream, WebSocketStream,
};

const CODEX_SSE_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_WS_URL: &str = "wss://chatgpt.com/backend-api/codex/responses";
const CODEX_WS_BETA: &str = "responses_websockets=2026-02-06";
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const WS_MAX_AGE: Duration = Duration::from_secs(55 * 60);
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WS_RESPONSE_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

static CLIENT_SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

type CodexSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: ToolCallFunction,
}

pub const OPENAI_PROMPT_CACHE_KEY_MAX_CHARS: usize = 64;

pub fn clamp_prompt_cache_key(key: &str) -> String {
    key.chars()
        .take(OPENAI_PROMPT_CACHE_KEY_MAX_CHARS)
        .collect()
}

fn generated_session_key() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = CLIENT_SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    clamp_prompt_cache_key(&format!("mypi-{nanos:x}-{counter:x}"))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    ContentToken(String),
    ReasoningToken(String),
    ToolCallStart {
        name: String,
    },
    ToolCallArgsDelta {
        args_chunk: String,
    },
    Finished {
        tool_calls: Vec<ToolCall>,
        usage: ProviderUsage,
    },
    Error(String),
}

fn is_responses_text_delta(event_type: &str) -> bool {
    matches!(
        event_type,
        "response.output_text.delta"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
    )
}

fn parse_responses_text_delta(value: &Value) -> Option<StreamEvent> {
    let event_type = value.get("type")?.as_str()?;
    let delta = value.get("delta")?.as_str()?.to_string();
    match event_type {
        "response.output_text.delta" => Some(StreamEvent::ContentToken(delta)),
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            Some(StreamEvent::ReasoningToken(delta))
        }
        _ => None,
    }
}

fn token_count(value: Option<&Value>) -> u32 {
    value
        .and_then(Value::as_u64)
        .unwrap_or_default()
        .min(u32::MAX as u64) as u32
}

fn normalized_usage(
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    total_tokens: u32,
) -> ProviderUsage {
    ProviderUsage {
        input_tokens: input_tokens
            .saturating_sub(cache_read_tokens)
            .saturating_sub(cache_write_tokens),
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        total_tokens: if total_tokens == 0 {
            input_tokens.saturating_add(output_tokens)
        } else {
            total_tokens
        },
    }
}

fn parse_chat_usage(value: &Value) -> Option<ProviderUsage> {
    let usage = value.get("usage")?;
    let prompt_details = usage.get("prompt_tokens_details");
    Some(normalized_usage(
        token_count(usage.get("prompt_tokens")),
        token_count(usage.get("completion_tokens")),
        token_count(prompt_details.and_then(|details| details.get("cached_tokens"))),
        token_count(prompt_details.and_then(|details| details.get("cache_write_tokens"))),
        token_count(usage.get("total_tokens")),
    ))
}

fn parse_responses_usage(value: &Value) -> Option<ProviderUsage> {
    let usage = value
        .get("response")
        .and_then(|response| response.get("usage"))
        .or_else(|| value.get("usage"))?;
    let input_details = usage.get("input_tokens_details");
    Some(normalized_usage(
        token_count(usage.get("input_tokens")),
        token_count(usage.get("output_tokens")),
        token_count(input_details.and_then(|details| details.get("cached_tokens"))),
        token_count(input_details.and_then(|details| details.get("cache_write_tokens"))),
        token_count(usage.get("total_tokens")),
    ))
}

fn api_error_details(value: &Value) -> (String, String) {
    let nested = value.get("error").or_else(|| {
        value
            .get("response")
            .and_then(|response| response.get("error"))
    });
    let code = nested
        .and_then(|error| error.get("code"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("error")
        .to_string();
    let message = nested
        .and_then(|error| error.get("message"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| nested.unwrap_or(value).to_string());
    (code, message)
}

#[derive(Clone)]
struct Continuation {
    last_request: Value,
    response_id: String,
    response_items: Vec<Value>,
}

fn request_fields_match(left: &Value, right: &Value) -> bool {
    fn without_context(value: &Value) -> Option<Map<String, Value>> {
        let mut object = value.as_object()?.clone();
        object.remove("input");
        object.remove("previous_response_id");
        Some(object)
    }
    without_context(left) == without_context(right)
}

fn continuation_payload(current: &Value, continuation: &Continuation) -> Option<Value> {
    if !request_fields_match(current, &continuation.last_request) {
        return None;
    }
    let current_input = current.get("input")?.as_array()?;
    let mut expected_prefix = continuation.last_request.get("input")?.as_array()?.clone();
    expected_prefix.extend(continuation.response_items.iter().cloned());
    if !current_input.starts_with(&expected_prefix) {
        return None;
    }

    let mut payload = current.clone();
    let object = payload.as_object_mut()?;
    object.insert(
        "input".to_string(),
        Value::Array(current_input[expected_prefix.len()..].to_vec()),
    );
    object.insert(
        "previous_response_id".to_string(),
        Value::String(continuation.response_id.clone()),
    );
    Some(payload)
}

struct CodexWsState {
    default_session_key: String,
    session_key: Option<String>,
    socket: Option<CodexSocket>,
    opened_at: Option<Instant>,
    last_used: Option<Instant>,
    continuation: Option<Continuation>,
    in_flight: bool,
}

impl CodexWsState {
    fn new() -> Self {
        Self {
            default_session_key: generated_session_key(),
            session_key: None,
            socket: None,
            opened_at: None,
            last_used: None,
            continuation: None,
            in_flight: false,
        }
    }

    fn should_reset(&self, key: &str, now: Instant) -> bool {
        self.in_flight
            || self.session_key.as_deref().is_some_and(|old| old != key)
            || self
                .last_used
                .is_some_and(|last| now.duration_since(last) >= WS_IDLE_TIMEOUT)
            || self
                .opened_at
                .is_some_and(|opened| now.duration_since(opened) >= WS_MAX_AGE)
    }

    async fn reset(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None).await;
        }
        self.session_key = None;
        self.opened_at = None;
        self.last_used = None;
        self.continuation = None;
        self.in_flight = false;
    }
}

#[derive(Default)]
struct ResponseAccumulator {
    active_tool_calls: HashMap<usize, (String, String, String)>,
    codex_tool_indices: HashMap<String, usize>,
    announced_tools: HashSet<usize>,
    usage: ProviderUsage,
    assistant_text: String,
    response_id: Option<String>,
    emitted_model_event: bool,
}

enum ParsedEvent {
    Continue,
    Terminal { successful: bool },
    ProviderError { code: String, message: String },
    ReceiverClosed,
}

impl ResponseAccumulator {
    async fn emit(&mut self, tx: &mpsc::Sender<StreamEvent>, event: StreamEvent) -> bool {
        self.emitted_model_event = true;
        tx.send(event).await.is_ok()
    }

    async fn process(&mut self, value: &Value, tx: &mpsc::Sender<StreamEvent>) -> ParsedEvent {
        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        if event_type == "error" || event_type == "response.failed" || value.get("error").is_some()
        {
            let (code, message) = api_error_details(value);
            return ParsedEvent::ProviderError { code, message };
        }

        if let Some(parsed_usage) = parse_chat_usage(value) {
            self.usage = parsed_usage;
        }
        if matches!(
            event_type,
            "response.completed" | "response.done" | "response.incomplete"
        ) {
            if let Some(parsed_usage) = parse_responses_usage(value) {
                self.usage = parsed_usage;
            }
            self.response_id = value
                .get("response")
                .and_then(|response| response.get("id"))
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| self.response_id.take());
        } else if event_type == "response.created" {
            self.response_id = value
                .get("response")
                .and_then(|response| response.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }

        if matches!(
            event_type,
            "response.output_item.added" | "response.output_item.done"
        ) {
            if let Some(item) = value.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    let index = value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(self.active_tool_calls.len() as u64)
                        as usize;
                    let item_id = item
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if !item_id.is_empty() {
                        self.codex_tool_indices.insert(item_id, index);
                    }
                    let entry = self.active_tool_calls.entry(index).or_insert((
                        String::new(),
                        String::new(),
                        String::new(),
                    ));
                    entry.0 = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    entry.1 = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
                        entry.2 = arguments.to_string();
                    }
                    if !entry.1.is_empty() && self.announced_tools.insert(index) {
                        let name = entry.1.clone();
                        if !self.emit(tx, StreamEvent::ToolCallStart { name }).await {
                            return ParsedEvent::ReceiverClosed;
                        }
                    }
                }
            }
        } else if event_type == "response.function_call_arguments.delta" {
            let index = value
                .get("output_index")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .or_else(|| {
                    value
                        .get("item_id")
                        .and_then(Value::as_str)
                        .and_then(|id| self.codex_tool_indices.get(id).copied())
                })
                .unwrap_or(0);
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                self.active_tool_calls
                    .entry(index)
                    .or_insert((String::new(), String::new(), String::new()))
                    .2
                    .push_str(delta);
                if !self
                    .emit(
                        tx,
                        StreamEvent::ToolCallArgsDelta {
                            args_chunk: delta.to_string(),
                        },
                    )
                    .await
                {
                    return ParsedEvent::ReceiverClosed;
                }
            }
        } else if is_responses_text_delta(event_type) {
            if let Some(event) = parse_responses_text_delta(value) {
                if let StreamEvent::ContentToken(text) = &event {
                    self.assistant_text.push_str(text);
                }
                if !self.emit(tx, event).await {
                    return ParsedEvent::ReceiverClosed;
                }
            }
        } else if let Some(delta) = value.get("delta") {
            let token = delta
                .as_str()
                .or_else(|| delta.get("text").and_then(Value::as_str))
                .or_else(|| delta.get("content").and_then(Value::as_str));
            if let Some(token) = token.filter(|token| !token.is_empty()) {
                self.assistant_text.push_str(token);
                if !self
                    .emit(tx, StreamEvent::ContentToken(token.to_string()))
                    .await
                {
                    return ParsedEvent::ReceiverClosed;
                }
            }
        }

        if !is_responses_text_delta(event_type) {
            if let Some(text) = value
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                self.assistant_text.push_str(text);
                if !self
                    .emit(tx, StreamEvent::ContentToken(text.to_string()))
                    .await
                {
                    return ParsedEvent::ReceiverClosed;
                }
            }
        }

        if let Some(first) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        {
            if let Some(delta) = first.get("delta") {
                if let Some(content) = delta
                    .get("content")
                    .and_then(Value::as_str)
                    .filter(|content| !content.is_empty())
                {
                    if !self
                        .emit(tx, StreamEvent::ContentToken(content.to_string()))
                        .await
                    {
                        return ParsedEvent::ReceiverClosed;
                    }
                }
                if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for tool_call in tool_calls {
                        let index =
                            tool_call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let id = tool_call.get("id").and_then(Value::as_str);
                        let function = tool_call.get("function");
                        let name = function
                            .and_then(|value| value.get("name"))
                            .and_then(Value::as_str);
                        let arguments = function
                            .and_then(|value| value.get("arguments"))
                            .and_then(Value::as_str);
                        {
                            let entry = self.active_tool_calls.entry(index).or_insert((
                                String::new(),
                                String::new(),
                                String::new(),
                            ));
                            if let Some(id) = id {
                                entry.0 = id.to_string();
                            }
                            if let Some(name) = name {
                                entry.1 = name.to_string();
                            }
                            if let Some(arguments) = arguments {
                                entry.2.push_str(arguments);
                            }
                        }
                        if let Some(name) = name {
                            if self.announced_tools.insert(index)
                                && !self
                                    .emit(
                                        tx,
                                        StreamEvent::ToolCallStart {
                                            name: name.to_string(),
                                        },
                                    )
                                    .await
                            {
                                return ParsedEvent::ReceiverClosed;
                            }
                        }
                        if let Some(arguments) = arguments {
                            if !self
                                .emit(
                                    tx,
                                    StreamEvent::ToolCallArgsDelta {
                                        args_chunk: arguments.to_string(),
                                    },
                                )
                                .await
                            {
                                return ParsedEvent::ReceiverClosed;
                            }
                        }
                    }
                }
            }
        }

        match event_type {
            "response.completed" | "response.done" => ParsedEvent::Terminal { successful: true },
            "response.incomplete" => ParsedEvent::Terminal { successful: false },
            _ => ParsedEvent::Continue,
        }
    }

    fn tool_calls(&self) -> Vec<ToolCall> {
        let mut indices: Vec<_> = self.active_tool_calls.keys().copied().collect();
        indices.sort_unstable();
        indices
            .into_iter()
            .filter_map(|index| self.active_tool_calls.get(&index))
            .map(|(id, name, arguments)| ToolCall {
                id: id.clone(),
                r#type: "function".to_string(),
                function: ToolCallFunction {
                    name: name.clone(),
                    arguments: arguments.clone(),
                },
            })
            .collect()
    }

    fn canonical_response_items(&self) -> Vec<Value> {
        let mut items = Vec::new();
        if !self.assistant_text.is_empty() {
            items.push(serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": self.assistant_text }]
            }));
        }
        items.extend(self.tool_calls().into_iter().map(|tool_call| {
            serde_json::json!({
                "type": "function_call",
                "call_id": tool_call.id,
                "name": tool_call.function.name,
                "arguments": tool_call.function.arguments
            })
        }));
        items
    }

    async fn finish(self, tx: &mpsc::Sender<StreamEvent>) -> bool {
        tx.send(StreamEvent::Finished {
            tool_calls: self.tool_calls(),
            usage: self.usage,
        })
        .await
        .is_ok()
    }
}

pub async fn fetch_available_models(api_key: &str, account_id: Option<&str>) -> Vec<String> {
    let client = reqwest::Client::new();
    let mut req = client
        .get("https://api.openai.com/v1/models")
        .header(AUTHORIZATION, format!("Bearer {api_key}"));
    if let Some(account_id) = account_id {
        req = req.header("chatgpt-account-id", account_id);
    }
    if let Ok(response) = req.send().await {
        if response.status().is_success() {
            if let Ok(value) = response.json::<Value>().await {
                if let Some(data) = value.get("data").and_then(Value::as_array) {
                    let mut models: Vec<_> = data
                        .iter()
                        .filter_map(|item| item.get("id").and_then(Value::as_str))
                        .filter(|id| {
                            id.starts_with("gpt-")
                                || id.starts_with("o1")
                                || id.starts_with("o3")
                                || id.contains("codex")
                        })
                        .map(str::to_string)
                        .collect();
                    if !models.is_empty() {
                        models.sort();
                        return models;
                    }
                }
            }
        }
    }
    vec![
        "gpt-5.6-luna".to_string(),
        "gpt-5.4".to_string(),
        "gpt-5.4-mini".to_string(),
        "gpt-5.5".to_string(),
        "gpt-5.6-sol".to_string(),
        "gpt-5.6-terra".to_string(),
        "gpt-5.3-codex-spark".to_string(),
        "gpt-4o".to_string(),
        "gpt-4o-mini".to_string(),
    ]
}

#[derive(Clone)]
pub struct OpenAIClient {
    api_key: String,
    account_id: Option<String>,
    client: reqwest::Client,
    codex_ws: Arc<Mutex<CodexWsState>>,
}

enum WsResult {
    Finished,
    Failed {
        message: String,
        emitted: bool,
        fallback_allowed: bool,
    },
    Aborted,
}

impl OpenAIClient {
    pub fn new(api_key: String, account_id: Option<String>) -> Self {
        Self {
            api_key,
            account_id,
            client: reqwest::Client::new(),
            codex_ws: Arc::new(Mutex::new(CodexWsState::new())),
        }
    }

    pub async fn stream_chat_completion(
        &self,
        api_payload: Value,
        codex_payload: Value,
        prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let is_codex = self.account_id.is_some() || self.api_key.starts_with("ey");
        if !is_codex {
            self.stream_sse(
                "https://api.openai.com/v1/chat/completions",
                api_payload,
                None,
                false,
                &event_tx,
            )
            .await;
            return;
        }

        let session_key = match prompt_cache_key
            .as_deref()
            .map(clamp_prompt_cache_key)
            .filter(|key| !key.is_empty())
        {
            Some(key) => key,
            None => self.codex_ws.lock().await.default_session_key.clone(),
        };

        match self
            .stream_codex_websocket(codex_payload.clone(), &session_key, &event_tx)
            .await
        {
            WsResult::Finished | WsResult::Aborted => {}
            WsResult::Failed {
                message,
                emitted: true,
                ..
            }
            | WsResult::Failed {
                message,
                fallback_allowed: false,
                ..
            } => {
                let _ = event_tx.send(StreamEvent::Error(message)).await;
            }
            WsResult::Failed {
                emitted: false,
                fallback_allowed: true,
                ..
            } => {
                self.stream_sse(
                    CODEX_SSE_URL,
                    codex_payload,
                    Some(&session_key),
                    true,
                    &event_tx,
                )
                .await;
            }
        }
    }

    async fn websocket_request(&self, session_key: &str) -> Result<Request<()>, String> {
        let mut request = CODEX_WS_URL
            .into_client_request()
            .map_err(|error| error.to_string())?;
        let headers = request.headers_mut();
        let authorization = HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .map_err(|error| error.to_string())?;
        headers.insert("authorization", authorization);
        if let Some(account_id) = &self.account_id {
            headers.insert(
                "chatgpt-account-id",
                HeaderValue::from_str(account_id).map_err(|error| error.to_string())?,
            );
        }
        headers.insert("originator", HeaderValue::from_static("mypi"));
        headers.insert("user-agent", HeaderValue::from_static("mypi"));
        headers.insert("openai-beta", HeaderValue::from_static(CODEX_WS_BETA));
        let key = HeaderValue::from_str(session_key).map_err(|error| error.to_string())?;
        headers.insert("session-id", key.clone());
        headers.insert("x-client-request-id", key);
        Ok(request)
    }

    async fn stream_codex_websocket(
        &self,
        full_payload: Value,
        session_key: &str,
        event_tx: &mpsc::Sender<StreamEvent>,
    ) -> WsResult {
        let mut state = self.codex_ws.lock().await;
        let now = Instant::now();
        if state.should_reset(session_key, now) {
            state.reset().await;
        }
        if state.session_key.is_none() {
            state.session_key = Some(session_key.to_string());
        }
        let mut retried_missing_previous = false;
        loop {
            state.in_flight = true;
            if state.socket.is_none() {
                let request = match self.websocket_request(session_key).await {
                    Ok(request) => request,
                    Err(message) => {
                        state.reset().await;
                        return WsResult::Failed {
                            message: format!("Codex WebSocket request error: {message}"),
                            emitted: false,
                            fallback_allowed: false,
                        };
                    }
                };
                match tokio::time::timeout(WS_CONNECT_TIMEOUT, connect_async(request)).await {
                    Ok(Ok((socket, _))) => {
                        state.socket = Some(socket);
                        state.opened_at = Some(Instant::now());
                        state.last_used = Some(Instant::now());
                        state.session_key = Some(session_key.to_string());
                    }
                    Ok(Err(error)) => {
                        state.reset().await;
                        return WsResult::Failed {
                            message: format!("Codex WebSocket connection error: {error}"),
                            emitted: false,
                            fallback_allowed: true,
                        };
                    }
                    Err(_) => {
                        state.reset().await;
                        return WsResult::Failed {
                            message: format!(
                                "Codex WebSocket connection timed out after {} seconds",
                                WS_CONNECT_TIMEOUT.as_secs()
                            ),
                            emitted: false,
                            fallback_allowed: true,
                        };
                    }
                }
            }

            let continued_payload = state
                .continuation
                .as_ref()
                .and_then(|continuation| continuation_payload(&full_payload, continuation));
            let used_continuation = continued_payload.is_some();
            if state.continuation.is_some() && !used_continuation {
                state.continuation = None;
            }
            let mut frame_payload = continued_payload.unwrap_or_else(|| full_payload.clone());
            if let Some(object) = frame_payload.as_object_mut() {
                object.insert(
                    "type".to_string(),
                    Value::String("response.create".to_string()),
                );
            } else {
                state.reset().await;
                return WsResult::Failed {
                    message: "Codex payload must be a JSON object".to_string(),
                    emitted: false,
                    fallback_allowed: false,
                };
            }

            let text = match serde_json::to_string(&frame_payload) {
                Ok(text) => text,
                Err(error) => {
                    state.reset().await;
                    return WsResult::Failed {
                        message: format!("Codex WebSocket serialization error: {error}"),
                        emitted: false,
                        fallback_allowed: false,
                    };
                }
            };
            if let Err(error) = state
                .socket
                .as_mut()
                .expect("socket was connected")
                .send(Message::Text(text.into()))
                .await
            {
                state.reset().await;
                return WsResult::Failed {
                    message: format!("Codex WebSocket send error: {error}"),
                    emitted: false,
                    fallback_allowed: true,
                };
            }

            let mut accumulator = ResponseAccumulator::default();
            let terminal_success = loop {
                let message = match tokio::time::timeout(
                    WS_RESPONSE_IDLE_TIMEOUT,
                    state.socket.as_mut().expect("socket exists").next(),
                )
                .await
                {
                    Ok(Some(Ok(message))) => message,
                    Ok(Some(Err(error))) => {
                        let emitted = accumulator.emitted_model_event;
                        state.reset().await;
                        return WsResult::Failed {
                            message: format!("Codex WebSocket stream error: {error}"),
                            emitted,
                            fallback_allowed: true,
                        };
                    }
                    Ok(None) => {
                        let emitted = accumulator.emitted_model_event;
                        state.reset().await;
                        return WsResult::Failed {
                            message: "Codex WebSocket closed before the response completed"
                                .to_string(),
                            emitted,
                            fallback_allowed: true,
                        };
                    }
                    Err(_) => {
                        let emitted = accumulator.emitted_model_event;
                        state.reset().await;
                        return WsResult::Failed {
                            message: format!(
                                "Codex WebSocket response timed out after {} seconds",
                                WS_RESPONSE_IDLE_TIMEOUT.as_secs()
                            ),
                            emitted,
                            fallback_allowed: true,
                        };
                    }
                };

                let data = match message {
                    Message::Text(text) => text.to_string(),
                    Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => text,
                        Err(error) => {
                            let emitted = accumulator.emitted_model_event;
                            state.reset().await;
                            return WsResult::Failed {
                                message: format!("Codex WebSocket sent invalid UTF-8: {error}"),
                                emitted,
                                fallback_allowed: false,
                            };
                        }
                    },
                    Message::Close(_) => {
                        let emitted = accumulator.emitted_model_event;
                        state.reset().await;
                        return WsResult::Failed {
                            message: "Codex WebSocket closed before the response completed"
                                .to_string(),
                            emitted,
                            fallback_allowed: true,
                        };
                    }
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                };
                let value = match serde_json::from_str::<Value>(&data) {
                    Ok(value) => value,
                    Err(error) => {
                        let emitted = accumulator.emitted_model_event;
                        state.reset().await;
                        return WsResult::Failed {
                            message: format!("Codex WebSocket JSON error: {error}"),
                            emitted,
                            fallback_allowed: false,
                        };
                    }
                };
                match accumulator.process(&value, event_tx).await {
                    ParsedEvent::Continue => {}
                    ParsedEvent::Terminal { successful } => break successful,
                    ParsedEvent::ReceiverClosed => {
                        state.reset().await;
                        return WsResult::Aborted;
                    }
                    ParsedEvent::ProviderError { code, message } => {
                        if code == "previous_response_not_found"
                            && used_continuation
                            && !retried_missing_previous
                            && !accumulator.emitted_model_event
                        {
                            retried_missing_previous = true;
                            state.reset().await;
                            state.session_key = Some(session_key.to_string());
                            break false;
                        }
                        let emitted = accumulator.emitted_model_event;
                        state.reset().await;
                        return WsResult::Failed {
                            message: format!("OpenAI WebSocket Error [{code}]: {message}"),
                            emitted,
                            fallback_allowed: false,
                        };
                    }
                }
            };

            if retried_missing_previous && !terminal_success && state.socket.is_none() {
                continue;
            }

            state.last_used = Some(Instant::now());
            state.in_flight = false;
            if terminal_success {
                state.continuation =
                    accumulator
                        .response_id
                        .clone()
                        .map(|response_id| Continuation {
                            last_request: full_payload.clone(),
                            response_id,
                            response_items: accumulator.canonical_response_items(),
                        });
            } else {
                state.continuation = None;
            }
            if !accumulator.finish(event_tx).await {
                state.reset().await;
                return WsResult::Aborted;
            }
            if !terminal_success {
                state.reset().await;
            }
            return WsResult::Finished;
        }
    }

    async fn stream_sse(
        &self,
        url: &str,
        payload: Value,
        session_key: Option<&str>,
        is_codex: bool,
        event_tx: &mpsc::Sender<StreamEvent>,
    ) {
        let mut request = self
            .client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json");
        if let Some(account_id) = &self.account_id {
            request = request.header("chatgpt-account-id", account_id);
        }
        if is_codex {
            request = request
                .header("OpenAI-Beta", "responses=experimental")
                .header(ACCEPT, "text/event-stream")
                .header("originator", "mypi")
                .header(USER_AGENT, "mypi");
            if let Some(key) = session_key {
                request = request
                    .header("session-id", key)
                    .header("x-client-request-id", key);
            }
        }

        let response = match request.json(&payload).send().await {
            Ok(response) => response,
            Err(error) => {
                let _ = event_tx
                    .send(StreamEvent::Error(format!("HTTP request error: {error}")))
                    .await;
                return;
            }
        };
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let _ = event_tx
                .send(StreamEvent::Error(format!(
                    "OpenAI API error ({status}): {body}"
                )))
                .await;
            return;
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut accumulator = ResponseAccumulator::default();
        let mut terminated = false;
        while let Some(chunk_result) = stream.next().await {
            let chunk: Bytes = match chunk_result {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = event_tx
                        .send(StreamEvent::Error(format!("Stream reading error: {error}")))
                        .await;
                    return;
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(position) = buffer.find("\n\n") {
                let block = buffer[..position].to_string();
                buffer = buffer[position + 2..].to_string();
                for line in block.lines().map(str::trim) {
                    let Some(data) = line.strip_prefix("data: ") else {
                        continue;
                    };
                    if data == "[DONE]" {
                        terminated = true;
                        break;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };
                    match accumulator.process(&value, event_tx).await {
                        ParsedEvent::Continue => {}
                        ParsedEvent::Terminal { .. } => terminated = true,
                        ParsedEvent::ReceiverClosed => return,
                        ParsedEvent::ProviderError { code, message } => {
                            let kind = if value.get("type").and_then(Value::as_str)
                                == Some("response.failed")
                            {
                                "OpenAI Response Failed"
                            } else if value.get("type").and_then(Value::as_str) == Some("error") {
                                "OpenAI SSE Error"
                            } else {
                                "OpenAI API Error"
                            };
                            let _ = event_tx
                                .send(StreamEvent::Error(format!("{kind} [{code}]: {message}")))
                                .await;
                            return;
                        }
                    }
                }
                if terminated {
                    break;
                }
            }
            if terminated {
                break;
            }
        }
        let _ = accumulator.finish(event_tx).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        api_error_details, clamp_prompt_cache_key, continuation_payload, parse_chat_usage,
        parse_responses_text_delta, parse_responses_usage, CodexWsState, Continuation,
        OpenAIClient, ProviderUsage, StreamEvent, OPENAI_PROMPT_CACHE_KEY_MAX_CHARS,
    };
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn continuation() -> Continuation {
        Continuation {
            last_request: json!({
                "model": "codex",
                "input": [{"role":"user","content":"one"}],
                "stream": true,
                "tools": []
            }),
            response_id: "response-1".to_string(),
            response_items: vec![json!({
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":"answer"}]
            })],
        }
    }

    #[test]
    fn prompt_cache_key_clamping_is_unicode_safe() {
        let key = "🦀".repeat(OPENAI_PROMPT_CACHE_KEY_MAX_CHARS + 10);
        let clamped = clamp_prompt_cache_key(&key);
        assert_eq!(clamped.chars().count(), OPENAI_PROMPT_CACHE_KEY_MAX_CHARS);
        assert_eq!(clamped, "🦀".repeat(OPENAI_PROMPT_CACHE_KEY_MAX_CHARS));
        assert_eq!(clamp_prompt_cache_key("session-a"), "session-a");
    }

    #[test]
    fn continuation_sends_only_input_delta() {
        let continuation = continuation();
        let mut input = continuation.last_request["input"]
            .as_array()
            .unwrap()
            .clone();
        input.extend(continuation.response_items.clone());
        input.push(json!({"role":"user","content":"two"}));
        let current = json!({"model":"codex","input":input,"stream":true,"tools":[]});
        let payload = continuation_payload(&current, &continuation).unwrap();
        assert_eq!(payload["previous_response_id"], "response-1");
        assert_eq!(payload["input"], json!([{"role":"user","content":"two"}]));
    }

    #[test]
    fn continuation_after_tool_call_sends_only_tool_output() {
        let mut continuation = continuation();
        continuation.response_items = vec![json!({
            "type": "function_call",
            "call_id": "call-1",
            "name": "read_file",
            "arguments": "{\"path\":\"src/main.rs\"}"
        })];
        let mut input = continuation.last_request["input"]
            .as_array()
            .unwrap()
            .clone();
        input.extend(continuation.response_items.clone());
        input.push(json!({
            "type": "function_call_output",
            "call_id": "call-1",
            "output": "fn main() {}"
        }));

        let payload = continuation_payload(
            &json!({"model":"codex","input":input,"stream":true,"tools":[]}),
            &continuation,
        )
        .unwrap();

        assert_eq!(payload["previous_response_id"], "response-1");
        assert_eq!(
            payload["input"],
            json!([{
                "type": "function_call_output",
                "call_id": "call-1",
                "output": "fn main() {}"
            }])
        );
    }

    #[test]
    fn cloned_clients_share_websocket_state() {
        let client = OpenAIClient::new("key".into(), Some("account".into()));
        let cloned = client.clone();

        assert!(std::sync::Arc::ptr_eq(&client.codex_ws, &cloned.codex_ws));
    }

    #[test]
    fn continuation_ignores_existing_previous_response_id_when_matching() {
        let continuation = continuation();
        let mut input = continuation.last_request["input"]
            .as_array()
            .unwrap()
            .clone();
        input.extend(continuation.response_items.clone());
        let current = json!({
            "model":"codex", "input":input, "stream":true, "tools":[],
            "previous_response_id":"stale"
        });
        assert_eq!(
            continuation_payload(&current, &continuation).unwrap()["previous_response_id"],
            "response-1"
        );
    }

    #[test]
    fn continuation_rejects_field_or_prefix_mismatch() {
        let continuation = continuation();
        let wrong_fields = json!({
            "model":"other", "input":[], "stream":true, "tools":[]
        });
        assert!(continuation_payload(&wrong_fields, &continuation).is_none());
        let wrong_prefix = json!({
            "model":"codex",
            "input":[{"role":"user","content":"different"}],
            "stream":true,
            "tools":[]
        });
        assert!(continuation_payload(&wrong_prefix, &continuation).is_none());
    }

    #[test]
    fn websocket_state_resets_for_session_key_and_expiry() {
        let mut state = CodexWsState::new();
        let now = Instant::now();
        state.session_key = Some("one".to_string());
        state.opened_at = Some(now);
        state.last_used = Some(now);
        assert!(!state.should_reset("one", now));
        assert!(state.should_reset("two", now));
        assert!(state.should_reset("one", now + Duration::from_secs(5 * 60)));
        state.last_used = Some(now + Duration::from_secs(54 * 60));
        assert!(state.should_reset("one", now + Duration::from_secs(55 * 60)));
    }

    #[test]
    fn parses_chat_usage_and_normalizes_uncached_input() {
        let usage = parse_chat_usage(&json!({"usage": {
            "prompt_tokens":1200,"completion_tokens":80,"total_tokens":1280,
            "prompt_tokens_details":{"cached_tokens":900,"cache_write_tokens":100}
        }}));
        assert_eq!(
            usage,
            Some(ProviderUsage {
                input_tokens: 200,
                output_tokens: 80,
                cache_read_tokens: 900,
                cache_write_tokens: 100,
                total_tokens: 1280
            })
        );
    }

    #[test]
    fn parses_responses_usage_and_normalizes_uncached_input() {
        let usage =
            parse_responses_usage(&json!({"type":"response.completed","response":{"usage":{
                "input_tokens":1000,"output_tokens":60,"total_tokens":1060,
                "input_tokens_details":{"cached_tokens":768}
            }}}));
        assert_eq!(
            usage,
            Some(ProviderUsage {
                input_tokens: 232,
                output_tokens: 60,
                cache_read_tokens: 768,
                cache_write_tokens: 0,
                total_tokens: 1060
            })
        );
    }

    #[test]
    fn parses_responses_output_text_delta_as_content() {
        let event = parse_responses_text_delta(&json!({
            "type":"response.output_text.delta","delta":"answer"
        }));
        assert!(matches!(event, Some(StreamEvent::ContentToken(text)) if text == "answer"));
    }

    #[test]
    fn parses_responses_reasoning_deltas_separately_from_content() {
        for event_type in [
            "response.reasoning_summary_text.delta",
            "response.reasoning_text.delta",
        ] {
            let event = parse_responses_text_delta(&json!({"type":event_type,"delta":"thinking"}));
            assert!(matches!(event, Some(StreamEvent::ReasoningToken(text)) if text == "thinking"));
        }
    }

    #[test]
    fn extracts_nested_sse_error_details() {
        let (code, message) = api_error_details(&json!({
            "type":"error","error":{"code":"model_not_found","message":"missing"}
        }));
        assert_eq!(code, "model_not_found");
        assert_eq!(message, "missing");
    }
}
