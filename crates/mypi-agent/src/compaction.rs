use crate::types::AgentMessage;

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

pub fn compact_messages(
    messages: &[AgentMessage],
    options: &CompactionOptions,
) -> Vec<AgentMessage> {
    if messages.len() <= options.max_messages {
        return messages.to_vec();
    }

    let mut compacted = Vec::new();
    let mut system_msg = None;

    for msg in messages {
        if matches!(msg, AgentMessage::System { .. }) && system_msg.is_none() {
            system_msg = Some(msg.clone());
        }
    }

    if let Some(sys) = system_msg {
        compacted.push(sys);
    }

    let keep_count = options.preserve_recent.min(messages.len());
    let start_idx = messages.len().saturating_sub(keep_count);

    let dropped_count = start_idx.saturating_sub(1);
    if dropped_count > 0 {
        compacted.push(AgentMessage::Custom {
            custom_type: "compaction_summary".to_string(),
            payload: serde_json::json!({
                "summary": format!("[Compacted {} previous messages]", dropped_count)
            }),
        });
    }

    for msg in &messages[start_idx..] {
        if !matches!(msg, AgentMessage::System { .. }) {
            compacted.push(msg.clone());
        }
    }

    compacted
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
                content: format!("User message {}", i),
            });
        }

        let opts = CompactionOptions {
            max_messages: 20,
            preserve_recent: 5,
        };

        let compacted = compact_messages(&msgs, &opts);
        assert!(compacted.len() <= 10);
        assert_eq!(compacted[0].role_str(), "system");
        assert_eq!(compacted[1].role_str(), "custom");
    }
}
