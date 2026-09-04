use crate::antigravity::AntigravityClient;
use crate::openai::OpenAIClient;
use crate::opencode::OpenCodeGoClient;
use crate::title_generator::{title_payload, TITLE_REQUEST_TIMEOUT};
use crate::traits::ModelProvider;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;
use threadlane_protocol::{
    DeferredResponse as RuntimeDeferredResponse, ProviderPort, RuntimeRequest,
    RuntimeStreamEvent as StreamEvent,
};
use tokio::sync::mpsc;

const ANTIGRAVITY_MODEL_PREFIX: &str = "antigravity/";
const OPENCODE_MODEL_PREFIX: &str = "opencode-go/";

pub fn is_antigravity_model(model: &str) -> bool {
    model.starts_with(ANTIGRAVITY_MODEL_PREFIX)
}

pub fn is_opencode_model(model: &str) -> bool {
    model.starts_with(OPENCODE_MODEL_PREFIX)
}

fn is_quota_or_rate_limit(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("429")
        || error.contains("rate limit")
        || error.contains("rate_limit")
        || error.contains("quota")
        || error.contains("too many requests")
        || error.contains("resource_exhausted")
        || error.contains("resource has been exhausted")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    ChatCompletions,
    Codex,
}

pub type LazyPayloadBuilder = Arc<dyn Fn(PayloadFormat) -> BoxFuture<'static, Value> + Send + Sync>;

#[derive(Clone)]
pub enum PayloadSource {
    Eager {
        chat_payload: Value,
        codex_payload: Value,
    },
    ChatCompletions(Value),
    Codex(Value),
    Lazy {
        model: String,
        builder: LazyPayloadBuilder,
    },
}

impl PayloadSource {
    fn lazy<F>(model: impl Into<String>, builder: F) -> Self
    where
        F: Fn(PayloadFormat) -> BoxFuture<'static, Value> + Send + Sync + 'static,
    {
        Self::Lazy {
            model: model.into(),
            builder: Arc::new(builder),
        }
    }

    fn model(&self) -> &str {
        match self {
            PayloadSource::Eager { chat_payload, .. } => chat_payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            PayloadSource::ChatCompletions(payload) | PayloadSource::Codex(payload) => payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            PayloadSource::Lazy { model, .. } => model.as_str(),
        }
    }

    pub(crate) async fn resolve(self, format: PayloadFormat) -> Value {
        match self {
            PayloadSource::Eager {
                chat_payload,
                codex_payload,
            } => match format {
                PayloadFormat::ChatCompletions => chat_payload,
                PayloadFormat::Codex => codex_payload,
            },
            PayloadSource::ChatCompletions(payload) | PayloadSource::Codex(payload) => payload,
            PayloadSource::Lazy { builder, .. } => builder(format).await,
        }
    }
}

impl From<(Value, Value)> for PayloadSource {
    fn from((chat_payload, codex_payload): (Value, Value)) -> Self {
        Self::Eager {
            chat_payload,
            codex_payload,
        }
    }
}

/// Build a complete provider-native object for a runtime request. The runtime
/// deliberately supplies transport-neutral message and tool arrays; the
/// router owns their envelope so Codex never receives a bare JSON array.
fn runtime_request_payload_source(request: &RuntimeRequest) -> PayloadSource {
    let model = request.model.clone();
    let messages = request.messages.clone();
    let tools = request.tools.clone();
    let reasoning_effort = request.reasoning_effort.clone();
    PayloadSource::lazy(model.clone(), move |format| {
        let model = model.clone();
        let messages = messages.clone();
        let tools = tools.clone();
        let reasoning_effort = reasoning_effort.clone();
        Box::pin(async move {
            match format {
                PayloadFormat::ChatCompletions => {
                    let agent_messages: Vec<threadlane_runtime::AgentMessage> =
                        serde_json::from_value(messages).unwrap_or_default();
                    let chat_messages = threadlane_runtime::convert_to_llm(&agent_messages);
                    let mut payload = serde_json::json!({
                        "model": model,
                        "messages": chat_messages,
                        "tools": tools,
                        "stream": true,
                        "stream_options": { "include_usage": true },
                    });
                    if let Some(effort) = reasoning_effort {
                        payload["reasoning_effort"] = effort.into();
                    }
                    payload
                }
                PayloadFormat::Codex => {
                    let agent_messages: Vec<threadlane_runtime::AgentMessage> =
                        serde_json::from_value(messages).unwrap_or_default();
                    let codex_tools = tools
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|tool| {
                            threadlane_runtime::types::AgentToolDefinition::from_provider_schema(
                                tool,
                            )
                            .ok()
                        })
                        .map(|tool| tool.to_codex_responses_tool())
                        .collect::<Vec<_>>();
                    let (instructions, input) =
                        threadlane_runtime::convert_to_codex_llm(&agent_messages);
                    let mut payload = serde_json::json!({
                        "model": model,
                        "instructions": instructions,
                        "input": input,
                        "tools": codex_tools,
                        "store": false,
                        "stream": true,
                    });
                    if let Some(effort) = reasoning_effort {
                        payload["reasoning"] = serde_json::json!({
                            "effort": effort,
                            "summary": "auto",
                        });
                    }
                    payload
                }
            }
        })
    })
}

#[derive(Clone)]
pub struct ProviderClient {
    openai: OpenAIClient,
    openai_fallbacks: Vec<OpenAIClient>,
    antigravity: AntigravityClient,
    opencode: OpenCodeGoClient,
}

#[async_trait::async_trait]
impl ProviderPort for ProviderClient {
    async fn stream_request(&self, request: RuntimeRequest, events: mpsc::Sender<StreamEvent>) {
        let payload = runtime_request_payload_source(&request);
        self.stream_chat_completion(payload, request.prompt_cache_key, events)
            .await;
    }

    async fn fetch_deferred(
        &self,
        model: &str,
        handle_id: &str,
    ) -> Result<RuntimeDeferredResponse, String> {
        self.fetch_deferred(model, handle_id)
            .await
            .map(|response| match response {
                crate::traits::DeferredResponse::Pending => RuntimeDeferredResponse::Pending,
                crate::traits::DeferredResponse::Ready { content } => {
                    RuntimeDeferredResponse::Ready { content }
                }
                crate::traits::DeferredResponse::Error { message } => {
                    RuntimeDeferredResponse::Error { message }
                }
            })
    }

    async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String> {
        self.cancel_deferred(model, handle_id).await
    }

    fn provider_kind(&self, model: &str) -> &'static str {
        self.provider_kind(model)
    }
}

impl ProviderClient {
    pub fn new(api_key: impl Into<String>, account_id: Option<String>) -> Self {
        let api_key = api_key.into();
        let backups = threadlane_auth::openai_auth::get_backup_codex_accounts();
        let openai_fallbacks: Vec<OpenAIClient> = backups
            .into_iter()
            .filter(|backup| backup.access_token != api_key)
            .map(|backup| OpenAIClient::new(backup.access_token, backup.account_id))
            .collect();

        Self {
            openai: OpenAIClient::new(api_key, account_id),
            openai_fallbacks,
            antigravity: AntigravityClient::new(),
            opencode: OpenCodeGoClient::new(),
        }
    }

    #[cfg(test)]
    fn with_fallback_account(
        api_key: impl Into<String>,
        account_id: Option<String>,
        fallback_api_key: impl Into<String>,
        fallback_account_id: Option<String>,
    ) -> Self {
        Self {
            openai: OpenAIClient::new(api_key.into(), account_id),
            openai_fallbacks: vec![OpenAIClient::new(
                fallback_api_key.into(),
                fallback_account_id,
            )],
            antigravity: AntigravityClient::new(),
            opencode: OpenCodeGoClient::new(),
        }
    }

    #[cfg(test)]
    fn with_fallback_accounts(
        api_key: impl Into<String>,
        account_id: Option<String>,
        fallbacks: Vec<(String, Option<String>)>,
    ) -> Self {
        Self {
            openai: OpenAIClient::new(api_key.into(), account_id),
            openai_fallbacks: fallbacks
                .into_iter()
                .map(|(key, acc)| OpenAIClient::new(key, acc))
                .collect(),
            antigravity: AntigravityClient::new(),
            opencode: OpenCodeGoClient::new(),
        }
    }

    #[cfg(test)]
    fn openai_fallback(&self) -> Option<&OpenAIClient> {
        self.openai_fallbacks.first()
    }

    #[cfg(test)]
    fn determine_format(&self, model: &str) -> PayloadFormat {
        if is_antigravity_model(model) || is_opencode_model(model) {
            PayloadFormat::ChatCompletions
        } else if self.openai.is_codex() {
            PayloadFormat::Codex
        } else {
            PayloadFormat::ChatCompletions
        }
    }

    fn provider_kind(&self, model: &str) -> &'static str {
        if is_antigravity_model(model) {
            "antigravity"
        } else if is_opencode_model(model) {
            "opencode-go"
        } else if self.openai.is_codex() {
            "codex"
        } else {
            "openai"
        }
    }

    async fn stream_chat_completion(
        &self,
        payload_source: impl Into<PayloadSource>,
        prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let source = payload_source.into();
        let model = source.model().to_string();
        let span = tracing::info_span!("provider.stream", model = %model);
        tracing::debug!(parent: &span, "routing stream request");
        if is_antigravity_model(&model) {
            tracing::debug!(provider = "antigravity", "selected provider");
            let provider = Arc::new(self.antigravity.clone());
            provider
                .stream_chat_completion(source, prompt_cache_key, event_tx)
                .await;
            return;
        }
        if is_opencode_model(&model) {
            tracing::debug!(provider = "opencode-go", "selected provider");
            let provider = Arc::new(self.opencode.clone());
            provider
                .stream_chat_completion(source, prompt_cache_key, event_tx)
                .await;
            return;
        }

        if !self.openai_fallbacks.is_empty() {
            tracing::debug!(
                provider = "openai",
                fallback_count = self.openai_fallbacks.len(),
                "selected provider with fallbacks"
            );
            let mut clients = Vec::with_capacity(1 + self.openai_fallbacks.len());
            clients.push(self.openai.clone());
            clients.extend(self.openai_fallbacks.clone());

            let tasks: Vec<
                Box<dyn FnOnce(mpsc::Sender<StreamEvent>) -> BoxFuture<'static, ()> + Send>,
            > = clients
                .into_iter()
                .map(|client| {
                    let source = source.clone();
                    let prompt_cache_key = prompt_cache_key.clone();
                    let task: Box<
                        dyn FnOnce(mpsc::Sender<StreamEvent>) -> BoxFuture<'static, ()> + Send,
                    > = Box::new(move |tx| {
                        Box::pin(async move {
                            let provider: Arc<dyn crate::traits::ModelProvider> = Arc::new(client);
                            provider
                                .stream_chat_completion(source, prompt_cache_key, tx)
                                .await;
                        })
                    });
                    task
                })
                .collect();

            Self::execute_stream_fallback_chain(tasks, event_tx).await;
        } else {
            tracing::debug!(provider = "openai", "selected provider");
            let provider: Arc<dyn crate::traits::ModelProvider> = Arc::new(self.openai.clone());
            provider
                .stream_chat_completion(source, prompt_cache_key, event_tx)
                .await;
        }
    }

    /// Returns true for provider errors where retrying the identical request on
    /// a configured fallback is safe (before any output was emitted).
    pub fn is_quota_or_rate_limit(error: &str) -> bool {
        is_quota_or_rate_limit(error)
    }

    /// Routes a completed provider stream through a sequence of fallbacks before
    /// forwarding events to the caller. Fallback is permitted only if the
    /// preceding provider failed before emitting visible output, preventing duplication.
    #[cfg(test)]
    async fn stream_with_fallback<P, F, PrimaryFut, FallbackFut>(
        &self,
        primary: P,
        fallback: F,
        event_tx: mpsc::Sender<StreamEvent>,
    ) where
        P: FnOnce(mpsc::Sender<StreamEvent>) -> PrimaryFut + Send + 'static,
        F: FnOnce(mpsc::Sender<StreamEvent>) -> FallbackFut + Send + 'static,
        PrimaryFut: std::future::Future<Output = ()> + Send + 'static,
        FallbackFut: std::future::Future<Output = ()> + Send + 'static,
    {
        let tasks: Vec<
            Box<dyn FnOnce(mpsc::Sender<StreamEvent>) -> BoxFuture<'static, ()> + Send>,
        > = vec![
            Box::new(move |tx| Box::pin(primary(tx))),
            Box::new(move |tx| Box::pin(fallback(tx))),
        ];
        Self::execute_stream_fallback_chain(tasks, event_tx).await;
    }

    async fn execute_stream_fallback_chain(
        tasks: Vec<Box<dyn FnOnce(mpsc::Sender<StreamEvent>) -> BoxFuture<'static, ()> + Send>>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let total = tasks.len();
        for (idx, task) in tasks.into_iter().enumerate() {
            let (tx, mut rx) = mpsc::channel(32);
            let producer_fut = task(tx);
            let producer_handle = tokio::spawn(producer_fut);

            let is_last = idx + 1 == total;
            let mut emitted_visible_output = false;
            let mut retry = false;

            while let Some(event) = rx.recv().await {
                match &event {
                    StreamEvent::ContentToken(_)
                    | StreamEvent::ReasoningToken(_)
                    | StreamEvent::ToolCallStart { .. } => {
                        emitted_visible_output = true;
                    }
                    StreamEvent::Error(error)
                        if !emitted_visible_output && !is_last && is_quota_or_rate_limit(error) =>
                    {
                        tracing::warn!(
                            attempt = idx + 1,
                            error = %error,
                            "quota or rate-limit failure; retrying on fallback"
                        );
                        retry = true;
                        break;
                    }
                    _ => {}
                }
                if event_tx.send(event).await.is_err() {
                    producer_handle.abort();
                    return;
                }
            }

            if retry {
                producer_handle.abort();
                continue;
            }

            let _ = producer_handle.await;
            return;
        }
    }

    async fn fetch_deferred(
        &self,
        model: &str,
        handle_id: &str,
    ) -> Result<crate::traits::DeferredResponse, String> {
        let provider: Arc<dyn crate::traits::ModelProvider> = if is_antigravity_model(model) {
            Arc::new(self.antigravity.clone())
        } else if is_opencode_model(model) {
            Arc::new(self.opencode.clone())
        } else {
            Arc::new(self.openai.clone())
        };
        provider.fetch_deferred(handle_id).await
    }

    async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String> {
        let provider: Arc<dyn crate::traits::ModelProvider> = if is_antigravity_model(model) {
            Arc::new(self.antigravity.clone())
        } else if is_opencode_model(model) {
            Arc::new(self.opencode.clone())
        } else {
            Arc::new(self.openai.clone())
        };
        provider.cancel_deferred(handle_id).await
    }

    /// Generate a short session title using the provider selected by the model id.
    pub async fn generate_title(&self, model: &str, prompt: &str) -> Result<String, String> {
        if !is_opencode_model(model) {
            return self.openai.generate_title(model, prompt).await;
        }

        let mut payload = title_payload(model, prompt, false);
        if let Some(object) = payload.as_object_mut() {
            object.insert("stream".to_owned(), Value::Bool(true));
        }

        let (event_tx, mut event_rx) = mpsc::channel(128);
        let client = self.clone();
        let stream_task = tokio::spawn(async move {
            client
                .stream_chat_completion(PayloadSource::ChatCompletions(payload), None, event_tx)
                .await;
        });

        let received = tokio::time::timeout(TITLE_REQUEST_TIMEOUT, async {
            let mut text = String::new();
            let mut error = None;
            while let Some(event) = event_rx.recv().await {
                match event {
                    StreamEvent::ContentToken(token) => text.push_str(&token),
                    StreamEvent::Error(message) => error = Some(message),
                    StreamEvent::Finished { .. }
                    | StreamEvent::ReasoningToken(_)
                    | StreamEvent::ToolCallStart { .. }
                    | StreamEvent::ToolCallArgsDelta { .. } => {}
                }
            }
            (text, error)
        })
        .await;

        let (text, error) = match received {
            Ok(result) => result,
            Err(_) => {
                stream_task.abort();
                return Err("OpenCode title request timed out".to_owned());
            }
        };

        if stream_task.await.is_err() && error.is_none() {
            return Err("OpenCode title stream terminated unexpectedly".to_owned());
        }
        if let Some(error) = error {
            return Err(error);
        }

        if text.trim().is_empty() {
            Err("OpenCode title response did not contain text".to_owned())
        } else {
            Ok(text)
        }
    }

    /// Generate a short commit subject from a Git diff without adding a message to the chat.
    pub async fn generate_commit_message(&self, model: &str, diff: &str) -> Result<String, String> {
        let model = model.to_owned();
        let instructions = concat!(
            "You are an expert software engineer generating a Git commit message.\n",
            "Follow Conventional Commits format (`<type>: <description>` or `<type>(<scope>): <description>`).\n",
            "Rules:\n",
            "1. Use imperative, present tense: 'add', 'fix', 'refactor', 'update', 'remove' (not 'added', 'fixed').\n",
            "2. Common types: feat, fix, refactor, style, perf, docs, test, chore.\n",
            "3. Keep the entire commit subject under 72 characters.\n",
            "4. Do not end the subject line with a period.\n",
            "5. Output ONLY the raw commit subject line. Do NOT include quotes, backticks, bullet points, or markdown formatting."
        );
        let prompt = Arc::new(format!(
            "{instructions}\n\nHere is the diff of the changes:\n\n{diff}"
        ));
        let instructions_str = instructions.to_string();
        let model_for_payload = model.clone();
        let payload = PayloadSource::lazy(model.clone(), move |format| {
            let prompt = Arc::clone(&prompt);
            let model = model_for_payload.clone();
            let instructions_str = instructions_str.clone();
            Box::pin(async move {
                match format {
                    PayloadFormat::Codex => serde_json::json!({
                        "model": model,
                        "instructions": instructions_str,
                        "input": [{
                            "type": "message",
                            "role": "user",
                            "content": [{"type": "input_text", "text": prompt.as_str()}]
                        }],
                        "store": false,
                        "stream": true
                    }),
                    PayloadFormat::ChatCompletions => serde_json::json!({
                        "model": model,
                        "messages": [
                            {"role": "system", "content": instructions_str},
                            {"role": "user", "content": prompt.as_str()}
                        ],
                        "max_tokens": 96,
                        "stream": true
                    }),
                }
            })
        });
        let (event_tx, mut event_rx) = mpsc::channel(128);
        let client = self.clone();
        let stream_task = tokio::spawn(async move {
            client.stream_chat_completion(payload, None, event_tx).await;
        });

        let mut text = String::new();
        let mut error = None;
        while let Some(event) = event_rx.recv().await {
            match event {
                StreamEvent::ContentToken(token) => text.push_str(&token),
                StreamEvent::Error(message) => error = Some(message),
                StreamEvent::Finished { .. }
                | StreamEvent::ReasoningToken(_)
                | StreamEvent::ToolCallStart { .. }
                | StreamEvent::ToolCallArgsDelta { .. } => {}
            }
        }
        if stream_task.await.is_err() && error.is_none() {
            return Err("commit message generation stream terminated unexpectedly".to_owned());
        }
        if let Some(error) = error {
            return Err(error);
        }
        let text = text
            .trim()
            .trim_matches('`')
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_owned();
        if text.is_empty() {
            Err("The model returned an empty commit message".to_owned())
        } else {
            Ok(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_retryable_quota_and_rate_limit_errors() {
        assert!(is_quota_or_rate_limit("HTTP 429 Too Many Requests"));
        assert!(is_quota_or_rate_limit("quota exhausted"));
        assert!(is_quota_or_rate_limit("rate_limit_exceeded"));
        assert!(!is_quota_or_rate_limit("HTTP 401 unauthorized"));
    }

    #[tokio::test]
    async fn retries_quota_failure_on_fallback_without_forwarding_error() {
        let client = ProviderClient::new("test", None);
        let (tx, mut rx) = mpsc::channel(8);
        client
            .stream_with_fallback(
                |tx| async move {
                    tx.send(StreamEvent::Error("HTTP 429 quota exceeded".into()))
                        .await
                        .unwrap();
                },
                |tx| async move {
                    tx.send(StreamEvent::ContentToken("recovered".into()))
                        .await
                        .unwrap();
                    tx.send(StreamEvent::Finished {
                        tool_calls: Vec::new(),
                        usage: crate::openai::ProviderUsage::default(),
                    })
                    .await
                    .unwrap();
                },
                tx,
            )
            .await;
        assert!(
            matches!(rx.recv().await, Some(StreamEvent::ContentToken(text)) if text == "recovered")
        );
        assert!(matches!(
            rx.recv().await,
            Some(StreamEvent::Finished { .. })
        ));
        assert!(rx.recv().await.is_none());
    }

    #[test]
    fn routes_only_prefixed_models_to_antigravity() {
        assert!(is_antigravity_model("antigravity/gemini-3.6-flash"));
        assert!(!is_antigravity_model("gpt-5.6-luna"));
        assert!(!is_antigravity_model("gemini-3.6-flash"));
    }

    #[test]
    fn routes_only_prefixed_models_to_opencode() {
        assert!(is_opencode_model("opencode-go/deepseek-v4-flash"));
        assert!(!is_opencode_model("deepseek-v4-flash"));
        assert!(!is_opencode_model("antigravity/gemini-3.6-flash"));
    }

    #[tokio::test]
    async fn runtime_request_builds_an_object_for_codex() {
        let request = RuntimeRequest {
            model: "gpt-5.6-luna".into(),
            messages: serde_json::json!([{"role": "user", "content": "hello"}]),
            tools: serde_json::json!([]),
            prompt_cache_key: Some("cache-key".into()),
            reasoning_effort: Some("medium".into()),
        };
        let payload = runtime_request_payload_source(&request)
            .resolve(PayloadFormat::Codex)
            .await;
        assert!(payload.is_object());
        assert_eq!(payload["model"], "gpt-5.6-luna");
        assert_eq!(
            payload["input"],
            serde_json::json!([{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}],
            }])
        );
        assert_eq!(payload["tools"], serde_json::json!([]));
        assert_eq!(payload["reasoning"]["effort"], "medium");
        assert_eq!(payload["store"], false);
    }

    #[tokio::test]
    async fn runtime_request_flattens_tools_for_codex() {
        let request = RuntimeRequest {
            model: "gpt-5.6-luna".into(),
            messages: serde_json::json!([]),
            tools: serde_json::json!([{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read a workspace file",
                    "parameters": {"type": "object", "properties": {}}
                }
            }]),
            prompt_cache_key: None,
            reasoning_effort: None,
        };
        let payload = runtime_request_payload_source(&request)
            .resolve(PayloadFormat::Codex)
            .await;
        assert_eq!(
            payload["tools"],
            serde_json::json!([{
                "type": "function",
                "name": "read_file",
                "description": "Read a workspace file",
                "parameters": {"type": "object", "properties": {}}
            }])
        );
        assert!(payload["tools"][0].get("function").is_none());
    }

    #[tokio::test]
    async fn runtime_request_converts_custom_messages_for_codex() {
        let request = RuntimeRequest {
            model: "gpt-5.6-luna".into(),
            messages: serde_json::json!([
                {"role": "system", "content": "system context"},
                {"role": "user", "content": "hello"},
                {
                    "role": "custom",
                    "custom_type": "thinking",
                    "payload": "internal reasoning"
                }
            ]),
            tools: serde_json::json!([]),
            prompt_cache_key: None,
            reasoning_effort: None,
        };
        let payload = runtime_request_payload_source(&request)
            .resolve(PayloadFormat::Codex)
            .await;
        assert_eq!(payload["instructions"], "system context");
        assert_eq!(payload["input"].as_array().unwrap().len(), 1);
        assert!(payload["input"].as_array().unwrap().iter().all(|item| {
            item.get("role")
                .and_then(Value::as_str)
                .is_none_or(|role| role != "custom")
        }));
    }

    #[tokio::test]
    async fn runtime_request_preserves_chat_completions_envelope() {
        let request = RuntimeRequest {
            model: "gpt-5.6-luna".into(),
            messages: serde_json::json!([
                {"role": "user", "content": "hello"},
                {
                    "role": "custom",
                    "custom_type": "thinking",
                    "payload": { "text": "internal reasoning" }
                }
            ]),
            tools: serde_json::json!([]),
            prompt_cache_key: None,
            reasoning_effort: Some("medium".into()),
        };
        let payload = runtime_request_payload_source(&request)
            .resolve(PayloadFormat::ChatCompletions)
            .await;
        assert!(payload.is_object());
        assert_eq!(
            payload["messages"],
            serde_json::json!([{"role": "user", "content": "hello"}])
        );
        assert_eq!(payload["tools"], request.tools);
        assert_eq!(payload["reasoning_effort"], "medium");
        assert_eq!(payload["stream_options"]["include_usage"], true);
    }

    #[test]
    fn determines_payload_format_correctly() {
        let client_std = ProviderClient::new("sk-test", None);
        assert_eq!(
            client_std.determine_format("gpt-4o"),
            PayloadFormat::ChatCompletions
        );
        assert_eq!(
            client_std.determine_format("antigravity/gemini-3.6-flash"),
            PayloadFormat::ChatCompletions
        );
        assert_eq!(
            client_std.determine_format("opencode-go/deepseek-v4-flash"),
            PayloadFormat::ChatCompletions
        );

        let client_codex = ProviderClient::new("ey-test", Some("acc-123".into()));
        assert_eq!(
            client_codex.determine_format("gpt-4o"),
            PayloadFormat::Codex
        );
        assert_eq!(
            client_codex.determine_format("antigravity/gemini-3.6-flash"),
            PayloadFormat::ChatCompletions
        );
    }

    #[test]
    fn opencode_title_route_stays_on_chat_completions_with_codex_credentials() {
        let client = ProviderClient::new("ey-test", Some("acc-123".into()));
        assert_eq!(
            client.determine_format("opencode-go/deepseek-v4-flash"),
            PayloadFormat::ChatCompletions
        );

        let payload = title_payload("opencode-go/deepseek-v4-flash", "Fix the login flow", false);
        assert!(payload.get("messages").is_some());
        assert!(payload.get("input").is_none());
    }

    #[test]
    fn test_provider_client_with_fallback_account_creation() {
        let client = ProviderClient::with_fallback_account(
            "key1",
            Some("acc1".into()),
            "key2",
            Some("acc2".into()),
        );
        assert_eq!(client.provider_kind("gpt-4o"), "codex");
        assert!(client.openai_fallback().is_some());
    }

    #[tokio::test]
    async fn does_not_deadlock_when_primary_emits_more_than_channel_capacity_events() {
        let client = ProviderClient::new("test", None);
        let (tx, mut rx) = mpsc::channel(200);

        // Send 100 events, which exceeds the mpsc channel capacity of 32
        client
            .stream_with_fallback(
                |tx| async move {
                    for i in 0..100 {
                        tx.send(StreamEvent::ContentToken(format!("token_{i}")))
                            .await
                            .unwrap();
                    }
                    tx.send(StreamEvent::Finished {
                        tool_calls: Vec::new(),
                        usage: crate::openai::ProviderUsage::default(),
                    })
                    .await
                    .unwrap();
                },
                |_tx| async move {},
                tx,
            )
            .await;

        let mut received = 0;
        while let Some(event) = rx.recv().await {
            match event {
                StreamEvent::ContentToken(_) => received += 1,
                StreamEvent::Finished { .. } => break,
                _ => {}
            }
        }
        assert_eq!(received, 100);
    }

    #[tokio::test]
    async fn tries_all_backups_in_fallback_chain() {
        let _client = ProviderClient::with_fallback_accounts(
            "key1",
            Some("acc1".into()),
            vec![
                ("key2".into(), Some("acc2".into())),
                ("key3".into(), Some("acc3".into())),
            ],
        );
        let (tx, mut rx) = mpsc::channel(8);

        let tasks: Vec<
            Box<dyn FnOnce(mpsc::Sender<StreamEvent>) -> BoxFuture<'static, ()> + Send>,
        > = vec![
            Box::new(|tx| {
                Box::pin(async move {
                    tx.send(StreamEvent::Error("HTTP 429 quota exceeded".into()))
                        .await
                        .unwrap();
                })
            }),
            Box::new(|tx| {
                Box::pin(async move {
                    tx.send(StreamEvent::Error("HTTP 429 quota exceeded".into()))
                        .await
                        .unwrap();
                })
            }),
            Box::new(|tx| {
                Box::pin(async move {
                    tx.send(StreamEvent::ContentToken("backup3_ok".into()))
                        .await
                        .unwrap();
                })
            }),
        ];

        ProviderClient::execute_stream_fallback_chain(tasks, tx).await;

        assert!(matches!(
            rx.recv().await,
            Some(StreamEvent::ContentToken(text)) if text == "backup3_ok"
        ));
    }

    #[tokio::test]
    async fn returns_final_error_when_all_backups_fail() {
        let (tx, mut rx) = mpsc::channel(8);

        let tasks: Vec<
            Box<dyn FnOnce(mpsc::Sender<StreamEvent>) -> BoxFuture<'static, ()> + Send>,
        > = vec![
            Box::new(|tx| {
                Box::pin(async move {
                    tx.send(StreamEvent::Error("HTTP 429 quota1".into()))
                        .await
                        .unwrap();
                })
            }),
            Box::new(|tx| {
                Box::pin(async move {
                    tx.send(StreamEvent::Error("HTTP 429 quota2".into()))
                        .await
                        .unwrap();
                })
            }),
        ];

        ProviderClient::execute_stream_fallback_chain(tasks, tx).await;

        assert!(matches!(
            rx.recv().await,
            Some(StreamEvent::Error(err)) if err == "HTTP 429 quota2"
        ));
    }
}
