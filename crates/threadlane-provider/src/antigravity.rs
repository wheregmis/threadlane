use crate::antigravity_auth::{get_valid_antigravity_token, load_antigravity_credentials};
use crate::openai::{ProviderUsage, StreamEvent, ToolCall, ToolCallFunction};
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Mutex, OnceCell};

const PROD_BASE_URL: &str = "https://cloudcode-pa.googleapis.com";
const DAILY_BASE_URL: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";
const STREAM_PATH: &str = "/v1internal:streamGenerateContent?alt=sse";
const CLIENT_VERSION: &str = "1.15.8";

type ProjectCache = Arc<Mutex<HashMap<[u8; 32], Arc<OnceCell<String>>>>>;

#[derive(Debug, Clone)]
pub enum AntigravityStreamEvent {
    ContentDelta(String),
    ThinkingDelta(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    FinishReason(String),
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "functionCall")]
    function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "functionResponse")]
    function_response: Option<GeminiFunctionResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiFunctionCall {
    name: String,
    args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiFunctionResponse {
    name: String,
    response: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiContent {
    pub role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiFunctionDecl {
    pub name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiTool {
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<GeminiFunctionDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiRequest {
    pub contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "systemInstruction")]
    pub system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "generationConfig")]
    generation_config: Option<Value>,
}

/// Compatibility builder retained for callers that already construct Gemini requests.
pub fn build_gemini_request(
    system_prompt: &str,
    messages: &[Value],
    tools: &[Value],
) -> GeminiRequest {
    let mut contents = Vec::new();
    let mut call_names = HashMap::new();

    for msg in messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        let mut parts = Vec::new();

        if role == "tool" {
            let call_id = msg
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = msg
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| call_names.get(call_id).map(String::as_str))
                .unwrap_or("tool")
                .to_string();
            let output = message_text(msg.get("content")).unwrap_or_default();
            parts.push(GeminiPart {
                text: None,
                function_call: None,
                function_response: Some(GeminiFunctionResponse {
                    name,
                    response: json!({ "output": output }),
                }),
            });
        } else {
            if let Some(text) = message_text(msg.get("content")) {
                if !text.trim().is_empty() {
                    parts.push(GeminiPart {
                        text: Some(text),
                        function_call: None,
                        function_response: None,
                    });
                }
            }
            if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let Some(function) = call.get("function") else {
                        continue;
                    };
                    let name = function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        call_names.insert(id.to_string(), name.clone());
                    }
                    parts.push(GeminiPart {
                        text: None,
                        function_call: Some(GeminiFunctionCall {
                            name,
                            args: parse_arguments(function.get("arguments")),
                        }),
                        function_response: None,
                    });
                }
            }
        }

        if !parts.is_empty() {
            contents.push(GeminiContent {
                role: if role == "assistant" || role == "model" {
                    "model".to_string()
                } else {
                    "user".to_string()
                },
                parts,
            });
        }
    }

    let declarations = tools
        .iter()
        .filter_map(tool_definition)
        .filter_map(|tool| {
            Some(GeminiFunctionDecl {
                name: tool.get("name").and_then(Value::as_str)?.to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                parameters: tool.get("parameters")?.clone(),
            })
        })
        .collect::<Vec<_>>();

    GeminiRequest {
        contents,
        tools: (!declarations.is_empty()).then(|| {
            vec![GeminiTool {
                function_declarations: declarations,
            }]
        }),
        system_instruction: (!system_prompt.trim().is_empty()).then(|| GeminiContent {
            role: "user".to_string(),
            parts: vec![GeminiPart {
                text: Some(system_prompt.to_string()),
                function_call: None,
                function_response: None,
            }],
        }),
        generation_config: None,
    }
}

#[derive(Clone)]
pub struct AntigravityClient {
    client: reqwest::Client,
    project_cache: ProjectCache,
}

impl Default for AntigravityClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AntigravityClient {
    pub(crate) fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            project_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Converts an OpenAI chat-completions payload and streams events understood by the
    /// shared agent loop.
    async fn stream_chat_completion(
        &self,
        api_payload: Value,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        if let Err(error) = self
            .stream_chat_completion_inner(api_payload, &event_tx)
            .await
        {
            let _ = event_tx.send(StreamEvent::Error(error)).await;
        }
    }

    async fn stream_chat_completion_inner(
        &self,
        api_payload: Value,
        event_tx: &mpsc::Sender<StreamEvent>,
    ) -> Result<(), String> {
        let token = get_valid_antigravity_token().await?;
        let project = self.resolve_project(&token).await;
        let (runtime_model, request) = convert_openai_payload(&api_payload)?;
        tracing::debug!(
            "Antigravity dispatching request: runtime_model={}, project={}",
            runtime_model,
            project
        );
        let envelope = request_envelope(&project, &runtime_model, request);
        let response = self
            .send_stream_request(&token, &runtime_model, &envelope)
            .await?;
        consume_openai_stream(response, event_tx).await
    }

    async fn resolve_project(&self, token: &str) -> String {
        let credential_key = credential_cache_key(token);
        let project = {
            let mut cache = self.project_cache.lock().await;
            Arc::clone(
                cache
                    .entry(credential_key)
                    .or_insert_with(|| Arc::new(OnceCell::new())),
            )
        };

        project
            .get_or_init(|| self.resolve_project_uncached(token))
            .await
            .clone()
    }

    async fn resolve_project_uncached(&self, token: &str) -> String {
        if let Ok(project) = std::env::var("ANTIGRAVITY_PROJECT_ID") {
            if !project.trim().is_empty() {
                tracing::debug!("Using ANTIGRAVITY_PROJECT_ID env override: {}", project);
                return project;
            }
        }

        let metadata = json!({
            "metadata": {
                "ideType": "ANTIGRAVITY",
                "platform": "PLATFORM_UNSPECIFIED",
                "pluginType": "GEMINI"
            }
        });
        for endpoint in endpoint_candidates() {
            if let Some(project) = self
                .discover_at(&endpoint, "loadCodeAssist", &metadata, token)
                .await
            {
                tracing::debug!(
                    "Antigravity resolved project from {endpoint}/loadCodeAssist: {project}"
                );
                return project;
            }
        }
        for endpoint in endpoint_candidates() {
            if let Some(project) = self
                .discover_at(&endpoint, "listCloudAICompanionProjects", &json!({}), token)
                .await
            {
                tracing::debug!("Antigravity resolved project from {endpoint}/listCloudAICompanionProjects: {project}");
                return project;
            }
        }

        let credentials = load_antigravity_credentials();
        let project = credentials
            .as_ref()
            .and_then(|creds| creds.project_id.clone())
            .filter(|project| !project.trim().is_empty())
            .unwrap_or_else(|| {
                stable_project_id(
                    credentials
                        .as_ref()
                        .and_then(|creds| creds.account_email.as_deref())
                        .unwrap_or("antigravity-default"),
                )
            });
        tracing::debug!("Antigravity falling back to credential/default project: {project}");
        project
    }

    async fn discover_at(
        &self,
        endpoint: &str,
        method: &str,
        body: &Value,
        token: &str,
    ) -> Option<String> {
        let url = format!("{endpoint}/v1internal:{method}");
        let response = self
            .client
            .post(url)
            .headers(antigravity_headers(token))
            .json(body)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            tracing::debug!(
                "Antigravity project discovery failed at {endpoint}:{method} with status {}",
                response.status()
            );
            return None;
        }
        let value = response.json::<Value>().await.ok()?;
        extract_project_id(&value)
    }

    async fn send_stream_request(
        &self,
        token: &str,
        runtime_model: &str,
        envelope: &Value,
    ) -> Result<reqwest::Response, String> {
        let mut last_error = "no Antigravity endpoint was available".to_string();
        let mut quota_error = None;
        for endpoint in endpoint_candidates() {
            let mut headers = antigravity_headers(token);
            if runtime_model.starts_with("claude-") {
                headers.insert(
                    "anthropic-beta",
                    reqwest::header::HeaderValue::from_static("interleaved-thinking-2025-05-14"),
                );
            }
            tracing::debug!(
                "Sending Antigravity stream request to {endpoint}{STREAM_PATH} (runtime_model={runtime_model})"
            );
            let response = self
                .client
                .post(format!("{endpoint}{STREAM_PATH}"))
                .headers(headers)
                .json(envelope)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    tracing::warn!("Request to Antigravity endpoint {endpoint} failed: {error}");
                    last_error = format!("request to Antigravity failed: {error}");
                    continue;
                }
            };
            if response.status().is_success() {
                tracing::debug!("Antigravity endpoint {endpoint} returned HTTP 200");
                return Ok(response);
            }
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            tracing::error!(
                "Antigravity endpoint {endpoint} returned error {status} for runtime model {runtime_model}: {text}"
            );
            last_error = format!(
                "Antigravity API error ({status}, runtime model {runtime_model}): {}",
                safe_error_text(&text)
            );
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && quota_error.is_none() {
                quota_error = Some(last_error.clone());
            }
            if !matches!(status.as_u16(), 403 | 404 | 429 | 500 | 502 | 503 | 504) {
                break;
            }
        }
        Err(quota_error.unwrap_or(last_error))
    }

    pub async fn run_diagnostics(&self) -> String {
        let mut report = vec!["=== Antigravity Doctor Diagnostics ===".to_string()];
        let Some(credentials) = load_antigravity_credentials() else {
            report.push("No stored Antigravity credentials found.".to_string());
            report.push("Run /login antigravity to authenticate.".to_string());
            return report.join("\n");
        };
        report.push("Credentials file found.".to_string());
        let now = unix_seconds();
        if credentials.expires_at > now {
            report.push(format!(
                "Access token is valid for {} seconds.",
                credentials.expires_at - now
            ));
        } else {
            report.push("Access token is expired and will be refreshed on request.".to_string());
        }
        report.push(if credentials.refresh_token.is_some() {
            "Refresh token is present.".to_string()
        } else {
            "Refresh token is missing; re-login is recommended.".to_string()
        });

        match get_valid_antigravity_token().await {
            Ok(token) => {
                report.push("Successfully retrieved a valid access token.".to_string());
                let project = self.resolve_project(&token).await;
                report.push(if project.is_empty() {
                    "Project discovery did not return a usable project.".to_string()
                } else {
                    "Project resolution succeeded.".to_string()
                });
            }
            Err(error) => report.push(format!("Failed to get a valid access token: {error}")),
        }
        report.push("Diagnostics complete.".to_string());
        report.join("\n")
    }
}

#[async_trait::async_trait]
impl crate::traits::ModelProvider for AntigravityClient {
    fn provider_id(&self) -> &'static str {
        "antigravity"
    }

    fn supports_model(&self, model: &str) -> bool {
        crate::router::is_antigravity_model(model)
    }

    async fn stream_chat_completion(
        &self,
        payload_source: crate::router::PayloadSource,
        _prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let payload = payload_source
            .resolve(crate::router::PayloadFormat::ChatCompletions)
            .await;
        self.stream_chat_completion(payload, event_tx).await;
    }
}

fn credential_cache_key(token: &str) -> [u8; 32] {
    let credentials = load_antigravity_credentials();
    let mut hasher = Sha256::new();

    if let Some(refresh_token) = credentials
        .as_ref()
        .and_then(|credentials| credentials.refresh_token.as_deref())
    {
        hasher.update(b"refresh-token\0");
        hasher.update(refresh_token.as_bytes());
    } else if let Some(account_email) = credentials
        .as_ref()
        .and_then(|credentials| credentials.account_email.as_deref())
    {
        hasher.update(b"account-email\0");
        hasher.update(account_email.as_bytes());
    } else {
        hasher.update(b"access-token\0");
        hasher.update(token.as_bytes());
    }

    if let Some(project_id) = credentials
        .as_ref()
        .and_then(|credentials| credentials.project_id.as_deref())
    {
        hasher.update(b"\0project-id\0");
        hasher.update(project_id.as_bytes());
    }

    hasher.finalize().into()
}

fn endpoint_candidates() -> Vec<String> {
    if let Ok(raw) = std::env::var("ANTIGRAVITY_BASE_URL") {
        if let Ok(url) = url::Url::parse(raw.trim()) {
            let allowed = url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.host_str().is_some_and(|host| {
                    host == "googleapis.com" || host.ends_with(".googleapis.com")
                });
            if allowed {
                return vec![raw.trim_end_matches('/').to_string()];
            }
        }
    }
    vec![PROD_BASE_URL.to_string(), DAILY_BASE_URL.to_string()]
}

fn antigravity_headers(token: &str) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};

    let platform = if cfg!(target_os = "macos") {
        "MACOS"
    } else if cfg!(target_os = "windows") {
        "WINDOWS"
    } else {
        "LINUX"
    };
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else {
        std::env::consts::ARCH
    };
    let user_agent = std::env::var("ANTIGRAVITY_USER_AGENT")
        .unwrap_or_else(|_| format!("antigravity/{CLIENT_VERSION} {os}/{arch}"));

    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
        headers.insert(AUTHORIZATION, value);
    }
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    headers.insert(
        "X-Goog-Api-Client",
        HeaderValue::from_static("google-cloud-sdk vscode_cloudshelleditor/0.1"),
    );
    if let Ok(value) = HeaderValue::from_str(&user_agent) {
        headers.insert(USER_AGENT, value);
    }
    if let Ok(value) = HeaderValue::from_str(
        &json!({
            "ideType": "ANTIGRAVITY",
            "platform": platform,
            "pluginType": "GEMINI"
        })
        .to_string(),
    ) {
        headers.insert("Client-Metadata", value);
    }
    headers
}

fn resolve_runtime_model(model_id: &str, effort: &str) -> String {
    if let Ok(model) = std::env::var("ANTIGRAVITY_RUNTIME_MODEL") {
        if !model.trim().is_empty() {
            return model;
        }
    }
    let model = model_id.strip_prefix("antigravity/").unwrap_or(model_id);
    match model {
        "gemini-3.7-flash" => match effort {
            "medium" => "gemini-3.7-flash-medium",
            "high" | "xhigh" => "gemini-3.7-flash-high",
            _ => "gemini-3.7-flash-low",
        },
        "gemini-3.6-flash" => match effort {
            "medium" => "gemini-3.6-flash-medium",
            "high" | "xhigh" => "gemini-3.6-flash-high",
            _ => "gemini-3.6-flash-low",
        },
        "gemini-3.5-flash" => match effort {
            "low" | "medium" => "gemini-3.5-flash-low",
            "high" | "xhigh" => "gemini-3-flash-agent",
            _ => "gemini-3.5-flash-extra-low",
        },
        "gemini-3.1-pro" => match effort {
            "high" | "xhigh" => "gemini-pro-agent",
            _ => "gemini-3.1-pro-low",
        },
        "claude-opus-4-6" => "claude-opus-4-6-thinking",
        "claude-sonnet-4-6" => "claude-sonnet-4-6",
        "gpt-oss-120b" => "gpt-oss-120b-medium",
        other => other,
    }
    .to_string()
}

fn convert_openai_payload(payload: &Value) -> Result<(String, Value), String> {
    let model_id = payload
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "Antigravity request is missing a model".to_string())?;
    let effort = payload
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .unwrap_or("off");
    let runtime_model = resolve_runtime_model(model_id, effort);
    let needs_legacy_schema =
        runtime_model.starts_with("claude-") || runtime_model.starts_with("gpt-oss-");
    let needs_call_ids = needs_legacy_schema;
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "Antigravity request is missing messages".to_string())?;

    let mut system_parts = Vec::new();
    let mut contents = Vec::new();
    let mut call_names = HashMap::new();
    let mut unreplayable_call_ids = HashSet::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        if role == "system" || role == "developer" {
            if let Some(text) = message_text(message.get("content")) {
                if !text.trim().is_empty() {
                    system_parts.push(json!({ "text": text }));
                }
            }
            continue;
        }

        let mut parts = if role == "tool" {
            let call_id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if unreplayable_call_ids.contains(call_id) {
                continue;
            }
            let name = message
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| call_names.get(call_id).map(String::as_str))
                .unwrap_or("tool");
            let mut response = json!({
                "name": name,
                "response": { "output": message_text(message.get("content")).unwrap_or_default() }
            });
            if needs_call_ids && !call_id.is_empty() {
                response["id"] = Value::String(sanitize_call_id(call_id, name));
            }
            vec![json!({ "functionResponse": response })]
        } else {
            content_parts(message.get("content"))
        };

        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let Some(function) = call.get("function") else {
                    continue;
                };
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                let thought_signature = call
                    .get("thoughtSignature")
                    .or_else(|| call.get("thought_signature"))
                    .and_then(Value::as_str);
                if runtime_model.starts_with("gemini-3.6-") && thought_signature.is_none() {
                    if !id.is_empty() {
                        unreplayable_call_ids.insert(id.to_string());
                    }
                    continue;
                }
                if !id.is_empty() {
                    call_names.insert(id.to_string(), name.to_string());
                }
                let mut function_call = json!({
                    "name": name,
                    "args": parse_arguments(function.get("arguments"))
                });
                if needs_call_ids && !id.is_empty() {
                    function_call["id"] = Value::String(sanitize_call_id(id, name));
                }
                let mut part = json!({ "functionCall": function_call });
                if let Some(signature) = thought_signature {
                    part["thoughtSignature"] = Value::String(signature.to_string());
                }
                parts.push(part);
            }
        }
        if !parts.is_empty() {
            contents.push(json!({
                "role": if role == "assistant" { "model" } else { "user" },
                "parts": parts
            }));
        }
    }

    let mut request = Map::new();
    request.insert("contents".to_string(), Value::Array(contents));
    if system_parts.is_empty() {
        system_parts.push(json!({
            "text": "You are Antigravity, a practical, tool-aware coding assistant."
        }));
    }
    request.insert(
        "systemInstruction".to_string(),
        json!({ "role": "user", "parts": system_parts }),
    );

    let mut generation = Map::new();
    if let Some(value) = payload.get("temperature").and_then(Value::as_f64) {
        generation.insert("temperature".to_string(), json!(value));
    }
    let max_output_tokens = payload
        .get("max_completion_tokens")
        .or_else(|| payload.get("max_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(8192);
    generation.insert("maxOutputTokens".to_string(), json!(max_output_tokens));
    request.insert("generationConfig".to_string(), Value::Object(generation));

    if let Some(tools) = payload.get("tools").and_then(Value::as_array) {
        let declarations = tools
            .iter()
            .filter_map(tool_definition)
            .map(|tool| {
                let mut declaration = Map::new();
                declaration.insert("name".to_string(), tool["name"].clone());
                declaration.insert(
                    "description".to_string(),
                    tool.get("description")
                        .cloned()
                        .unwrap_or_else(|| Value::String(String::new())),
                );
                let schema = strip_meta_schema(
                    tool.get("parameters")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                );
                if needs_legacy_schema {
                    declaration.insert("parameters".to_string(), sanitize_custom_schema(schema));
                } else {
                    declaration.insert("parametersJsonSchema".to_string(), schema);
                }
                Value::Object(declaration)
            })
            .collect::<Vec<_>>();
        if !declarations.is_empty() {
            request.insert(
                "tools".to_string(),
                json!([{ "functionDeclarations": declarations }]),
            );
            if let Some(mode) = tool_choice_mode(payload.get("tool_choice"), needs_legacy_schema) {
                request.insert(
                    "toolConfig".to_string(),
                    json!({ "functionCallingConfig": { "mode": mode } }),
                );
            }
        }
    }
    if let Some(session) = payload
        .get("session_id")
        .or_else(|| payload.get("conversation_id"))
        .and_then(Value::as_str)
    {
        request.insert("sessionId".to_string(), Value::String(session.to_string()));
    }

    Ok((runtime_model, Value::Object(request)))
}

fn request_envelope(project: &str, runtime_model: &str, request: Value) -> Value {
    json!({
        "project": project,
        "model": runtime_model,
        "request": request,
        "requestType": "agent",
        "userAgent": "antigravity",
        "requestId": format!("threadlane-{}", rand_id())
    })
}

fn message_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(|item| {
                    let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
                    matches!(kind, "text" | "input_text" | "output_text")
                        .then(|| item.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        value if !value.is_null() => Some(value.to_string()),
        _ => None,
    }
}

fn content_parts(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) if !text.is_empty() => vec![json!({ "text": text })],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
                if matches!(kind, "text" | "input_text" | "output_text") {
                    return item
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| json!({ "text": text }));
                }
                if matches!(kind, "image_url" | "input_image") {
                    let url = item
                        .get("image_url")
                        .and_then(|value| value.get("url").or(Some(value)))
                        .and_then(Value::as_str)?;
                    let (metadata, data) = url.strip_prefix("data:")?.split_once(',')?;
                    let mime_type = metadata.split(';').next().unwrap_or("image/png");
                    return Some(json!({
                        "inlineData": { "mimeType": mime_type, "data": data }
                    }));
                }
                None
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(arguments)) => {
            serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
        }
        Some(value) if value.is_object() => value.clone(),
        _ => json!({}),
    }
}

fn tool_definition(tool: &Value) -> Option<&Value> {
    tool.get("function")
        .filter(|_| tool.get("type").and_then(Value::as_str) == Some("function"))
        .or(Some(tool))
        .filter(|definition| definition.get("name").and_then(Value::as_str).is_some())
}

fn strip_meta_schema(schema: Value) -> Value {
    const OMIT: &[&str] = &[
        "$schema",
        "$id",
        "$anchor",
        "$dynamicAnchor",
        "$vocabulary",
        "$comment",
        "$defs",
        "definitions",
    ];
    map_schema(schema, &|key, _| !OMIT.contains(&key))
}

fn sanitize_custom_schema(schema: Value) -> Value {
    const OMIT: &[&str] = &[
        "anyOf",
        "oneOf",
        "allOf",
        "not",
        "if",
        "then",
        "else",
        "$schema",
        "$id",
        "$anchor",
        "$dynamicAnchor",
        "$vocabulary",
        "$comment",
        "$defs",
        "$ref",
        "$dynamicRef",
        "definitions",
        "patternProperties",
        "additionalProperties",
        "unevaluatedProperties",
        "propertyNames",
        "dependentSchemas",
        "dependentRequired",
        "prefixItems",
        "unevaluatedItems",
        "contains",
        "minContains",
        "maxContains",
        "uniqueItems",
        "const",
        "default",
        "examples",
        "example",
        "title",
        "readOnly",
        "writeOnly",
        "deprecated",
        "contentMediaType",
        "contentEncoding",
        "contentSchema",
        "maxLength",
        "minLength",
        "maxItems",
        "minItems",
        "maximum",
        "minimum",
        "exclusiveMaximum",
        "exclusiveMinimum",
        "multipleOf",
        "maxProperties",
        "minProperties",
        "pattern",
        "format",
    ];
    map_schema(schema, &|key, value| {
        !key.starts_with('$')
            && !OMIT.contains(&key)
            && !(key == "enum"
                && value
                    .as_array()
                    .is_some_and(|items| !items.iter().all(Value::is_string)))
    })
}

fn map_schema(schema: Value, keep: &impl Fn(&str, &Value) -> bool) -> Value {
    match schema {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| map_schema(item, keep))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter(|(key, value)| keep(key, value))
                .map(|(key, value)| (key, map_schema(value, keep)))
                .collect(),
        ),
        other => other,
    }
}

fn tool_choice_mode(choice: Option<&Value>, claude_default: bool) -> Option<&'static str> {
    match choice {
        Some(Value::String(value)) if value == "none" => Some("NONE"),
        Some(Value::String(value)) if value == "required" || value == "any" => Some("ANY"),
        Some(_) => Some("AUTO"),
        None if claude_default => Some("VALIDATED"),
        None => None,
    }
}

#[derive(Debug, Default)]
struct ParsedChunk {
    content: Vec<String>,
    reasoning: Vec<String>,
    tool_calls: Vec<ToolCall>,
    usage: Option<ProviderUsage>,
    finish_reason: Option<String>,
    error: Option<String>,
}

fn parse_stream_value(value: &Value) -> ParsedChunk {
    let mut parsed = ParsedChunk::default();
    if let Some(error) = value.get("error") {
        parsed.error = Some(
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| error.as_str().unwrap_or("Antigravity stream error"))
                .to_string(),
        );
        return parsed;
    }
    let response = value.get("response").unwrap_or(value);
    if let Some(error) = response.get("error") {
        parsed.error = Some(
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| error.as_str().unwrap_or("Antigravity stream error"))
                .to_string(),
        );
        return parsed;
    }
    if let Some(candidates) = response.get("candidates").and_then(Value::as_array) {
        for candidate in candidates {
            if let Some(parts) = candidate
                .get("content")
                .and_then(|content| content.get("parts"))
                .and_then(Value::as_array)
            {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                                parsed.reasoning.push(text.to_string());
                            } else {
                                parsed.content.push(text.to_string());
                            }
                        }
                    }
                    if let Some(thought) = part.get("thought").and_then(Value::as_str) {
                        if !thought.is_empty() {
                            parsed.reasoning.push(thought.to_string());
                        }
                    }
                    if let Some(call) = part.get("functionCall") {
                        let name = call
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .map(|id| sanitize_call_id(id, &name))
                            .unwrap_or_else(|| format!("call_{}", rand_id()));
                        let arguments = call
                            .get("args")
                            .cloned()
                            .unwrap_or_else(|| json!({}))
                            .to_string();
                        parsed.tool_calls.push(ToolCall {
                            id,
                            r#type: "function".to_string(),
                            function: ToolCallFunction { name, arguments },
                            thought_signature: part
                                .get("thoughtSignature")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        });
                    }
                }
            }
            if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
                parsed.finish_reason = Some(reason.to_string());
            }
        }
    }
    if let Some(usage) = response.get("usageMetadata") {
        let prompt = token_count(usage.get("promptTokenCount"));
        let cache = token_count(usage.get("cachedContentTokenCount"));
        let output = token_count(usage.get("candidatesTokenCount"))
            .saturating_add(token_count(usage.get("thoughtsTokenCount")));
        let total = token_count(usage.get("totalTokenCount"));
        parsed.usage = Some(ProviderUsage {
            input_tokens: prompt.saturating_sub(cache),
            output_tokens: output,
            cache_read_tokens: cache,
            cache_write_tokens: 0,
            total_tokens: if total == 0 {
                prompt.saturating_add(output)
            } else {
                total
            },
        });
    }
    parsed
}

async fn consume_openai_stream(
    response: reqwest::Response,
    event_tx: &mpsc::Sender<StreamEvent>,
) -> Result<(), String> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut tool_calls = Vec::new();
    let mut usage = ProviderUsage::default();
    let mut finished = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Antigravity stream error: {error}"))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        let mut start = 0;
        while let Some(rel_newline) = buffer[start..].find('\n') {
            let newline = start + rel_newline;
            let line = buffer[start..newline].trim_end_matches('\r');
            start = newline + 1;
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                finished = true;
                break;
            }
            let Ok(value) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let parsed = parse_stream_value(&value);
            if let Some(error) = parsed.error {
                return Err(safe_error_text(&error));
            }
            for text in parsed.content {
                if event_tx
                    .send(StreamEvent::ContentToken(text))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            for thought in parsed.reasoning {
                if event_tx
                    .send(StreamEvent::ReasoningToken(thought))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            for call in parsed.tool_calls {
                if event_tx
                    .send(StreamEvent::ToolCallStart {
                        name: call.function.name.clone(),
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                if event_tx
                    .send(StreamEvent::ToolCallArgsDelta {
                        args_chunk: call.function.arguments.clone(),
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
                tool_calls.push(call);
            }
            if let Some(new_usage) = parsed.usage {
                usage = new_usage;
            }
        }
        if finished {
            break;
        }
        if start > 0 {
            buffer.drain(..start);
        }
    }

    let _ = event_tx
        .send(StreamEvent::Finished { tool_calls, usage })
        .await;
    Ok(())
}

fn extract_project_id(value: &Value) -> Option<String> {
    if let Some(object) = value.as_object() {
        for key in [
            "antigravityProjectId",
            "projectId",
            "backendProjectId",
            "userDefinedCloudaicompanionProject",
            "cloudaicompanionProject",
            "project",
        ] {
            if let Some(candidate) = object.get(key) {
                if let Some(id) = candidate.as_str() {
                    if !id.is_empty() {
                        return Some(id.to_string());
                    }
                }
                if let Some(id) = candidate.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        return Some(id.to_string());
                    }
                }
            }
        }
        for key in ["projects", "projectIds", "cloudaicompanionProjects"] {
            if let Some(items) = object.get(key).and_then(Value::as_array) {
                for item in items {
                    if let Some(id) = item
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| extract_project_id(item))
                    {
                        return Some(id);
                    }
                }
            }
        }
    }
    None
}

fn sanitize_call_id(id: &str, name: &str) -> String {
    let cleaned = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>();
    if cleaned.is_empty() {
        format!("{}_{}", name, rand_id())
    } else {
        cleaned
    }
}

fn stable_project_id(seed: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in format!("antigravity:{seed}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let second = hash.rotate_left(29) ^ 0x5a17_c9e3_d421_b806;
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&hash.to_be_bytes());
    bytes[8..].copy_from_slice(&second.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn token_count(value: Option<&Value>) -> u32 {
    value
        .and_then(Value::as_u64)
        .unwrap_or_default()
        .min(u64::from(u32::MAX)) as u32
}

fn safe_error_text(text: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(error) = value.get("error") {
            let mut parts = Vec::new();
            if let Some(msg) = error.get("message").and_then(Value::as_str) {
                parts.push(msg.to_string());
            }
            if let Some(status) = error.get("status").and_then(Value::as_str) {
                if !parts.iter().any(|p| p.contains(status)) {
                    parts.push(format!("status: {status}"));
                }
            }
            if let Some(details) = error.get("details").and_then(Value::as_array) {
                for detail in details {
                    if let Some(violations) =
                        detail.get("fieldViolations").and_then(Value::as_array)
                    {
                        for v in violations {
                            let field = v.get("field").and_then(Value::as_str).unwrap_or("");
                            let desc = v.get("description").and_then(Value::as_str).unwrap_or("");
                            if !field.is_empty() || !desc.is_empty() {
                                parts.push(format!("field '{field}': {desc}"));
                            }
                        }
                    } else if let Some(msg) = detail
                        .get("description")
                        .or_else(|| detail.get("message"))
                        .and_then(Value::as_str)
                    {
                        parts.push(msg.to_string());
                    }
                }
            }
            if !parts.is_empty() {
                let joined = parts.join(" | ");
                return joined.chars().take(2000).collect();
            }
            return error.to_string().chars().take(2000).collect();
        }
    }
    text.chars().take(2000).collect()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn rand_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_public_models_and_reasoning_effort() {
        assert_eq!(
            resolve_runtime_model("antigravity/gemini-3.7-flash", "medium"),
            "gemini-3.7-flash-medium"
        );
        assert_eq!(
            resolve_runtime_model("antigravity/gemini-3.7-flash", "high"),
            "gemini-3.7-flash-high"
        );
        assert_eq!(
            resolve_runtime_model("antigravity/gemini-3.6-flash", "medium"),
            "gemini-3.6-flash-medium"
        );
        assert_eq!(
            resolve_runtime_model("gemini-3.5-flash", "high"),
            "gemini-3-flash-agent"
        );
        assert_eq!(
            resolve_runtime_model("gemini-3.1-pro", "high"),
            "gemini-pro-agent"
        );
        assert_eq!(
            resolve_runtime_model("claude-opus-4-6", "high"),
            "claude-opus-4-6-thinking"
        );
    }

    #[test]
    fn converts_openai_payload_and_builds_envelope() {
        let payload = json!({
            "model": "antigravity/gemini-3.6-flash",
            "reasoning_effort": "high",
            "messages": [
                { "role": "system", "content": "Be precise" },
                { "role": "user", "content": "Inspect this" },
                { "role": "assistant", "content": null, "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"a.rs\"}" },
                    "thoughtSignature": "signature-1"
                }]},
                { "role": "tool", "tool_call_id": "call-1", "content": "contents" }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read a file",
                    "parameters": { "$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object" }
                }
            }]
        });
        let (model, request) = convert_openai_payload(&payload).unwrap();
        assert_eq!(model, "gemini-3.6-flash-high");
        assert_eq!(
            request["systemInstruction"]["parts"][0]["text"],
            "Be precise"
        );
        assert_eq!(
            request["contents"][1]["parts"][0]["functionCall"]["name"],
            "read_file"
        );
        assert_eq!(
            request["contents"][1]["parts"][0]["thoughtSignature"],
            "signature-1"
        );
        assert_eq!(
            request["contents"][2]["parts"][0]["functionResponse"]["name"],
            "read_file"
        );
        assert!(request["tools"][0]["functionDeclarations"][0]
            .get("parametersJsonSchema")
            .is_some());
        assert!(
            request["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]
                .get("$schema")
                .is_none()
        );

        let envelope = request_envelope("project-1", &model, request);
        assert_eq!(envelope["project"], "project-1");
        assert_eq!(envelope["model"], "gemini-3.6-flash-high");
        assert_eq!(envelope["requestType"], "agent");
        assert_eq!(envelope["userAgent"], "antigravity");
    }

    #[test]
    fn drops_legacy_gemini_tool_turns_without_required_signatures() {
        let payload = json!({
            "model": "antigravity/gemini-3.6-flash",
            "reasoning_effort": "medium",
            "messages": [
                { "role": "assistant", "tool_calls": [{
                    "id": "legacy-call",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{}" }
                }]},
                {
                    "role": "tool",
                    "tool_call_id": "legacy-call",
                    "name": "read_file",
                    "content": "legacy output"
                },
                { "role": "user", "content": "Continue" }
            ]
        });

        let (_, request) = convert_openai_payload(&payload).unwrap();

        assert_eq!(request["contents"].as_array().unwrap().len(), 1);
        assert_eq!(request["contents"][0]["parts"][0]["text"], "Continue");
    }

    #[test]
    fn claude_tools_use_sanitized_legacy_parameters() {
        let payload = json!({
            "model": "claude-sonnet-4-6",
            "reasoning_effort": "high",
            "messages": [{ "role": "user", "content": "go" }],
            "tools": [{ "type": "function", "function": {
                "name": "run",
                "description": "Run",
                "parameters": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "mode": { "type": "string", "default": "safe" } }
                }
            }}]
        });
        let (_, request) = convert_openai_payload(&payload).unwrap();
        let declaration = &request["tools"][0]["functionDeclarations"][0];
        assert!(declaration.get("parametersJsonSchema").is_none());
        assert!(declaration["parameters"]
            .get("additionalProperties")
            .is_none());
        assert!(declaration["parameters"]["properties"]["mode"]
            .get("default")
            .is_none());
        assert_eq!(
            request["toolConfig"]["functionCallingConfig"]["mode"],
            "VALIDATED"
        );
    }

    #[test]
    fn parses_wrapped_sse_payload() {
        let value = json!({
            "response": {
                "candidates": [{
                    "content": { "parts": [
                        { "thought": true, "text": "reason" },
                        { "text": "answer" },
                        {
                            "functionCall": { "id": "call:1", "name": "read_file", "args": { "path": "a.rs" } },
                            "thoughtSignature": "signature-1"
                        }
                    ]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 20,
                    "cachedContentTokenCount": 5,
                    "candidatesTokenCount": 7,
                    "thoughtsTokenCount": 3,
                    "totalTokenCount": 30
                }
            }
        });
        let parsed = parse_stream_value(&value);
        assert_eq!(parsed.reasoning, vec!["reason"]);
        assert_eq!(parsed.content, vec!["answer"]);
        assert_eq!(parsed.tool_calls[0].id, "call_1");
        assert_eq!(parsed.tool_calls[0].function.name, "read_file");
        assert_eq!(
            parsed.tool_calls[0].thought_signature.as_deref(),
            Some("signature-1")
        );
        assert_eq!(parsed.finish_reason.as_deref(), Some("STOP"));
        assert_eq!(
            parsed.usage,
            Some(ProviderUsage {
                input_tokens: 15,
                output_tokens: 10,
                cache_read_tokens: 5,
                cache_write_tokens: 0,
                total_tokens: 30,
            })
        );
    }
}
