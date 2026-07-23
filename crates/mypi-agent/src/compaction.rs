use crate::types::AgentMessage;

pub const AUTO_COMPACTION_THRESHOLD_TOKENS: usize = 96_000;
pub const AUTO_COMPACTION_KEEP_RECENT_TOKENS: usize = 20_000;
const MAX_CHECKPOINT_CHARS: usize = 12_000;
const ESTIMATED_IMAGE_TOKENS: usize = 1_200;

#[derive(Debug, Clone)]
pub struct CompactionOptions {
    pub max_messages: usize,
    pub preserve_recent: usize,
}

impl Default for CompactionOptions {
    fn default() -> Self {
        Self {
            max_messages: 50,
            preserve_recent: 10,
        }
    }
}

pub fn estimate_message_tokens(message: &AgentMessage) -> usize {
    let chars = match message {
        AgentMessage::System { content } | AgentMessage::User { content } => content.len(),
        AgentMessage::UserWithImages { content, images } => {
            return content.len().div_ceil(4) + images.len() * ESTIMATED_IMAGE_TOKENS;
        }
        AgentMessage::Assistant {
            content,
            tool_calls,
        } => {
            content.as_deref().map_or(0, str::len)
                + tool_calls
                    .as_ref()
                    .and_then(|calls| serde_json::to_string(calls).ok())
                    .map_or(0, |calls| calls.len())
        }
        AgentMessage::Tool { name, content, .. } => name.len() + content.len(),
        AgentMessage::Custom { payload, .. } => payload.to_string().len(),
    };
    chars.div_ceil(4)
}

pub fn estimate_context_tokens(messages: &[AgentMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

pub fn should_auto_compact(messages: &[AgentMessage]) -> bool {
    estimate_context_tokens(messages) > AUTO_COMPACTION_THRESHOLD_TOKENS
}

pub fn is_context_overflow_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("context_length_exceeded")
        || error.contains("context length exceeded")
        || error.contains("maximum context length")
        || error.contains("input exceeds the context window")
        || error.contains("too many tokens")
}

pub fn compaction_summary_text(message: &AgentMessage) -> Option<&str> {
    let AgentMessage::Custom {
        custom_type,
        payload,
    } = message
    else {
        return None;
    };
    if custom_type != "compaction_summary" {
        return None;
    }
    payload.get("summary").and_then(serde_json::Value::as_str)
}

pub fn compact_messages(
    messages: &[AgentMessage],
    options: &CompactionOptions,
) -> Vec<AgentMessage> {
    if messages.len() <= options.max_messages {
        return messages.to_vec();
    }

    let keep_count = options.preserve_recent.min(messages.len());
    compact_from_index(messages, messages.len().saturating_sub(keep_count))
}

pub fn compact_messages_to_token_budget(
    messages: &[AgentMessage],
    keep_recent_tokens: usize,
) -> Vec<AgentMessage> {
    if messages.len() <= 2 {
        return messages.to_vec();
    }

    let mut tokens = 0;
    let mut start = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        if matches!(message, AgentMessage::System { .. }) {
            continue;
        }
        tokens += estimate_message_tokens(message);
        start = index;
        if tokens >= keep_recent_tokens {
            break;
        }
    }

    // A tool result must never be sent without the assistant tool call that created it.
    while start > 0 && matches!(messages[start], AgentMessage::Tool { .. }) {
        start -= 1;
    }

    compact_from_index(messages, start)
}

fn compact_from_index(messages: &[AgentMessage], mut start: usize) -> Vec<AgentMessage> {
    while start < messages.len() && matches!(messages[start], AgentMessage::System { .. }) {
        start += 1;
    }

    let system = messages
        .iter()
        .find(|message| matches!(message, AgentMessage::System { .. }))
        .cloned();
    let dropped: Vec<_> = messages[..start]
        .iter()
        .filter(|message| !matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect();

    if dropped.is_empty() {
        return messages.to_vec();
    }

    let mut compacted = Vec::new();
    if let Some(system) = system {
        compacted.push(system);
    }
    compacted.push(AgentMessage::Custom {
        custom_type: "compaction_summary".to_string(),
        payload: serde_json::json!({
            "summary": build_checkpoint(&dropped),
            "compacted_messages": dropped.len(),
        }),
    });
    compacted.extend(
        messages[start..]
            .iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .cloned(),
    );
    compacted
}

fn build_checkpoint(messages: &[AgentMessage]) -> String {
    let mut excerpts = Vec::new();
    let mut used_chars = 0;

    for message in messages.iter().rev() {
        let Some(excerpt) = message_excerpt(message) else {
            continue;
        };
        if used_chars + excerpt.len() > MAX_CHECKPOINT_CHARS {
            break;
        }
        used_chars += excerpt.len();
        excerpts.push(excerpt);
    }
    excerpts.reverse();

    format!(
        "Context checkpoint from {} earlier messages. Continue the same task using the retained recent messages and these earlier excerpts:\n\n{}",
        messages.len(),
        excerpts.join("\n\n")
    )
}

fn message_excerpt(message: &AgentMessage) -> Option<String> {
    match message {
        AgentMessage::User { content } => Some(format!("User: {content}")),
        AgentMessage::UserWithImages { content, images } => Some(format!(
            "User: {content}\n[{} image attachment(s)]",
            images.len()
        )),
        AgentMessage::Assistant { content, .. } => content
            .as_ref()
            .filter(|content| !content.trim().is_empty())
            .map(|content| format!("Assistant: {content}")),
        AgentMessage::Tool { name, content, .. } => Some(format!("Tool {name}: {content}")),
        AgentMessage::Custom { .. } => compaction_summary_text(message).map(str::to_string),
        AgentMessage::System { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_messages() {
        let mut msgs = vec![AgentMessage::System {
            content: "System prompt".into(),
        }];
        for i in 0..100 {
            msgs.push(AgentMessage::User {
                content: format!("User message {i}"),
            });
        }

        let compacted = compact_messages(
            &msgs,
            &CompactionOptions {
                max_messages: 20,
                preserve_recent: 5,
            },
        );
        assert!(compacted.len() <= 10);
        assert_eq!(compacted[0].role_str(), "system");
        assert!(compaction_summary_text(&compacted[1]).is_some());
    }

    #[test]
    fn token_compaction_keeps_tool_call_before_tool_result() {
        let mut msgs = vec![AgentMessage::System {
            content: "system".into(),
        }];
        msgs.push(AgentMessage::User {
            content: "older request".into(),
        });
        msgs.push(AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![]),
        });
        msgs.push(AgentMessage::Tool {
            tool_call_id: "call_1".into(),
            name: "read_file".into(),
            content: "x".repeat(1_000),
            is_error: false,
        });

        let compacted = compact_messages_to_token_budget(&msgs, 1);
        assert!(matches!(compacted[2], AgentMessage::Assistant { .. }));
        assert!(matches!(compacted[3], AgentMessage::Tool { .. }));
    }

    #[test]
    fn detects_provider_context_overflow_errors() {
        assert!(is_context_overflow_error(
            "OpenAI SSE Error [context_length_exceeded]: input exceeds the context window"
        ));
        assert!(!is_context_overflow_error("rate limit exceeded"));
    }
}
