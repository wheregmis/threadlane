use crate::openai::StreamEvent;
use crate::router::PayloadSource;
use async_trait::async_trait;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferredResponse {
    Pending,
    Ready { content: String },
    Error { message: String },
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Identifier for the model provider (e.g., "openai", "codex", "antigravity").
    fn provider_id(&self) -> &'static str;

    /// Checks if this provider handles the given model identifier.
    fn supports_model(&self, model: &str) -> bool;

    /// Streams a chat completion response over `event_tx`.
    async fn stream_chat_completion(
        &self,
        payload_source: PayloadSource,
        prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    );

    /// Fetches a previously accepted deferred response without starting a new
    /// model request. Providers that do not expose deferred responses keep the
    /// safe unsupported default.
    async fn fetch_deferred(&self, _handle_id: &str) -> Result<DeferredResponse, String> {
        Err(format!(
            "provider {} does not support deferred responses",
            self.provider_id()
        ))
    }

    /// Best-effort cancellation for a previously accepted deferred response.
    async fn cancel_deferred(&self, _handle_id: &str) -> Result<(), String> {
        Err(format!(
            "provider {} does not support deferred responses",
            self.provider_id()
        ))
    }
}
