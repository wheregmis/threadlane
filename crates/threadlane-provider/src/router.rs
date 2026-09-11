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

    fn refresh_openai_credentials(&self, api_key: String, account_id: Option<String>) {
        self.openai.refresh_credentials(api_key, account_id);
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
            "You MUST strictly follow the Conventional Commits specification.\n\n",
            "Format:\n",
            "<type>: <description> OR <type>(<scope>): <description>\n\n",
            "Allowed types (MUST be lowercase):\n",
            "- feat: A new feature or capability\n",
            "- fix: A bug fix or defect resolution\n",
            "- chore: Maintenance, dependency updates, tooling, or minor chores\n",
            "- refactor: Code restructuring without changing behavior or fixing bugs\n",
            "- docs: Documentation changes only\n",
            "- style: Code formatting or styling with no logic change\n",
            "- perf: Performance improvements\n",
            "- test: Adding or updating tests\n",
            "- build: Build system or packaging changes\n",
            "- ci: CI/CD configuration and automation scripts\n",
            "- revert: Reverting a previous commit\n\n",
            "Rules:\n",
            "1. The commit message MUST start with one of the allowed types (e.g. 'feat:', 'fix:', 'chore:'). Never omit the type prefix.\n",
            "2. An optional scope may describe the affected component: e.g. 'feat(auth):', 'fix(git):'.\n",
            "3. Use imperative, present tense for the description: 'add' (not 'added'), 'fix' (not 'fixed'), 'update' (not 'updated').\n",
            "4. Keep the entire commit subject under 72 characters.\n",
            "5. Do not end the subject line with a period.\n",
            "6. Output ONLY the raw commit subject line. Do NOT include quotes, backticks, bullet points, preamble, or markdown formatting."
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
                        "max_tokens": 256,
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
        let message = normalize_commit_message(&text);
        if message.is_empty() {
            Err("The model returned an empty commit message".to_owned())
        } else {
            Ok(message)
        }
    }
}

/// Formats and truncates a Conventional Commit message to at most `max_chars` Unicode characters,
/// budgeting scope to ensure a complete `type(scope): description` structure is preserved.
fn format_and_truncate_conventional_commit(
    type_name: &str,
    scope: Option<&str>,
    is_breaking: bool,
    desc: &str,
    max_chars: usize,
) -> String {
    let bang = if is_breaking { "!" } else { "" };
    let desc = if desc.is_empty() { "update" } else { desc };

    // Budget scope so that a complete `type(scope): description` structure is preserved
    let min_desc_budget = 10;
    let scope_str = match scope {
        Some(s) => {
            let fixed_overhead = type_name.chars().count() + bang.chars().count() + 4; // '(): '
            let available_scope = max_chars.saturating_sub(fixed_overhead + min_desc_budget);
            if s.chars().count() > available_scope && available_scope > 0 {
                let truncated: String = s.chars().take(available_scope).collect();
                let clean = truncated.trim_end_matches(['-', '_', '.', '/']);
                format!("({clean})")
            } else {
                format!("({s})")
            }
        }
        None => String::new(),
    };

    let prefix = format!("{}{}{}: ", type_name, scope_str, bang);
    let prefix_len = prefix.chars().count();
    let avail_desc = max_chars.saturating_sub(prefix_len);

    if desc.chars().count() <= avail_desc {
        return format!("{}{}", prefix, desc);
    }

    if avail_desc == 0 {
        return prefix.trim_end().to_string();
    }

    let desc_trunc: String = desc.chars().take(avail_desc).collect();
    let truncated_desc = if let Some(last_space) = desc_trunc.rfind(' ') {
        let candidate = desc_trunc[..last_space].trim_end();
        let cleaned = candidate.trim_end_matches([',', ';', '-', ':', '.']);
        if !cleaned.is_empty() {
            cleaned
        } else {
            desc_trunc.trim_end_matches([',', ';', '-', ':', '.'])
        }
    } else {
        desc_trunc.trim_end_matches([',', ';', '-', ':', '.'])
    };

    format!("{}{}", prefix, truncated_desc)
}

fn clean_wrapper_prefixes(line: &str) -> &str {
    let mut s = line.trim();

    let trimmed_prompt = s.trim_start_matches(['$', '>', '#', ' ']);
    if trimmed_prompt
        .to_ascii_lowercase()
        .starts_with("git commit")
    {
        if let Some(m_idx) = trimmed_prompt.find("-m") {
            let rest = trimmed_prompt[m_idx + 2..].trim();
            let unquoted = rest
                .strip_prefix('"')
                .and_then(|r| r.strip_suffix('"'))
                .or_else(|| rest.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
                .unwrap_or(rest)
                .trim();
            if !unquoted.is_empty() {
                return unquoted;
            }
        }
    }

    s = s
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim();

    let lower = s.to_ascii_lowercase();
    if (lower.starts_with("here is") || lower.starts_with("here's"))
        && (lower.contains("commit message") || lower.ends_with(':'))
    {
        s = match s.find(':') {
            Some(idx) => s[idx + 1..].trim(),
            None => "",
        };
    }

    for prefix in &[
        "commit message:",
        "commit:",
        "git commit:",
        "subject:",
        "proposed commit message:",
        "generated commit message:",
    ] {
        if s.to_ascii_lowercase().starts_with(prefix) {
            s = s[prefix.len()..].trim();
            break;
        }
    }

    s.trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
}

fn extract_candidate_line(raw: &str) -> Option<&str> {
    let mut in_fence = false;
    let mut fence_lines = Vec::new();
    let mut normal_lines = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if in_fence {
            fence_lines.push(trimmed);
        } else {
            normal_lines.push(trimmed);
        }
    }

    let candidates = if !fence_lines.is_empty() {
        fence_lines
    } else {
        normal_lines
    };

    for candidate in candidates {
        let cleaned = clean_wrapper_prefixes(candidate);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

struct ConventionalMatch<'a> {
    type_name: &'static str,
    scope: Option<&'a str>,
    is_breaking: bool,
    description: &'a str,
}

fn map_alias_type(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "feat" | "feature" => Some("feat"),
        "fix" | "bugfix" | "bug" => Some("fix"),
        "chore" | "maintenance" => Some("chore"),
        "refactor" | "refactoring" => Some("refactor"),
        "docs" | "doc" | "documentation" => Some("docs"),
        "style" | "styles" => Some("style"),
        "perf" | "performance" => Some("perf"),
        "test" | "tests" => Some("test"),
        "build" => Some("build"),
        "ci" => Some("ci"),
        "revert" => Some("revert"),
        _ => None,
    }
}

fn parse_conventional_prefix(line: &str) -> Option<ConventionalMatch<'_>> {
    let colon_idx = line.find(':')?;
    let prefix = line[..colon_idx].trim();
    let rest = line[colon_idx + 1..].trim();

    let (prefix_no_bang, is_breaking) = if let Some(stripped) = prefix.strip_suffix('!') {
        (stripped.trim_end(), true)
    } else {
        (prefix, false)
    };

    let (raw_type, scope) = if let Some(scope_start) = prefix_no_bang.find('(') {
        if prefix_no_bang.ends_with(')') && scope_start > 0 {
            let t = prefix_no_bang[..scope_start].trim();
            let s = prefix_no_bang[scope_start + 1..prefix_no_bang.len() - 1].trim();
            (t, if s.is_empty() { None } else { Some(s) })
        } else {
            return None;
        }
    } else {
        (prefix_no_bang.trim(), None)
    };

    if let Some(s) = scope {
        if s.contains('\n') || s.contains(':') || s.contains('(') || s.contains(')') {
            return None;
        }
    }

    let mapped_type = map_alias_type(raw_type)?;
    Some(ConventionalMatch {
        type_name: mapped_type,
        scope,
        is_breaking,
        description: rest,
    })
}

fn normalize_word(word: &str) -> String {
    let is_all_caps = word.len() > 1 && word.chars().all(|c| c.is_ascii_uppercase());
    let has_inner_caps = word.chars().skip(1).any(|c| c.is_ascii_uppercase());
    if is_all_caps || has_inner_caps {
        word.to_string()
    } else {
        let mut chars = word.chars();
        match chars.next() {
            Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    }
}

fn normalize_description(desc: &str) -> String {
    let mut s = desc
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    s = s.trim_end_matches('.');
    if s.is_empty() {
        return String::new();
    }

    let words: Vec<&str> = s.split_whitespace().collect();
    if words.is_empty() {
        return String::new();
    }

    let first_word = words[0];
    let lower_first = first_word.to_ascii_lowercase();
    let converted_first = match lower_first.as_str() {
        "added" | "adds" => "add",
        "created" | "creates" => "create",
        "implemented" | "implements" => "implement",
        "introduced" | "introduces" => "introduce",
        "supported" | "supports" => "support",
        "allowed" | "allows" => "allow",
        "enabled" | "enables" => "enable",
        "provided" | "provides" => "provide",
        "updated" | "updates" => "update",
        "bumped" | "bumps" => "bump",
        "upgraded" | "upgrades" => "upgrade",
        "simplified" | "simplifies" => "simplify",
        "restructured" | "restructures" => "restructure",
        "reorganized" | "reorganizes" => "reorganize",
        "cleaned" | "cleans" => "clean",
        "optimized" | "optimizes" => "optimize",
        "formatted" | "formats" => "format",
        "documented" | "documents" => "document",
        "tested" | "tests" => "test",
        "prevented" | "prevents" => "prevent",
        "resolved" | "resolves" => "resolve",
        "handled" | "handles" => "handle",
        "corrected" | "corrects" => "correct",
        "patched" | "patches" => "patch",
        "avoided" | "avoids" => "avoid",
        _ => "",
    };

    let first_normalized = if !converted_first.is_empty() {
        converted_first.to_string()
    } else {
        normalize_word(first_word)
    };

    let mut result = vec![first_normalized];
    for &word in &words[1..] {
        result.push(normalize_word(word));
    }
    result.join(" ")
}

fn infer_conventional_commit(line: &str) -> (&'static str, String) {
    let trimmed = line.trim().trim_start_matches(['-', '*', '>', '•']).trim();
    let leading_token = trimmed.split_whitespace().next().unwrap_or_default();
    let first_word = leading_token.trim_matches(|c: char| !c.is_alphanumeric());
    let lower_first = first_word.to_ascii_lowercase();

    let lower_line = trimmed.to_ascii_lowercase();
    if lower_line.contains("readme")
        || matches!(
            lower_first.as_str(),
            "doc" | "docs" | "document" | "documents" | "documented" | "documentation"
        )
    {
        let desc = normalize_description(trimmed);
        return ("docs", desc);
    }

    let has_test_word = trimmed
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| matches!(w.to_ascii_lowercase().as_str(), "test" | "tests" | "testing"));
    if has_test_word {
        let desc = normalize_description(trimmed);
        return ("test", desc);
    }

    if matches!(
        lower_first.as_str(),
        "fix" | "fixes" | "fixed" | "fixing" | "resolve" | "resolves" | "resolved"
            | "resolving" | "prevent" | "prevents" | "prevented" | "preventing" | "handle"
            | "handles" | "handled" | "handling" | "correct" | "corrects" | "corrected"
            | "correcting" | "patch" | "patches" | "patched" | "avoid" | "avoids" | "avoided"
    ) {
        let desc = if matches!(lower_first.as_str(), "fix" | "fixes" | "fixed" | "fixing") {
            let rest = trimmed[leading_token.len()..].trim();
            let next_word = rest
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if matches!(next_word.as_str(), "and" | "or" | "for" | "to" | "in" | "on" | "with")
                || rest.is_empty()
            {
                normalize_description(trimmed)
            } else {
                normalize_description(rest)
            }
        } else {
            normalize_description(trimmed)
        };
        return ("fix", desc);
    }

    if matches!(
        lower_first.as_str(),
        "add" | "adds" | "added" | "adding" | "create" | "creates" | "created" | "creating"
            | "implement" | "implements" | "implemented" | "implementing" | "introduce"
            | "introduces" | "introduced" | "introducing" | "support" | "supports"
            | "supported" | "supporting" | "allow" | "allows" | "allowed" | "allowing"
            | "enable" | "enables" | "enabled" | "enabling" | "provide" | "provides"
            | "provided" | "providing" | "feat" | "feature" | "features"
    ) {
        let desc = normalize_description(trimmed);
        return ("feat", desc);
    }

    if matches!(
        lower_first.as_str(),
        "refactor" | "refactors" | "refactored" | "refactoring" | "restructure"
            | "restructures" | "restructured" | "restructuring" | "reorganize"
            | "reorganizes" | "reorganized" | "reorganizing" | "simplify" | "simplifies"
            | "simplified" | "simplifying" | "rewrite" | "rewrites" | "rewrote" | "rewriting"
            | "cleanup" | "clean" | "cleans" | "cleaned" | "cleaning"
    ) {
        let desc = if matches!(
            lower_first.as_str(),
            "refactor" | "refactors" | "refactored" | "refactoring"
        ) {
            let rest = trimmed[leading_token.len()..].trim();
            if rest.is_empty() {
                normalize_description(trimmed)
            } else {
                normalize_description(rest)
            }
        } else {
            normalize_description(trimmed)
        };
        return ("refactor", desc);
    }

    let has_perf_word = trimmed
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| {
            matches!(
                w.to_ascii_lowercase().as_str(),
                "perf"
                    | "performance"
                    | "speedup"
                    | "optimize"
                    | "optimizes"
                    | "optimized"
                    | "optimizing"
                    | "optimization"
            )
        });
    if has_perf_word {
        let desc = normalize_description(trimmed);
        return ("perf", desc);
    }

    if matches!(
        lower_first.as_str(),
        "style" | "styles" | "format" | "formats" | "formatted" | "formatting" | "lint"
            | "lints" | "linted" | "linting" | "rustfmt" | "prettier"
    ) {
        let desc = normalize_description(trimmed);
        return ("style", desc);
    }

    if matches!(
        lower_first.as_str(),
        "build" | "cmake" | "cargo" | "package" | "packaging"
    ) {
        let desc = normalize_description(trimmed);
        return ("build", desc);
    }

    if matches!(
        lower_first.as_str(),
        "ci" | "cd" | "workflow" | "workflows" | "action" | "actions" | "pipeline" | "pipelines"
    ) {
        let desc = normalize_description(trimmed);
        return ("ci", desc);
    }

    if matches!(
        lower_first.as_str(),
        "revert" | "reverts" | "reverted" | "reverting"
    ) {
        let desc = normalize_description(trimmed);
        return ("revert", desc);
    }

    if matches!(
        lower_first.as_str(),
        "update" | "updates" | "updated" | "updating" | "bump" | "bumps" | "bumped"
            | "bumping" | "upgrade" | "upgrades" | "upgraded" | "upgrading" | "deps"
            | "dependencies" | "dependency" | "release" | "releases" | "released" | "chore"
            | "chores"
    ) {
        let desc = normalize_description(trimmed);
        return ("chore", desc);
    }

    ("chore", normalize_description(trimmed))
}

/// Normalizes a commit message candidate to follow the Conventional Commits specification
/// (`<type>: <description>` or `<type>(<scope>): <description>`).
pub fn normalize_commit_message(raw: &str) -> String {
    let candidate = match extract_candidate_line(raw) {
        Some(line) => line,
        None => return String::new(),
    };

    let (type_name, scope, is_breaking, desc) =
        if let Some(matched) = parse_conventional_prefix(candidate) {
            let desc = normalize_description(matched.description);
            let desc = if desc.is_empty() {
                "update".to_string()
            } else {
                desc
            };
            (matched.type_name, matched.scope, matched.is_breaking, desc)
        } else {
            let (type_name, desc) = infer_conventional_commit(candidate);
            let desc = if desc.is_empty() {
                "update".to_string()
            } else {
                desc
            };
            (type_name, None, false, desc)
        };

    format_and_truncate_conventional_commit(type_name, scope, is_breaking, &desc, 72)
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

    #[test]
    fn normalizes_valid_conventional_commits() {
        assert_eq!(
            normalize_commit_message("feat: add user authentication"),
            "feat: add user authentication"
        );
        assert_eq!(
            normalize_commit_message("fix(git): resolve merge conflicts"),
            "fix(git): resolve merge conflicts"
        );
        assert_eq!(
            normalize_commit_message("chore(deps)!: upgrade tokio to 1.38"),
            "chore(deps)!: upgrade tokio to 1.38"
        );
        assert_eq!(
            normalize_commit_message("Feat: Add Dark Mode Support."),
            "feat: add dark mode support"
        );
        assert_eq!(
            normalize_commit_message("fix:handle missing branch"),
            "fix: handle missing branch"
        );
        assert_eq!(
            normalize_commit_message("feat: ."),
            "feat: update"
        );
        assert_eq!(
            normalize_commit_message("feat(auth): \"\""),
            "feat(auth): update"
        );
    }

    #[test]
    fn normalizes_wrapper_formatting_and_fences() {
        assert_eq!(
            normalize_commit_message("```git\nfeat: add git commit generation\n```"),
            "feat: add git commit generation"
        );
        assert_eq!(
            normalize_commit_message("\"feat: add git review dialog\""),
            "feat: add git review dialog"
        );
        assert_eq!(
            normalize_commit_message("git commit -m \"fix: resolve crash on startup\""),
            "fix: resolve crash on startup"
        );
        assert_eq!(
            normalize_commit_message("Commit message: feat: add new button"),
            "feat: add new button"
        );
        assert_eq!(
            normalize_commit_message("Commit: Fix branch picker"),
            "fix: branch picker"
        );
        assert_eq!(
            normalize_commit_message(
                "Here is the commit message:\n\nfeat: add dark mode\n\nDetailed explanation..."
            ),
            "feat: add dark mode"
        );
        assert_eq!(
            normalize_commit_message("Here is the commit message: feat: add dark mode"),
            "feat: add dark mode"
        );
        assert_eq!(
            normalize_commit_message("fix: handle git commit -m arguments"),
            "fix: handle git commit -m arguments"
        );
    }

    #[test]
    fn infers_standard_types_when_prefix_omitted() {
        assert_eq!(
            normalize_commit_message("Add dark mode toggle button"),
            "feat: add dark mode toggle button"
        );
        assert_eq!(
            normalize_commit_message("Added support for OAuth login"),
            "feat: add support for OAuth login"
        );
        assert_eq!(
            normalize_commit_message("Fix crash when clicking button"),
            "fix: crash when clicking button"
        );
        assert_eq!(
            normalize_commit_message("Avoid main status for worktree sessions"),
            "fix: avoid main status for worktree sessions"
        );
        assert_eq!(
            normalize_commit_message("Prevent memory leak in terminal"),
            "fix: prevent memory leak in terminal"
        );
        assert_eq!(
            normalize_commit_message("Update dependencies to latest versions"),
            "chore: update dependencies to latest versions"
        );
        assert_eq!(
            normalize_commit_message("Bump version to 0.1.16"),
            "chore: bump version to 0.1.16"
        );
        assert_eq!(
            normalize_commit_message("Refactor right panel view components"),
            "refactor: right panel view components"
        );
        assert_eq!(
            normalize_commit_message("Update README with installation instructions"),
            "docs: update README with installation instructions"
        );
        assert_eq!(
            normalize_commit_message("Add unit tests for commit normalization"),
            "test: add unit tests for commit normalization"
        );
        assert_eq!(
            normalize_commit_message("Improve performance of diff rendering"),
            "perf: improve performance of diff rendering"
        );
        assert_eq!(
            normalize_commit_message("Initial commit of the project"),
            "chore: initial commit of the project"
        );
        assert_eq!(
            normalize_commit_message("🔧Fix crash in parser"),
            "fix: crash in parser"
        );
        assert_eq!(
            normalize_commit_message("[fix] crash in parser"),
            "fix: crash in parser"
        );
        assert_eq!(
            normalize_commit_message("[refactor] simplify view component"),
            "refactor: simplify view component"
        );
    }

    #[test]
    fn preserves_acronyms_and_truncates_at_72_chars() {
        assert_eq!(
            normalize_commit_message("feat: support PTY terminal and URL parsing"),
            "feat: support PTY terminal and URL parsing"
        );
        assert_eq!(
            normalize_commit_message("fix: macOS window resize handling"),
            "fix: macOS window resize handling"
        );
        let long = "feat: implement comprehensive support for nested workspace directory scanning in right panel review tab";
        let normalized = normalize_commit_message(long);
        assert!(normalized.chars().count() <= 72);
        assert!(normalized.starts_with("feat: implement comprehensive support for nested workspace directory"));
        assert!(!normalized.ends_with('.'));

        let scoped_long = "feat(this-is-a-long-scope-name-that-is-about-sixty-chars-long): add x";
        let norm_scoped = normalize_commit_message(scoped_long);
        assert!(norm_scoped.chars().count() <= 72);
        assert!(norm_scoped.contains(": "));
        assert!(norm_scoped.ends_with("add x"));

        let very_long_scope = "feat(super-long-scope-name-that-is-way-too-long-and-keeps-going-and-going-beyond-the-limit): add authentication system";
        let norm_very_long = normalize_commit_message(very_long_scope);
        assert!(norm_very_long.chars().count() <= 72);
        assert!(norm_very_long.contains(": "));
        assert!(!norm_very_long.split(": ").nth(1).unwrap().is_empty());
    }
}
