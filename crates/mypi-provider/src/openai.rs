use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::mpsc;

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

#[derive(Debug, Clone)]
pub enum StreamEvent {
    ContentToken(String),
    ToolCallStart { name: String },
    ToolCallArgsDelta { args_chunk: String },
    Finished { tool_calls: Vec<ToolCall> },
    Error(String),
}

pub async fn fetch_available_models(api_key: &str, account_id: Option<&str>) -> Vec<String> {
    let client = reqwest::Client::new();
    let mut req = client
        .get("https://api.openai.com/v1/models")
        .header(AUTHORIZATION, format!("Bearer {api_key}"));

    if let Some(acc_id) = account_id {
        req = req.header("chatgpt-account-id", acc_id);
    }

    if let Ok(res) = req.send().await {
        if res.status().is_success() {
            if let Ok(val) = res.json::<Value>().await {
                if let Some(data) = val.get("data").and_then(|d| d.as_array()) {
                    let mut models = Vec::new();
                    for item in data {
                        if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                            if id.starts_with("gpt-")
                                || id.starts_with("o1")
                                || id.starts_with("o3")
                                || id.contains("codex")
                            {
                                models.push(id.to_string());
                            }
                        }
                    }
                    if !models.is_empty() {
                        models.sort();
                        return models;
                    }
                }
            }
        }
    }

    vec![
        "gpt-5.4".to_string(),
        "gpt-5.4-mini".to_string(),
        "gpt-5.5".to_string(),
        "gpt-5.6-luna".to_string(),
        "gpt-5.6-sol".to_string(),
        "gpt-5.6-terra".to_string(),
        "gpt-5.3-codex-spark".to_string(),
        "gpt-4o".to_string(),
        "gpt-4o-mini".to_string(),
    ]
}

pub struct OpenAIClient {
    api_key: String,
    account_id: Option<String>,
    client: reqwest::Client,
}

impl OpenAIClient {
    pub fn new(api_key: String, account_id: Option<String>) -> Self {
        Self {
            api_key,
            account_id,
            client: reqwest::Client::new(),
        }
    }

    pub async fn stream_chat_completion(
        &self,
        api_payload: Value,
        codex_payload: Value,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let is_codex = self.account_id.is_some() || self.api_key.starts_with("ey");

        let (url, payload) = if is_codex {
            (
                "https://chatgpt.com/backend-api/codex/responses".to_string(),
                codex_payload,
            )
        } else {
            (
                "https://api.openai.com/v1/chat/completions".to_string(),
                api_payload,
            )
        };

        let mut req = self
            .client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json");

        if let Some(ref acc_id) = self.account_id {
            req = req.header("chatgpt-account-id", acc_id.clone());
        }

        let res = match req.json(&payload).send().await {
            Ok(r) => r,
            Err(e) => {
                let _ = event_tx
                    .send(StreamEvent::Error(format!("HTTP request error: {e}")))
                    .await;
                return;
            }
        };

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            let _ = event_tx
                .send(StreamEvent::Error(format!(
                    "OpenAI API error ({status}): {body}"
                )))
                .await;
            return;
        }

        let mut stream = res.bytes_stream();
        let mut buffer = String::new();
        let mut active_tool_calls: HashMap<usize, (String, String, String)> = HashMap::new();
        let mut codex_tool_indices: HashMap<String, usize> = HashMap::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk: Bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    let _ = event_tx
                        .send(StreamEvent::Error(format!("Stream reading error: {e}")))
                        .await;
                    return;
                }
            };

            let text = String::from_utf8_lossy(&chunk);
            buffer.push_str(&text);

            while let Some(pos) = buffer.find("\n\n") {
                let sse_block = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                for line in sse_block.lines() {
                    let line = line.trim();
                    if line.starts_with("data: ") {
                        let data_str = &line[6..];
                        if data_str == "[DONE]" {
                            break;
                        }

                        if let Ok(v) = serde_json::from_str::<Value>(data_str) {
                            let event_type =
                                v.get("type").and_then(|value| value.as_str()).unwrap_or("");

                            // Codex Responses API emits function calls as output items and
                            // their JSON arguments as a separate delta event. These must not
                            // be rendered as assistant text.
                            if event_type == "response.output_item.added"
                                || event_type == "response.output_item.done"
                            {
                                if let Some(item) = v.get("item") {
                                    if item.get("type").and_then(|value| value.as_str())
                                        == Some("function_call")
                                    {
                                        let index = v
                                            .get("output_index")
                                            .and_then(|value| value.as_u64())
                                            .unwrap_or(active_tool_calls.len() as u64)
                                            as usize;
                                        let item_id = item
                                            .get("id")
                                            .and_then(|value| value.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        if !item_id.is_empty() {
                                            codex_tool_indices.insert(item_id, index);
                                        }
                                        let entry = active_tool_calls.entry(index).or_insert((
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                        ));
                                        entry.0 = item
                                            .get("call_id")
                                            .or_else(|| item.get("id"))
                                            .and_then(|value| value.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        entry.1 = item
                                            .get("name")
                                            .and_then(|value| value.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        if let Some(arguments) =
                                            item.get("arguments").and_then(|value| value.as_str())
                                        {
                                            entry.2 = arguments.to_string();
                                        }
                                        if !entry.1.is_empty() {
                                            let _ = event_tx
                                                .send(StreamEvent::ToolCallStart {
                                                    name: entry.1.clone(),
                                                })
                                                .await;
                                        }
                                    }
                                }
                            } else if event_type == "response.function_call_arguments.delta" {
                                let index = v
                                    .get("output_index")
                                    .and_then(|value| value.as_u64())
                                    .map(|value| value as usize)
                                    .or_else(|| {
                                        v.get("item_id")
                                            .and_then(|value| value.as_str())
                                            .and_then(|id| codex_tool_indices.get(id).copied())
                                    })
                                    .unwrap_or(0);
                                if let Some(delta) = v.get("delta").and_then(|value| value.as_str())
                                {
                                    active_tool_calls
                                        .entry(index)
                                        .or_insert((String::new(), String::new(), String::new()))
                                        .2
                                        .push_str(delta);
                                    let _ = event_tx
                                        .send(StreamEvent::ToolCallArgsDelta {
                                            args_chunk: delta.to_string(),
                                        })
                                        .await;
                                }
                            } else if event_type == "response.output_text.delta" {
                                if let Some(delta) = v.get("delta").and_then(|value| value.as_str())
                                {
                                    let _ = event_tx
                                        .send(StreamEvent::ContentToken(delta.to_string()))
                                        .await;
                                }
                            // 1. Handle remaining Codex text delta fields
                            } else if let Some(delta) = v.get("delta") {
                                let token = if let Some(s) = delta.as_str() {
                                    Some(s.to_string())
                                } else if let Some(s) = delta.get("text").and_then(|t| t.as_str()) {
                                    Some(s.to_string())
                                } else if let Some(s) =
                                    delta.get("content").and_then(|c| c.as_str())
                                {
                                    Some(s.to_string())
                                } else {
                                    None
                                };

                                if let Some(t) = token {
                                    if !t.is_empty() {
                                        let _ = event_tx.send(StreamEvent::ContentToken(t)).await;
                                    }
                                }
                            }

                            // 2. Handle Codex output_text / text fields
                            if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    let _ = event_tx
                                        .send(StreamEvent::ContentToken(text.to_string()))
                                        .await;
                                }
                            }

                            // 3. Handle Standard API completions choices array
                            if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                                if let Some(first) = choices.first() {
                                    if let Some(delta) = first.get("delta") {
                                        if let Some(content) =
                                            delta.get("content").and_then(|c| c.as_str())
                                        {
                                            if !content.is_empty() {
                                                let _ = event_tx
                                                    .send(StreamEvent::ContentToken(
                                                        content.to_string(),
                                                    ))
                                                    .await;
                                            }
                                        }

                                        if let Some(t_calls) =
                                            delta.get("tool_calls").and_then(|tc| tc.as_array())
                                        {
                                            for tc in t_calls {
                                                let index = tc
                                                    .get("index")
                                                    .and_then(|i| i.as_u64())
                                                    .unwrap_or(0)
                                                    as usize;
                                                let entry =
                                                    active_tool_calls.entry(index).or_insert((
                                                        String::new(),
                                                        String::new(),
                                                        String::new(),
                                                    ));

                                                if let Some(id) =
                                                    tc.get("id").and_then(|i| i.as_str())
                                                {
                                                    entry.0 = id.to_string();
                                                }
                                                if let Some(func) = tc.get("function") {
                                                    if let Some(name) =
                                                        func.get("name").and_then(|n| n.as_str())
                                                    {
                                                        entry.1 = name.to_string();
                                                        let _ = event_tx
                                                            .send(StreamEvent::ToolCallStart {
                                                                name: name.to_string(),
                                                            })
                                                            .await;
                                                    }
                                                    if let Some(args_chunk) = func
                                                        .get("arguments")
                                                        .and_then(|a| a.as_str())
                                                    {
                                                        entry.2.push_str(args_chunk);
                                                        let _ = event_tx
                                                            .send(StreamEvent::ToolCallArgsDelta {
                                                                args_chunk: args_chunk.to_string(),
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
                }
            }
        }

        let mut final_tool_calls = Vec::new();
        let mut indices: Vec<usize> = active_tool_calls.keys().cloned().collect();
        indices.sort();

        for idx in indices {
            if let Some((id, name, args)) = active_tool_calls.get(&idx) {
                final_tool_calls.push(ToolCall {
                    id: id.clone(),
                    r#type: "function".to_string(),
                    function: ToolCallFunction {
                        name: name.clone(),
                        arguments: args.clone(),
                    },
                });
            }
        }

        let _ = event_tx
            .send(StreamEvent::Finished {
                tool_calls: final_tool_calls,
            })
            .await;
    }
}
