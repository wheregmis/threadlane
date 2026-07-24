//! Title generation payloads and response parsing.

use serde_json::Value;
use std::time::Duration;

pub const TITLE_SYSTEM_PROMPT: &str =
    "Return only a concise session title, maximum 42 Unicode characters, with no Markdown or explanation.";
pub const TITLE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub fn title_payload(model: &str, prompt: &str, is_codex: bool) -> Value {
    if is_codex {
        serde_json::json!({
            "model": model,
            "instructions": TITLE_SYSTEM_PROMPT,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": prompt}]
            }],
            "store": false,
            "stream": true
        })
    } else {
        serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": TITLE_SYSTEM_PROMPT},
                {"role": "user", "content": prompt}
            ],
            "stream": false
        })
    }
}

pub fn title_response_text(value: &Value) -> Result<String, String> {
    let text = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| {
            content.as_str().map(str::to_string).or_else(|| {
                content.as_array().map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect()
                })
            })
        })
        .or_else(|| {
            value
                .get("output_text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            value.get("output").and_then(Value::as_array).map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
                    .flat_map(|item| {
                        item.get("content")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                    })
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<String>()
            })
        })
        .ok_or_else(|| "OpenAI title response did not contain text".to_string())?;

    if text.trim().is_empty() {
        Err("OpenAI title response contained empty text".to_string())
    } else {
        Ok(text)
    }
}

fn api_error_details(value: &Value) -> (String, String) {
    let err = value.get("error").unwrap_or(value);
    let code = err
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| err.get("type").and_then(Value::as_str))
        .unwrap_or("unknown_error")
        .to_string();
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or("No error message provided")
        .to_string();
    (code, message)
}

pub fn title_stream_text(body: &str) -> Result<String, String> {
    let mut streamed_text = String::new();
    let mut terminal_text = None;

    for block in body.split("\n\n") {
        let data = block
            .lines()
            .filter_map(|line| line.trim().strip_prefix("data:"))
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(&data)
            .map_err(|error| format!("Failed to parse OpenAI title stream ({error}): {data}"))?;
        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        if event_type == "error" || event_type == "response.failed" || value.get("error").is_some()
        {
            let (code, message) = api_error_details(&value);
            return Err(format!("OpenAI title stream failed [{code}]: {message}"));
        }
        if event_type == "response.output_text.delta" {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                streamed_text.push_str(delta);
            }
        }
        if matches!(
            event_type,
            "response.completed" | "response.done" | "response.incomplete"
        ) {
            let response = value.get("response").unwrap_or(&value);
            terminal_text = title_response_text(response).ok();
        }
    }

    terminal_text
        .or_else(|| (!streamed_text.trim().is_empty()).then_some(streamed_text))
        .ok_or_else(|| "OpenAI title stream did not contain text".to_string())
}
