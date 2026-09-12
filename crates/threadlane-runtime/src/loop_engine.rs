use threadlane_provider::convert::normalized_tool_call_id;
use crate::types::AgentMessage;
use std::collections::HashSet;

pub(crate) use crate::utils::AbortOnDrop;

/// Removes an assistant tool-call turn that was interrupted before every call
/// received a tool result. Provider APIs reject replaying such incomplete turns.
pub fn repair_interrupted_tool_turn(messages: &mut Vec<AgentMessage>) -> bool {
    let mut index = 0;
    while index < messages.len() {
        let AgentMessage::Assistant {
            tool_calls: Some(tool_calls),
            ..
        } = &messages[index]
        else {
            index += 1;
            continue;
        };
        if tool_calls.is_empty() {
            index += 1;
            continue;
        }

        let expected_ids: HashSet<String> = tool_calls
            .iter()
            .enumerate()
            .map(|(idx, call)| normalized_tool_call_id(&call.id, idx))
            .collect();
        let mut completed_ids = HashSet::new();
        let mut next = index + 1;
        let mut tool_index = 0;
        while let Some(AgentMessage::Tool { tool_call_id, .. }) = messages.get(next) {
            let id = normalized_tool_call_id(tool_call_id, tool_index);
            tool_index += 1;
            completed_ids.insert(id);
            next += 1;
        }

        if expected_ids.is_subset(&completed_ids) {
            index = next;
            continue;
        }

        let truncate_at = index.checked_sub(1).filter(|previous| {
            matches!(
                &messages[*previous],
                AgentMessage::Custom { custom_type, .. } if custom_type == "thinking"
            )
        });
        messages.truncate(truncate_at.unwrap_or(index));
        return true;
    }
    false
}
