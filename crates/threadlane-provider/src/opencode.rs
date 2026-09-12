use crate::openai::{ProviderUsage, StreamEvent, ToolCall, ToolCallFunction};
use crate::router::{PayloadFormat, PayloadSource};
use crate::traits::ModelProvider;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc;

const DEFAULT_OPENCODE_BASE_URL: &str = "https://opencode.ai/zen/go/v1";
const MODELS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Bare model ids shipped as an offline fallback. Live discovery via
/// [`fetch_available_models`] replaces this list whenever it succeeds, so new
/// Zen models appear without a code change.
const FALLBACK_MODELS: &[&str] = &[
    "mimo-v2.5-pro",
    "mimo-v2.5",
    "qwen3.8-max",
    "minimax-m3",
    "minimax-m2.7",
    "deepseek-v4-pro",
    "deepseek-v4-flash",
    "hy3",
];

struct ModelsCacheEntry {
    stored_at: Instant,
    models: Vec<String>,
}

static MODELS_CACHE: OnceLock<StdMutex<HashMap<u64, ModelsCacheEntry>>> = OnceLock::new();

fn models_cache_key(api_key: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    api_key.hash(&mut hasher);
    OpenCodeGoClient::get_base_url().hash(&mut hasher);
    hasher.finish()
}

fn strip_opencode_prefix(model: &str) -> &str {
    model.strip_prefix("opencode-go/").unwrap_or(model)
}

#[derive(Debug, Clone, Default)]
pub struct OpenCodeGoClient {
    client: reqwest::Client,
    api_key: Option<String>,
}

impl OpenCodeGoClient {
    pub(crate) fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: None,
        }
    }

    /// Injects the stored OpenCode API key. Without one, requests fall back
    /// to `OPENCODE_API_KEY`/`OPENCODE_GO_API_KEY` from the environment.
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        let key = api_key.into();
        self.api_key = (!key.trim().is_empty()).then_some(key);
        self
    }

    fn api_key(&self) -> Option<String> {
        self.api_key
            .clone()
            .filter(|key| !key.trim().is_empty())
            .or_else(|| {
                ["OPENCODE_API_KEY", "OPENCODE_GO_API_KEY"]
                    .into_iter()
                    .filter_map(|name| std::env::var(name).ok())
                    .map(|key| key.trim().to_string())
                    .find(|key| !key.is_empty())
            })
    }

    fn get_base_url() -> String {
        std::env::var("OPENCODE_BASE_URL").unwrap_or_else(|_| DEFAULT_OPENCODE_BASE_URL.to_string())
    }
}

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(reqwest::Client::new)
}

async fn fetch_available_models_network(
    api_key: String,
    cache_key: u64,
    now: Instant,
) -> Vec<String> {
    let base_url = OpenCodeGoClient::get_base_url();
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = http_client()
        .get(&url)
        .header(USER_AGENT, "threadlane/1.0")
        .timeout(std::time::Duration::from_secs(10));
    if !api_key.trim().is_empty() {
        request = request.header(AUTHORIZATION, format!("Bearer {api_key}"));
    }
    if let Ok(response) = request.send().await {
        if response.status().is_success() {
            if let Ok(value) = response.json::<Value>().await {
                if let Some(data) = value.get("data").and_then(Value::as_array) {
                    let mut models: Vec<String> = data
                        .iter()
                        .filter_map(|item| item.get("id").and_then(Value::as_str))
                        .filter(|id| !id.trim().is_empty())
                        .map(str::to_string)
                        .collect();
                    if !models.is_empty() {
                        models.sort();
                        models.dedup();
                        if let Ok(mut cache) = MODELS_CACHE
                            .get_or_init(|| StdMutex::new(HashMap::new()))
                            .lock()
                        {
                            cache.insert(
                                cache_key,
                                ModelsCacheEntry {
                                    stored_at: now,
                                    models: models.clone(),
                                },
                            );
                        }
                        return models;
                    }
                }
            }
        }
    }
    FALLBACK_MODELS
        .iter()
        .map(|model| model.to_string())
        .collect()
}

/// Lists bare Zen model ids from the OpenAI-compatible `/models` endpoint.
///
/// Results are cached per API key for [`MODELS_CACHE_TTL`]; failures fall back
/// to [`FALLBACK_MODELS`] so the picker keeps working offline. An empty key
/// returns the fallback list without a network round trip.
pub async fn fetch_available_models(api_key: &str) -> Vec<String> {
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return FALLBACK_MODELS
            .iter()
            .map(|model| model.to_string())
            .collect();
    }
    let cache_key = models_cache_key(&api_key);
    let now = Instant::now();
    if let Some(models) = MODELS_CACHE
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|cache| {
            cache
                .get(&cache_key)
                .map(|entry| (entry.stored_at, entry.models.clone()))
        })
        .filter(|(stored_at, _)| now.duration_since(*stored_at) <= MODELS_CACHE_TTL)
        .map(|(_, models)| models)
    {
        return models;
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        fetch_available_models_network(api_key, cache_key, now).await
    } else {
        let handle = crate::exec::get_runtime()
            .spawn(fetch_available_models_network(api_key, cache_key, now));
        match handle.await {
            Ok(models) => models,
            Err(_) => FALLBACK_MODELS
                .iter()
                .map(|model| model.to_string())
                .collect(),
        }
    }
}

#[async_trait]
impl ModelProvider for OpenCodeGoClient {
    fn provider_id(&self) -> &'static str {
        "opencode-go"
    }

    fn supports_model(&self, model: &str) -> bool {
        model.starts_with("opencode-go/")
    }

    async fn stream_chat_completion(
        &self,
        payload_source: PayloadSource,
        _prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let api_key = match self.api_key() {
            Some(key) => key,
            None => {
                let _ = event_tx
                    .send(StreamEvent::Error(
                        "No OpenCode API key provided. Set OPENCODE_API_KEY environment variable or save key in Settings.".to_string(),
                    ))
                    .await;
                return;
            }
        };

        let mut payload = payload_source.resolve(PayloadFormat::ChatCompletions).await;

        // Strip provider prefix from model string if present
        if let Some(model_str) = payload.get("model").and_then(Value::as_str) {
            let clean_model = strip_opencode_prefix(model_str).to_string();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("model".to_string(), Value::String(clean_model));
            }
        }

        // Ensure streaming is enabled
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("stream".to_string(), Value::Bool(true));
        }

        let base_url = Self::get_base_url();
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        let response = match self
            .client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "text/event-stream")
            .header(USER_AGENT, "threadlane/1.0")
            .header("x-opencode-client", "threadlane")
            .json(&payload)
            .send()
            .await
        {
            Ok(res) => res,
            Err(err) => {
                let _ = event_tx
                    .send(StreamEvent::Error(format!(
                        "OpenCode API request failed: {err}"
                    )))
                    .await;
                return;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let _ = event_tx
                .send(StreamEvent::Error(format!(
                    "OpenCode API error ({status}): {body}"
                )))
                .await;
            return;
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut tool_calls_map: HashMap<usize, (String, String, String)> = HashMap::new(); // index -> (id, name, args)
        let mut tool_names_announced = std::collections::HashSet::new();
        let mut usage = ProviderUsage::default();

        while let Some(chunk_result) = stream.next().await {
            let chunk = match chunk_result {
                Ok(bytes) => bytes,
                Err(err) => {
                    let _ = event_tx
                        .send(StreamEvent::Error(format!(
                            "Error reading stream chunk: {err}"
                        )))
                        .await;
                    return;
                }
            };

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer.drain(..=line_end);

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                let data = match line.strip_prefix("data: ") {
                    Some(d) => d.trim(),
                    None => continue,
                };

                if data == "[DONE]" {
                    break;
                }

                let val: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Check for usage stats
                if let Some(usage_val) = val.get("usage") {
                    let prompt_tokens = usage_val
                        .get("prompt_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default() as u32;
                    let completion_tokens = usage_val
                        .get("completion_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default() as u32;
                    let total_tokens = usage_val
                        .get("total_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or_default() as u32;

                    usage = ProviderUsage {
                        input_tokens: prompt_tokens,
                        output_tokens: completion_tokens,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        total_tokens: if total_tokens == 0 {
                            prompt_tokens.saturating_add(completion_tokens)
                        } else {
                            total_tokens
                        },
                    };
                }

                if let Some(choices) = val.get("choices").and_then(Value::as_array) {
                    for choice in choices {
                        let delta = match choice.get("delta") {
                            Some(d) => d,
                            None => continue,
                        };

                        // Content token
                        if let Some(content) = delta.get("content").and_then(Value::as_str) {
                            if !content.is_empty() {
                                let _ = event_tx
                                    .send(StreamEvent::ContentToken(content.to_string()))
                                    .await;
                            }
                        }

                        // Reasoning token
                        if let Some(reasoning) = delta
                            .get("reasoning_content")
                            .or_else(|| delta.get("reasoning"))
                            .and_then(Value::as_str)
                        {
                            if !reasoning.is_empty() {
                                let _ = event_tx
                                    .send(StreamEvent::ReasoningToken(reasoning.to_string()))
                                    .await;
                            }
                        }

                        // Tool calls
                        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array)
                        {
                            for tc in tool_calls {
                                let idx =
                                    tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;

                                let (ref mut id, ref mut name, ref mut args) =
                                    tool_calls_map.entry(idx).or_insert_with(|| {
                                        (String::new(), String::new(), String::new())
                                    });

                                if let Some(tc_id) = tc.get("id").and_then(Value::as_str) {
                                    id.clone_from(&tc_id.to_string());
                                }

                                if let Some(fn_obj) = tc.get("function") {
                                    if let Some(fn_name) =
                                        fn_obj.get("name").and_then(Value::as_str)
                                    {
                                        if !fn_name.is_empty() {
                                            name.clone_from(&fn_name.to_string());
                                            if !tool_names_announced.contains(fn_name) {
                                                tool_names_announced.insert(fn_name.to_string());
                                                let _ = event_tx
                                                    .send(StreamEvent::ToolCallStart {
                                                        name: fn_name.to_string(),
                                                    })
                                                    .await;
                                            }
                                        }
                                    }

                                    if let Some(fn_args) =
                                        fn_obj.get("arguments").and_then(Value::as_str)
                                    {
                                        if !fn_args.is_empty() {
                                            args.push_str(fn_args);
                                            let _ = event_tx
                                                .send(StreamEvent::ToolCallArgsDelta {
                                                    args_chunk: fn_args.to_string(),
                                                })
                                                .await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Assemble finished tool calls
        let mut final_tool_calls = Vec::new();
        let mut indices: Vec<_> = tool_calls_map.keys().cloned().collect();
        indices.sort_unstable();

        for idx in indices {
            if let Some((id, name, arguments)) = tool_calls_map.remove(&idx) {
                if !name.is_empty() {
                    final_tool_calls.push(ToolCall {
                        id,
                        r#type: "function".to_string(),
                        function: ToolCallFunction { name, arguments },
                        thought_signature: None,
                    });
                }
            }
        }

        let _ = event_tx
            .send(StreamEvent::Finished {
                tool_calls: final_tool_calls,
                usage,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_opencode_prefix() {
        assert_eq!(
            strip_opencode_prefix("opencode-go/deepseek-v4-flash"),
            "deepseek-v4-flash"
        );
        assert_eq!(
            strip_opencode_prefix("deepseek-v4-flash"),
            "deepseek-v4-flash"
        );
    }

    #[test]
    fn test_opencode_client_supports_model() {
        let client = OpenCodeGoClient::new();
        assert!(client.supports_model("opencode-go/deepseek-v4-flash"));
        assert!(client.supports_model("opencode-go/kimi-k2.6"));
        assert!(!client.supports_model("gpt-4o"));
        assert!(!client.supports_model("antigravity/gemini-3.6-flash"));
    }

    #[test]
    fn fallback_models_cover_all_seeds() {
        for model in FALLBACK_MODELS {
            assert!(!model.trim().is_empty());
        }
        assert!(FALLBACK_MODELS.contains(&"minimax-m2.7"));
    }
}
