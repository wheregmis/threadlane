use crate::antigravity::AntigravityClient;
use crate::openai::{OpenAIClient, StreamEvent};
use crate::opencode::OpenCodeGoClient;
use crate::title_generator::{title_payload, TITLE_REQUEST_TIMEOUT};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

const ANTIGRAVITY_MODEL_PREFIX: &str = "antigravity/";
const OPENCODE_MODEL_PREFIX: &str = "opencode-go/";

pub fn is_antigravity_model(model: &str) -> bool {
    model.starts_with(ANTIGRAVITY_MODEL_PREFIX)
}

pub fn is_opencode_model(model: &str) -> bool {
    model.starts_with(OPENCODE_MODEL_PREFIX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    ChatCompletions,
    Codex,
}

pub type LazyPayloadBuilder = Arc<dyn Fn(PayloadFormat) -> BoxFuture<'static, Value> + Send + Sync>;

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
    pub fn lazy<F>(model: impl Into<String>, builder: F) -> Self
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

#[derive(Clone)]
pub struct ProviderClient {
    openai: OpenAIClient,
    antigravity: AntigravityClient,
    opencode: OpenCodeGoClient,
}

impl ProviderClient {
    pub fn new(api_key: impl Into<String>, account_id: Option<String>) -> Self {
        Self {
            openai: OpenAIClient::new(api_key.into(), account_id),
            antigravity: AntigravityClient::new(),
            opencode: OpenCodeGoClient::new(),
        }
    }

    pub fn determine_format(&self, model: &str) -> PayloadFormat {
        if is_antigravity_model(model) || is_opencode_model(model) {
            PayloadFormat::ChatCompletions
        } else if self.openai.is_codex() {
            PayloadFormat::Codex
        } else {
            PayloadFormat::ChatCompletions
        }
    }

    pub async fn stream_chat_completion(
        &self,
        payload_source: impl Into<PayloadSource>,
        prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let source = payload_source.into();
        let model = source.model().to_string();

        let provider: Arc<dyn crate::traits::ModelProvider> = if is_antigravity_model(&model) {
            Arc::new(self.antigravity.clone())
        } else if is_opencode_model(&model) {
            Arc::new(self.opencode.clone())
        } else {
            Arc::new(self.openai.clone())
        };

        provider
            .stream_chat_completion(source, prompt_cache_key, event_tx)
            .await;
    }

    pub async fn fetch_deferred(
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

    pub async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String> {
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
}
