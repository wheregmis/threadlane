use crate::types::AgentMessage;

const AUTO_COMPACTION_THRESHOLD_TOKENS: usize = 96_000;
pub(crate) const AUTO_COMPACTION_KEEP_RECENT_TOKENS: usize = 20_000;
const MAX_CHECKPOINT_CHARS: usize = 12_000;
const ESTIMATED_IMAGE_TOKENS: usize = 1_200;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompactionStrategy {
    #[default]
    TokenBudget,
    SemanticKeyframes,
}

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

fn estimate_message_tokens(message: &AgentMessage) -> usize {
    let chars = match message {
        AgentMessage::System { content } | AgentMessage::User { content } => content.len(),
        AgentMessage::UserWithImages { content, images } => {
            return content.len().div_ceil(4) + images.len() * ESTIMATED_IMAGE_TOKENS;
        }
        AgentMessage::Assistant {
            content,
            tool_calls,
            ..
        } => {
            content.as_deref().map_or(0, str::len)
                + tool_calls.as_ref().map_or(0, |calls| {
                    calls
                        .iter()
                        .map(|call| {
                            call.id.len()
                                + call.r#type.len()
                                + call.function.name.len()
                                + call.function.arguments.len()
                                + call.thought_signature.as_deref().map_or(0, str::len)
                        })
                        .sum()
                })
        }
        AgentMessage::Tool { name, content, .. } => name.len() + content.len(),
        AgentMessage::Custom { payload, .. } => payload.to_string().len(),
    };
    chars.div_ceil(4)
}

fn estimate_context_tokens(messages: &[AgentMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

pub(crate) fn should_auto_compact(messages: &[AgentMessage]) -> bool {
    estimate_context_tokens(messages) > AUTO_COMPACTION_THRESHOLD_TOKENS
}

pub(crate) fn is_context_overflow_error(error: &str) -> bool {
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

pub(crate) fn compact_messages_to_token_budget(
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

    let system_messages: Vec<_> = messages
        .iter()
        .filter(|message| matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect();
    let dropped: Vec<_> = messages[..start]
        .iter()
        .filter(|message| !matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect();

    if dropped.is_empty() {
        return messages.to_vec();
    }

    let mut compacted = Vec::new();
    compacted.extend(system_messages);
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

pub fn compact_messages_with_strategy(
    messages: &[AgentMessage],
    target_tokens: usize,
    strategy: CompactionStrategy,
) -> Vec<AgentMessage> {
    match strategy {
        CompactionStrategy::TokenBudget => {
            compact_messages_to_token_budget(messages, target_tokens)
        }
        CompactionStrategy::SemanticKeyframes => {
            if messages.len() <= 2 {
                return messages.to_vec();
            }
            let mut keyframes = Vec::new();
            let mut user_keyframes = 0;
            for (idx, msg) in messages.iter().enumerate() {
                if idx == 0 && matches!(msg, AgentMessage::System { .. }) {
                    keyframes.push(msg.clone());
                } else if matches!(
                    msg,
                    AgentMessage::User { .. } | AgentMessage::UserWithImages { .. }
                ) && user_keyframes < 3
                {
                    keyframes.push(msg.clone());
                    user_keyframes += 1;
                }
            }
            let keyframe_tokens: usize = keyframes.iter().map(estimate_message_tokens).sum();
            let remaining_budget = target_tokens.saturating_sub(keyframe_tokens);

            let recent = compact_messages_to_token_budget(messages, remaining_budget);
            let mut result = keyframes;
            let mut result_json: std::collections::HashSet<String> = result
                .iter()
                .filter_map(|m| serde_json::to_string(m).ok())
                .collect();
            for msg in recent {
                if let Ok(json) = serde_json::to_string(&msg) {
                    if result_json.insert(json) {
                        result.push(msg);
                    }
                }
            }
            result
        }
    }
}

/// Squeezes historical tool outputs older than `keep_recent_tool_turns` to save input tokens.
pub fn prune_historical_tool_outputs(
    messages: &[AgentMessage],
    keep_recent_tool_turns: usize,
) -> Vec<AgentMessage> {
    const INLINE_TOOL_OUTPUT_LIMIT: usize = 200;
    let mut tool_seen_count = 0;
    let mut result = Vec::with_capacity(messages.len());

    let mut keep_full = vec![false; messages.len()];
    for (i, msg) in messages.iter().enumerate().rev() {
        if matches!(msg, AgentMessage::Tool { .. }) {
            tool_seen_count += 1;
            if tool_seen_count <= keep_recent_tool_turns {
                keep_full[i] = true;
            }
        }
    }

    for (i, msg) in messages.iter().enumerate() {
        match msg {
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                terminate,
            } => {
                if keep_full[i] || content.len() <= INLINE_TOOL_OUTPUT_LIMIT {
                    result.push(msg.clone());
                } else {
                    let pruned_content = format!(
                        "[Historical tool output truncated for '{name}' ({} bytes)]",
                        content.len()
                    );
                    result.push(AgentMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        name: name.clone(),
                        content: pruned_content,
                        is_error: *is_error,
                        terminate: *terminate,
                    });
                }
            }
            _ => result.push(msg.clone()),
        }
    }

    result
}

/// Prepares a token-optimal message context for model invocation by squeezing historical tool outputs
/// and applying semantic keyframe compaction.
pub fn prepare_token_optimal_context(
    messages: &[AgentMessage],
    target_tokens: usize,
) -> Vec<AgentMessage> {
    let pruned = prune_historical_tool_outputs(messages, 3);
    compact_messages_with_strategy(
        &pruned,
        target_tokens,
        CompactionStrategy::SemanticKeyframes,
    )
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
        AgentMessage::Tool { name, content, .. } => {
            let truncated_content = if content.len() > 400 {
                let head: String = content.chars().take(200).collect();
                let tail_chars: Vec<char> = content.chars().rev().take(150).collect();
                let tail: String = tail_chars.into_iter().rev().collect();
                format!("{head} ... [truncated] ... {tail}")
            } else {
                content.clone()
            };
            Some(format!("Tool {name}: {truncated_content}"))
        }
        AgentMessage::Custom { .. } => compaction_summary_text(message).map(str::to_string),
        AgentMessage::System { .. } => None,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn extract_session_insights(messages: &[AgentMessage]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut verification = Vec::new();
    let mut gotchas = Vec::new();
    let mut architecture = Vec::new();

    for msg in messages {
        match msg {
            AgentMessage::Tool {
                name,
                content,
                is_error,
                ..
            } => {
                if name == "run_command" {
                    if content.contains("cargo test") && !content.contains("error:") {
                        let line = "cargo test --workspace";
                        if !verification.contains(&line.to_string()) {
                            verification.push(line.to_string());
                        }
                    }
                    if content.contains("cargo check") && !content.contains("error:") {
                        let line = "cargo check -p threadlane";
                        if !verification.contains(&line.to_string()) {
                            verification.push(line.to_string());
                        }
                    }
                }
                if *is_error
                    && (content.contains("Access denied")
                        || content.contains("Operation not permitted"))
                {
                    let line = "Command execution in restricted environments may require BypassSandbox mode.";
                    if !gotchas.contains(&line.to_string()) {
                        gotchas.push(line.to_string());
                    }
                }
            }
            AgentMessage::Assistant {
                content: Some(text),
                ..
            } if text.contains("Makepad") && text.contains("theme") => {
                let line = "UI components in crates/threadlane/src must reference theme tokens from crates/threadlane/src/theme/mod.rs.";
                if !architecture.contains(&line.to_string()) {
                    architecture.push(line.to_string());
                }
            }
            _ => {}
        }
    }

    (architecture, gotchas, verification)
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
            stop_reason: None,
            deferred_handle: None,
        });
        msgs.push(AgentMessage::Tool {
            tool_call_id: "call_1".into(),
            name: "read_file".into(),
            content: "x".repeat(1_000),
            is_error: false,
            terminate: false,
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

    #[test]
    fn test_extract_session_insights() {
        let msgs = vec![
            AgentMessage::Tool {
                tool_call_id: "1".into(),
                name: "run_command".into(),
                content: "running cargo test ... finished cleanly".into(),
                is_error: false,
                terminate: false,
            },
            AgentMessage::Assistant {
                content: Some("Makepad theme tokens must be used.".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
        ];

        let (arch, _gotchas, verify) = extract_session_insights(&msgs);
        assert!(!arch.is_empty());
        assert!(!verify.is_empty());
        assert!(verify.contains(&"cargo test --workspace".to_string()));
    }

    #[test]
    fn test_semantic_keyframe_compaction() {
        let msgs = vec![
            AgentMessage::System {
                content: "System Goal".into(),
            },
            AgentMessage::User {
                content: "Initial User Goal".into(),
            },
            AgentMessage::Assistant {
                content: Some("Intermediate reasoning".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::User {
                content: "Latest prompt".into(),
            },
        ];

        let compacted =
            compact_messages_with_strategy(&msgs, 200, CompactionStrategy::SemanticKeyframes);
        assert!(!compacted.is_empty());
        assert_eq!(compacted[0].role_str(), "system");
    }

    #[test]
    fn test_prune_historical_tool_outputs_and_optimal_context() {
        let mut msgs = vec![
            AgentMessage::System {
                content: "system prompt".into(),
            },
            AgentMessage::User {
                content: "initial goal prompt".into(),
            },
        ];

        for i in 0..10 {
            msgs.push(AgentMessage::Assistant {
                content: Some(format!("step {i}")),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            });
            msgs.push(AgentMessage::Tool {
                tool_call_id: format!("call_{i}"),
                name: "view_file".into(),
                content: "a".repeat(5_000),
                is_error: false,
                terminate: false,
            });
        }

        let pruned = prune_historical_tool_outputs(&msgs, 3);
        assert_eq!(pruned.len(), msgs.len());

        let full_count = pruned
            .iter()
            .filter(
                |m| matches!(m, AgentMessage::Tool { content, .. } if content.contains("aaaaa")),
            )
            .count();
        assert_eq!(full_count, 3);

        let truncated_count = pruned
            .iter()
            .filter(|m| matches!(m, AgentMessage::Tool { content, .. } if content.contains("Historical tool output truncated")))
            .count();
        assert_eq!(truncated_count, 7);

        let optimal = prepare_token_optimal_context(&msgs, 10_000);
        assert!(!optimal.is_empty());
        assert_eq!(optimal[0].role_str(), "system");
    }
}
