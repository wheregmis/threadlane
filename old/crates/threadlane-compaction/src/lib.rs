//! Token-budget compaction, checkpoint excerpts, and tool-output shaking.
//!
//! Pure message math over `threadlane-protocol` contracts plus
//! `threadlane-context` budgets — never engine or durability state.
//! `build_checkpoint_omitting_tool_outputs` takes pre-mapped
//! `(message, output_omitted)` pairs so durable `Entry` mapping stays with
//! the harness caller. Re-exported through `threadlane_runtime::compaction`
//! for compatibility; new code should import `threadlane_compaction`
//! directly.

use threadlane_context::ContextBudget;
use threadlane_protocol::AgentMessage;
use threadlane_protocol::ImageAttachment;

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashSet;

// Checkpoint text helpers live in `threadlane-provider` (payload translation
// needs them); re-exported here so existing paths keep working.
pub use threadlane_provider::convert::{
    compaction_checkpoint_text, compaction_summary_text, MAX_CONTEXT_SNAPSHOT_INDEX_CHARS,
    MAX_CONTEXT_SNAPSHOT_INDEX_ENTRIES,
};

/// Tuning knobs for compaction math. Mirrors the compaction `AgentConfig`
/// fields; the runtime adapter converts between them (pinned by a
/// default-drift test) so this crate stays engine-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionParams {
    pub auto_compaction_threshold_tokens: usize,
    pub auto_compaction_keep_recent_tokens: usize,
    pub max_checkpoint_chars: usize,
    pub estimated_image_tokens: usize,
}

impl Default for CompactionParams {
    fn default() -> Self {
        Self {
            auto_compaction_threshold_tokens: 96_000,
            auto_compaction_keep_recent_tokens: 20_000,
            max_checkpoint_chars: 12_000,
            estimated_image_tokens: 1_200,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompactionStrategy {
    #[default]
    TokenBudget,
    SemanticKeyframes,
}

#[derive(Debug, Clone)]
pub struct CompactionOptions {
    max_messages: usize,
    preserve_recent: usize,
}

#[derive(Debug, Clone)]
pub struct PreparedCompaction {
    pub messages: Vec<AgentMessage>,
    pub pre_tokens: usize,
    pub post_tokens: usize,
    pub compacted_messages: usize,
    /// Policy input used to choose the retained tail.
    pub retained_tail_target: usize,
    /// Measured serialized tokens actually retained after boundary adjustment.
    pub retained_tail_tokens: usize,
}

impl Default for CompactionOptions {
    fn default() -> Self {
        Self {
            max_messages: 50,
            preserve_recent: 10,
        }
    }
}

/// Engine-internal helpers, also used for request accounting. `pub` so the
/// runtime adapter can forward its historical `crate::compaction::…` paths;
/// treat them as engine-owned.
pub fn serialized_message(message: &AgentMessage) -> Vec<u8> {
    serde_json::to_vec(message).unwrap_or_default()
}

/// Returns the model-visible equivalent of a canonical message. Providers
/// intentionally omit arbitrary custom records, while durable compaction
/// summaries are converted to an ordinary user checkpoint message. The
/// borrowed variant covers the common identity case, so normalization does
/// not deep-clone every message on a hot accounting pass.
pub fn provider_normalized_message(message: &AgentMessage) -> Option<Cow<'_, AgentMessage>> {
    match message {
        AgentMessage::Custom { .. } => {
            compaction_checkpoint_text(message).map(|checkpoint| {
                Cow::Owned(AgentMessage::User {
                    content: format!("<context-checkpoint>\n{checkpoint}\n</context-checkpoint>"),
                })
            })
        }
        _ => Some(Cow::Borrowed(message)),
    }
}

pub fn estimate_message_tokens(message: &AgentMessage, config: &CompactionParams) -> usize {
    estimate_message_tokens_serialized(message, &serialized_message(message), config)
}

/// The same estimate as [`estimate_message_tokens`] for a caller that
/// already holds the serialized message bytes — the manifest and digest
/// paths serialize each message once and reuse the bytes here.
pub fn estimate_message_tokens_serialized(
    message: &AgentMessage,
    serialized: &[u8],
    config: &CompactionParams,
) -> usize {
    let (image_data_bytes, image_count) = image_payload_stats(message);
    // Inline image payloads (base64 data URLs persisted with the message so
    // reloads reproduce provider context) are not text tokens: providers bill
    // images separately. Charging the raw bytes here made one screenshot read
    // as ~230k tokens, tripping a compaction that could never retain the
    // recent image message under any tail target — so `compact_for_budget`
    // returned None and the turn failed with "could not drop historical
    // messages". Count the surrounding JSON only, plus the configured
    // per-image charge.
    let serialized_tokens = serialized
        .len()
        .saturating_sub(image_data_bytes)
        .div_ceil(4);
    serialized_tokens.saturating_add(image_count.saturating_mul(config.estimated_image_tokens))
}

fn image_payload_stats(message: &AgentMessage) -> (usize, usize) {
    match message {
        AgentMessage::UserWithImages { images, .. } => (
            images.iter().map(|image| image.data_url.len()).sum(),
            images.len(),
        ),
        AgentMessage::Tool { images, .. } => (
            images.iter().map(|image| image.data_url.len()).sum(),
            images.len(),
        ),
        _ => (0usize, 0usize),
    }
}

fn estimate_context_tokens(messages: &[AgentMessage], config: &CompactionParams) -> usize {
    messages
        .iter()
        .filter_map(provider_normalized_message)
        .map(|message| estimate_message_tokens(&message, config))
        .sum()
}

pub fn estimate_request_tokens(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    config: &CompactionParams,
) -> usize {
    estimate_context_tokens(messages, config)
        .saturating_add(tool_schema_json.map_or(0, |tools| tools.len().div_ceil(4)))
}

pub fn compact_for_budget(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    retained_tail_target: usize,
    config: &CompactionParams,
) -> Option<PreparedCompaction> {
    let pre_tokens = estimate_request_tokens(messages, tool_schema_json, config);
    let compacted =
        compact_messages_to_token_budget_with_config(messages, retained_tail_target, config);
    if compacted.len() == messages.len() {
        return None;
    }
    let post_tokens = estimate_request_tokens(&compacted, tool_schema_json, config);
    let compacted_messages = messages
        .len()
        .saturating_sub(compacted.len().saturating_sub(1));
    let retained_tail_tokens = compacted
        .iter()
        .skip_while(|message| compaction_summary_text(message).is_none())
        .skip(1)
        .map(|message| estimate_message_tokens(message, config))
        .sum();
    Some(PreparedCompaction {
        messages: compacted,
        pre_tokens,
        post_tokens,
        compacted_messages,
        retained_tail_target,
        retained_tail_tokens,
    })
}

#[cfg(test)]
fn compact_for_context_budget(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    budget: &ContextBudget,
    config: &CompactionParams,
) -> Option<PreparedCompaction> {
    if estimate_request_tokens(messages, tool_schema_json, config) <= budget.trigger_tokens {
        return None;
    }
    compact_for_budget(
        messages,
        tool_schema_json,
        budget.retained_tail_tokens,
        config,
    )
}

/// Returns true if message history exceeds the speculative compaction threshold (~85% of budget).
pub fn should_speculatively_compact(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    budget: &ContextBudget,
    config: &CompactionParams,
) -> bool {
    estimate_request_tokens(messages, tool_schema_json, config)
        >= budget.speculative_trigger_tokens()
}

/// Applies local deterministic tool output trimming ("tool shaking") to old turns,
/// preserving recent turns while reclaiming tokens without calling an LLM summarizer.
pub fn shake_historical_tool_outputs(
    messages: &[AgentMessage],
    preserve_recent_turns: usize,
) -> Vec<AgentMessage> {
    prune_historical_tool_outputs(messages, preserve_recent_turns)
}

pub fn should_auto_compact(messages: &[AgentMessage], config: &CompactionParams) -> bool {
    estimate_context_tokens(messages, config) > config.auto_compaction_threshold_tokens
}

pub fn is_context_overflow_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("context_length_exceeded")
        || error.contains("context length exceeded")
        || error.contains("maximum context length")
        || error.contains("input exceeds the context window")
        || error.contains("too many tokens")
}

pub fn compact_messages(
    messages: &[AgentMessage],
    options: &CompactionOptions,
) -> Vec<AgentMessage> {
    if messages.len() <= options.max_messages {
        return messages.to_vec();
    }

    let keep_count = options.preserve_recent.min(messages.len());
    compact_from_index(
        messages,
        messages.len().saturating_sub(keep_count),
        &CompactionParams::default(),
    )
}

pub fn compact_messages_to_token_budget(
    messages: &[AgentMessage],
    keep_recent_tokens: usize,
) -> Vec<AgentMessage> {
    compact_messages_to_token_budget_with_config(
        messages,
        keep_recent_tokens,
        &CompactionParams::default(),
    )
}

fn compact_messages_to_token_budget_with_config(
    messages: &[AgentMessage],
    keep_recent_tokens: usize,
    config: &CompactionParams,
) -> Vec<AgentMessage> {
    if messages.len() <= 2 {
        return messages.to_vec();
    }

    let mut tokens: usize = 0;
    let mut start = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        if matches!(message, AgentMessage::System { .. }) {
            continue;
        }
        tokens = tokens.saturating_add(estimate_message_tokens(message, config));
        start = index;
        if tokens >= keep_recent_tokens {
            break;
        }
    }

    start = tool_boundary_safe_start(messages, start);

    compact_from_index(messages, start, config)
}

fn tool_boundary_safe_start(messages: &[AgentMessage], start: usize) -> usize {
    if start >= messages.len() {
        return start;
    }

    if matches!(messages[start], AgentMessage::Tool { .. }) {
        let mut tool_block_start = start;
        while tool_block_start > 0
            && matches!(messages[tool_block_start - 1], AgentMessage::Tool { .. })
        {
            tool_block_start -= 1;
        }
        if tool_block_start > 0
            && is_complete_tool_exchange(messages, tool_block_start - 1, tool_block_start)
        {
            return tool_block_start - 1;
        }

        let mut after_tools = start;
        while after_tools < messages.len()
            && matches!(messages[after_tools], AgentMessage::Tool { .. })
        {
            after_tools += 1;
        }
        return after_tools;
    }

    if has_tool_calls(&messages[start]) && !is_complete_tool_exchange(messages, start, start + 1) {
        let mut after_exchange = start + 1;
        while after_exchange < messages.len()
            && matches!(messages[after_exchange], AgentMessage::Tool { .. })
        {
            after_exchange += 1;
        }
        return after_exchange;
    }

    start
}

fn has_tool_calls(message: &AgentMessage) -> bool {
    matches!(
        message,
        AgentMessage::Assistant {
            tool_calls: Some(calls),
            ..
        } if !calls.is_empty()
    )
}

fn is_complete_tool_exchange(
    messages: &[AgentMessage],
    assistant_index: usize,
    first_tool_index: usize,
) -> bool {
    let AgentMessage::Assistant {
        tool_calls: Some(calls),
        ..
    } = &messages[assistant_index]
    else {
        return false;
    };
    if calls.is_empty() || first_tool_index >= messages.len() {
        return false;
    }

    let tool_results = messages[first_tool_index..]
        .iter()
        .take_while(|message| matches!(message, AgentMessage::Tool { .. }));
    let result_ids: Vec<&str> = tool_results
        .filter_map(|message| match message {
            AgentMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect();

    result_ids.len() == calls.len()
        && calls
            .iter()
            .all(|call| result_ids.contains(&call.id.as_str()))
}
fn compact_from_index(
    messages: &[AgentMessage],
    mut start: usize,
    config: &CompactionParams,
) -> Vec<AgentMessage> {
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
            "schema_version": 1,
            "summary": build_checkpoint(&dropped, config),
            "compacted_messages": dropped.len(),
            "checkpoint_kind": "token_budget",
            "context_snapshot_index": [],
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
    config: &CompactionParams,
) -> Vec<AgentMessage> {
    match strategy {
        CompactionStrategy::TokenBudget => {
            compact_messages_to_token_budget_with_config(messages, target_tokens, config)
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
            let keyframe_tokens: usize = keyframes
                .iter()
                .map(|m| estimate_message_tokens(m, config))
                .sum();
            let remaining_budget = target_tokens.saturating_sub(keyframe_tokens);

            let recent =
                compact_messages_to_token_budget_with_config(messages, remaining_budget, config);
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

const AGED_TOOL_INLINE_LIMIT: usize = 2_000;
const AGED_TOOL_PRUNE_HEAD_CHARS: usize = 1_000;
const AGED_TOOL_PRUNE_TAIL_CHARS: usize = 400;

fn aged_tool_output_preview(content: &str, total_chars: usize) -> String {
    let head_end = content
        .char_indices()
        .nth(AGED_TOOL_PRUNE_HEAD_CHARS)
        .map_or(content.len(), |(index, _)| index);
    let tail_start = content
        .char_indices()
        .rev()
        .nth(AGED_TOOL_PRUNE_TAIL_CHARS - 1)
        .map_or(0, |(index, _)| index);
    let head = &content[..head_end];
    let tail = &content[tail_start..];
    let hidden = total_chars.saturating_sub(AGED_TOOL_PRUNE_HEAD_CHARS + AGED_TOOL_PRUNE_TAIL_CHARS);
    format!(
        "{head}\n\n[... {hidden} characters pruned from the middle of this tool output ...]\n\n{tail}"
    )
}

/// Squeezes historical tool outputs older than `keep_recent_tool_turns` to save input tokens.
///
/// Oversized aged outputs keep a bounded head/tail preview of their content
/// (the same shape dispatch-time `truncate_tool_output` produces) instead of
/// being replaced by a bare stub — the model keeps the beginning and the end
/// of what it saw while the middle ages out. Bodies below
/// `AGED_TOOL_INLINE_LIMIT` stay verbatim: a marker costs more than they do.
///
/// User-attached images age out the same way: only the most recent
/// `USER_IMAGE_KEEP_RECENT` user turn keeps its attachments (the current
/// request still needs them); older ones keep their text with images
/// cleared, exactly like pruned tool outputs.
pub fn prune_historical_tool_outputs(
    messages: &[AgentMessage],
    keep_recent_tool_turns: usize,
) -> Vec<AgentMessage> {
    prune_historical_tool_outputs_preserving(messages, keep_recent_tool_turns, &HashSet::new())
}

/// Same squeeze as [`prune_historical_tool_outputs`], except tool results whose
/// `tool_call_id` appears in `preserve_call_ids` always stay verbatim. Requests
/// may carry earlier messages that reference those results by call id (e.g.
/// "full content remains in earlier tool result X"), and previewing the anchor
/// would leave the pointer pointing at an excerpt.
pub fn prune_historical_tool_outputs_preserving(
    messages: &[AgentMessage],
    keep_recent_tool_turns: usize,
    preserve_call_ids: &HashSet<&str>,
) -> Vec<AgentMessage> {
    const USER_IMAGE_KEEP_RECENT: usize = 1;
    let mut tool_seen_count = 0;
    let mut user_image_seen_count = 0;
    let mut result = Vec::with_capacity(messages.len());

    let mut keep_full = vec![false; messages.len()];
    for (i, msg) in messages.iter().enumerate().rev() {
        if let AgentMessage::Tool { tool_call_id, .. } = msg {
            tool_seen_count += 1;
            if tool_seen_count <= keep_recent_tool_turns
                || preserve_call_ids.contains(tool_call_id.as_str())
            {
                keep_full[i] = true;
            }
        } else if matches!(
            msg,
            AgentMessage::UserWithImages { images, .. } if !images.is_empty()
        ) {
            user_image_seen_count += 1;
            if user_image_seen_count <= USER_IMAGE_KEEP_RECENT {
                keep_full[i] = true;
            }
        }
    }

    for (i, msg) in messages.iter().enumerate() {
        match msg {
            AgentMessage::Tool { .. } if keep_full[i] => result.push(msg.clone()),
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                terminate,
                images,
            } => {
                let image_bytes: usize = images.iter().map(|image| image.data_url.len()).sum();
                let content_chars = content.chars().count();
                if content_chars <= AGED_TOOL_INLINE_LIMIT
                    && image_bytes <= AGED_TOOL_INLINE_LIMIT
                {
                    result.push(msg.clone());
                } else {
                    let pruned_content = if content_chars > AGED_TOOL_INLINE_LIMIT {
                        aged_tool_output_preview(content, content_chars)
                    } else {
                        content.clone()
                    };
                    result.push(AgentMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        name: name.clone(),
                        content: pruned_content,
                        is_error: *is_error,
                        terminate: *terminate,
                        // Pruning bounds context: attached images age out
                        // with the turn they illustrated.
                        images: Vec::new(),
                    });
                }
            }
            AgentMessage::UserWithImages { content, images } => {
                if keep_full[i] || images.is_empty() {
                    result.push(msg.clone());
                } else {
                    // Historical user attachments: text stays, images age
                    // out with the turn they illustrated.
                    result.push(AgentMessage::UserWithImages {
                        content: content.clone(),
                        images: Vec::new(),
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
    config: &CompactionParams,
) -> Vec<AgentMessage> {
    let pruned = prune_historical_tool_outputs(messages, 3);
    compact_messages_with_strategy(
        &pruned,
        target_tokens,
        CompactionStrategy::SemanticKeyframes,
        config,
    )
}

fn build_checkpoint(messages: &[AgentMessage], config: &CompactionParams) -> String {
    build_checkpoint_from_entries(
        messages.len(),
        messages.iter().map(|message| (message, false)),
        config,
    )
}

pub fn build_checkpoint_omitting_tool_outputs(
    messages: &[(&AgentMessage, bool)],
    config: &CompactionParams,
) -> String {
    build_checkpoint_from_entries(
        messages.len(),
        messages
            .iter()
            .map(|(message, omitted)| (*message, *omitted)),
        config,
    )
}

fn build_checkpoint_from_entries<'a>(
    message_count: usize,
    entries: impl DoubleEndedIterator<Item = (&'a AgentMessage, bool)>,
    config: &CompactionParams,
) -> String {
    let mut excerpts = Vec::new();

    // Findings first: condensed dead-ends survive rotation while raw
    // transcripts do not. A bounded slice of the same budget.
    let dropped: Vec<(&AgentMessage, bool)> = entries.collect();
    let task_state = checkpoint_task_state(&dropped, config.max_checkpoint_chars / 2);
    let mut used_chars = task_state.chars().count();
    let findings = build_findings(
        &dropped,
        config.max_checkpoint_chars.saturating_sub(used_chars),
    );
    used_chars += findings.chars().count();

    for (message, output_omitted) in dropped.iter().rev() {
        let Some(excerpt) = message_excerpt(message, *output_omitted) else {
            continue;
        };
        let separator_chars = if excerpts.is_empty() { 0 } else { 2 };
        let excerpt_chars = excerpt.chars().count() + separator_chars;
        if used_chars + excerpt_chars > config.max_checkpoint_chars {
            // An oversized recent message must not hide all older evidence.
            continue;
        }
        used_chars += excerpt_chars;
        excerpts.push(excerpt);
    }
    excerpts.reverse();

    format!(
        "Context checkpoint from {} earlier messages. Continue the same task using the retained recent messages and these earlier excerpts:\n\n{}{}{}",
        message_count,
        task_state,
        findings,
        excerpts.join("\n\n")
    )
}

#[derive(Default, Serialize, Deserialize)]
struct CheckpointTaskState {
    objective: String,
    instructions: Vec<String>,
}

/// Preserve user-authored intent explicitly; never infer decisions or success
/// from prose. The durable adapter adds the current plan separately.
fn checkpoint_task_state(dropped: &[(&AgentMessage, bool)], budget: usize) -> String {
    let mut state = dropped
        .iter()
        .find_map(|(message, _)| {
            let summary = compaction_summary_text(message)?;
            let text = summary
                .split_once("<task-state>\n")?
                .1
                .split_once("\n</task-state>")?
                .0;
            serde_json::from_str::<CheckpointTaskState>(text).ok()
        })
        .unwrap_or_default();
    let users: Vec<_> = dropped
        .iter()
        .filter_map(|(message, _)| match message {
            AgentMessage::User { content } | AgentMessage::UserWithImages { content, .. } => {
                Some(content)
            }
            _ => None,
        })
        .collect();
    if state.objective.is_empty() {
        state.objective = users
            .first()
            .map(|text| (*text).clone())
            .unwrap_or_default();
    }
    for text in users {
        if text != &state.objective && !state.instructions.contains(text) {
            state.instructions.push(text.clone());
        }
    }
    if state.objective.is_empty() && state.instructions.is_empty() {
        return String::new();
    }
    if state.instructions.len() > 2 {
        state.instructions.drain(..state.instructions.len() - 2);
    }
    // ponytail: bounded verbatim intent, not semantic summarization. Retain
    // the original journal for full instructions beyond the checkpoint budget.
    let mut cap = budget;
    loop {
        let excerpt = |text: &str| {
            if text.chars().count() <= cap {
                text.to_owned()
            } else {
                format!(
                    "{} [excerpt; full text remains in transcript]",
                    text.chars().take(cap).collect::<String>()
                )
            }
        };
        let bounded = CheckpointTaskState {
            objective: excerpt(&state.objective),
            instructions: state
                .instructions
                .iter()
                .map(|text| excerpt(text))
                .collect(),
        };
        let json = serde_json::to_string(&bounded).unwrap_or_default();
        let text = format!("<task-state>\n{json}\n</task-state>\n\n");
        if text.chars().count() <= budget {
            return text;
        }
        if cap == 0 {
            return String::new();
        }
        cap /= 2;
    }
}

/// Condensed episodic memory for the dropped range: failed tool calls
/// (deduplicated, with counts) and loop-guard trip notices. Raw outputs age
/// out; what was tried and what failed must not.
fn build_findings(dropped: &[(&AgentMessage, bool)], budget: usize) -> String {
    const MAX_FINDINGS: usize = 6;
    const MAX_FINDING_CHARS: usize = 200;
    const MAX_FINDINGS_CHARS: usize = 1_200;

    let mut failures: Vec<(String, usize, String)> = Vec::new();
    let mut trips: Vec<String> = Vec::new();
    for (message, _) in dropped {
        match message {
            AgentMessage::Tool {
                name,
                content,
                is_error,
                ..
            } if *is_error => {
                let first_line: String = content
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .chars()
                    .take(MAX_FINDING_CHARS)
                    .collect();
                match failures
                    .iter_mut()
                    .find(|(existing_name, _, existing_first)| {
                        existing_name == name && existing_first == &first_line
                    }) {
                    Some((_, count, _)) => *count += 1,
                    None => failures.push((name.clone(), 1, first_line)),
                }
            }
            AgentMessage::Assistant {
                content: Some(content),
                ..
            } if content.starts_with("Stopped:") => {
                let line: String = content.chars().take(MAX_FINDING_CHARS).collect();
                if !trips.contains(&line) {
                    trips.push(line);
                }
            }
            _ => {}
        }
    }
    if failures.is_empty() && trips.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    for (name, count, first_line) in failures.iter().take(MAX_FINDINGS) {
        let times = if *count > 1 {
            format!(" {count}x")
        } else {
            String::new()
        };
        lines.push(format!("- `{name}` failed{times}: {first_line}"));
    }
    for trip in trips.iter().take(MAX_FINDINGS) {
        lines.push(format!("- {trip}"));
    }
    let mut findings = format!(
        "Tried and failed (do not retry verbatim):\n{}\n\n",
        lines.join("\n")
    );
    if let Some((end, _)) = findings.char_indices().nth(budget.min(MAX_FINDINGS_CHARS)) {
        findings.truncate(end);
    }
    findings
}

fn message_excerpt(message: &AgentMessage, output_omitted: bool) -> Option<String> {
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
            if output_omitted {
                return Some(format!(
                    "Tool {name}: [output stored as an available context snapshot]"
                ));
            }
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

#[cfg(test)]
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
                        let line = "cargo check -p threadlane-gpui";
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
            } if text.contains("GPUI") && text.contains("component") => {
                let line = "UI components in crates/threadlane-gpui/src must use GPUI components.";
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
    #[test]
    fn task_intent_survives_repeated_checkpoints_and_oversized_recent_excerpt() {
        let config = super::CompactionParams::default();
        let initial = vec![
            threadlane_protocol::AgentMessage::user(
                "Fix pricing; preserve immutable history",
                vec![],
            ),
            threadlane_protocol::AgentMessage::Assistant {
                content: Some("successful verification evidence".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            threadlane_protocol::AgentMessage::user("Do not change public APIs", vec![]),
            threadlane_protocol::AgentMessage::Assistant {
                content: Some("large output ".repeat(2000)),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
        ];
        let first = super::build_checkpoint(&initial, &config);
        assert!(first.contains("successful verification evidence"));
        assert!(first.contains("Do not change public APIs"));
        let checkpoint = threadlane_protocol::AgentMessage::Custom {
            custom_type: "compaction_summary".into(),
            payload: serde_json::json!({"summary": first}),
        };
        let second = super::build_checkpoint(
            &[
                checkpoint,
                threadlane_protocol::AgentMessage::user("Verify reload", vec![]),
            ],
            &config,
        );
        let state = second
            .split_once("<task-state>\n")
            .unwrap()
            .1
            .split_once("\n</task-state>")
            .unwrap()
            .0;
        let state: serde_json::Value = serde_json::from_str(state).unwrap();
        assert_eq!(
            state["objective"],
            "Fix pricing; preserve immutable history"
        );
        assert_eq!(
            state["instructions"],
            serde_json::json!(["Do not change public APIs", "Verify reload"])
        );
        assert!(second.chars().count() < config.max_checkpoint_chars + 200);
    }

    use super::*;
    use std::collections::HashSet;
    use threadlane_protocol::ImageAttachment;
    use threadlane_protocol::{
        RuntimeToolCall as ToolCall, RuntimeToolCallFunction as ToolCallFunction,
    };

    fn tool_exchange_fixture(historical_chars: usize) -> Vec<AgentMessage> {
        vec![
            AgentMessage::System {
                content: "system".into(),
            },
            AgentMessage::User {
                content: "older request".into(),
            },
            AgentMessage::User {
                content: "x".repeat(historical_chars),
            },
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::Tool {
                tool_call_id: "call_1".into(),
                name: "read_file".into(),
                content: "result".repeat(1_000),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            },
            AgentMessage::User {
                content: "continue".into(),
            },
        ]
    }

    fn assert_valid_tool_pairs(messages: &[AgentMessage]) {
        let mut pending = HashSet::new();
        for message in messages {
            match message {
                AgentMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } if !calls.is_empty() => {
                    assert!(pending.is_empty(), "tool call missing its result");
                    pending.extend(calls.iter().map(|call| call.id.as_str()));
                }
                AgentMessage::Tool { tool_call_id, .. } => {
                    assert!(
                        pending.remove(tool_call_id.as_str()),
                        "tool result missing its assistant tool call"
                    );
                }
                _ => assert!(pending.is_empty(), "tool call missing its result"),
            }
        }
        assert!(pending.is_empty(), "tool call missing its result");
    }

    #[test]
    fn request_estimator_charges_text_and_configured_images_not_image_bytes() {
        let config = CompactionParams {
            estimated_image_tokens: 77,
            ..CompactionParams::default()
        };
        let data_url = "data:image/png;base64,AA==";
        let messages = vec![AgentMessage::UserWithImages {
            content: "x".repeat(400),
            images: vec![ImageAttachment {
                display_name: "image.png".into(),
                data_url: data_url.into(),
            }],
        }];
        // The inline payload is not text: only the surrounding JSON counts,
        // plus the configured per-image charge.
        let serialized_message_tokens = (serde_json::to_vec(&messages[0]).unwrap().len()
            - data_url.len())
        .div_ceil(4);
        assert_eq!(
            estimate_request_tokens(&messages, Some(&"t".repeat(400)), &config),
            serialized_message_tokens + 100 + 77
        );
    }

    #[test]
    fn screenshot_sized_images_do_not_trip_compaction() {
        // Regression for a failed computer-use turn: a `computer_ax` result
        // carrying a ~900KB inline PNG screenshot estimated ~230k tokens
        // (full base64 / 4), exceeding every budget trigger while the recent
        // image message alone exceeded every retained tail — so
        // `compact_for_budget` returned None ("could not drop historical
        // messages") and the turn died before acting.
        let config = CompactionParams::default();
        let big_png = format!("data:image/png;base64,{}", "QUJD".repeat(225_000));
        assert!(big_png.len() > 900_000);
        let messages = vec![
            AgentMessage::User {
                content: "older request".into(),
            },
            AgentMessage::Assistant {
                content: Some("older response".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::Tool {
                tool_call_id: "call_0".into(),
                name: "computer_windows".into(),
                content: "result".repeat(1_000),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            },
            AgentMessage::User {
                content: "Can you open youtube in my safari?".into(),
            },
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: "computer_ax".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::Tool {
                tool_call_id: "call_1".into(),
                name: "computer_ax".into(),
                content: "tree".repeat(2_500),
                is_error: false,
                terminate: false,
                images: vec![ImageAttachment {
                    display_name: "computer-1.png".into(),
                    data_url: big_png,
                }],
            },
        ];
        let estimate = estimate_request_tokens(&messages, None, &config);
        assert!(
            estimate < config.auto_compaction_threshold_tokens,
            "screenshot history must stay under the compaction trigger, got {estimate}"
        );
        // And a tight tail budget still compacts text history around the
        // image instead of failing closed on it.
        let prepared = compact_for_budget(&messages, None, 1_000, &config)
            .expect("image-heavy history must still compact");
        assert!(
            prepared.compacted_messages > 0,
            "compaction should drop messages, not no-op"
        );
    }

    #[test]
    fn provider_normalization_preserves_summary_and_renders_structured_snapshot_index() {
        let summary = concat!(
            "Keep this model-authored text exactly.\n",
            "<!-- threadlane:context-snapshots:index:begin -->\n",
            "## Available context snapshots\n",
            "- user-authored marker-looking content\n",
            "<!-- threadlane:context-snapshots:index:end -->",
        );
        let message = AgentMessage::Custom {
            custom_type: "compaction_summary".into(),
            payload: serde_json::json!({
                "summary": summary,
                "context_snapshot_index": [{
                    "context_id": "ctx-1",
                    "path": "src/lib.rs",
                    "start_line": 2,
                    "end_line": 4,
                    "file_sha256": "abc123",
                }],
            }),
        };

        assert_eq!(compaction_summary_text(&message), Some(summary));
        assert_eq!(
            provider_normalized_message(&message).map(Cow::into_owned),
            Some(AgentMessage::User {
                content: format!(
                    "<context-checkpoint>\n{summary}\n\n## Available context snapshots\n- ctx-1 src/lib.rs:2-4 sha256=abc123\n</context-checkpoint>"
                ),
            })
        );

        let legacy = AgentMessage::Custom {
            custom_type: "compaction_summary".into(),
            payload: serde_json::json!({"summary": "legacy summary"}),
        };
        assert_eq!(
            provider_normalized_message(&legacy).map(Cow::into_owned),
            Some(AgentMessage::User {
                content: "<context-checkpoint>\nlegacy summary\n</context-checkpoint>".into(),
            })
        );
    }

    #[test]
    fn context_budget_compacts_strictly_above_trigger() {
        let config = CompactionParams::default();
        let messages = tool_exchange_fixture(12_000);
        let request_tokens = estimate_request_tokens(&messages, None, &config);
        let budget = threadlane_context::ContextBudget {
            limit: request_tokens + 1,
            limit_is_estimate: false,
            trigger_tokens: request_tokens,
            retained_tail_tokens: 1_000,
            strict_retained_tail_tokens: 500,
        };

        assert!(compact_for_context_budget(&messages, None, &budget, &config).is_none());

        let above_trigger = threadlane_context::ContextBudget {
            trigger_tokens: request_tokens - 1,
            ..budget
        };
        assert!(compact_for_context_budget(&messages, None, &above_trigger, &config).is_some());
    }

    #[test]
    fn budget_compaction_uses_caller_config_for_tail_and_checkpoint() {
        let config = CompactionParams {
            estimated_image_tokens: 1,
            max_checkpoint_chars: 0,
            ..CompactionParams::default()
        };
        let messages = vec![
            AgentMessage::System {
                content: "system".into(),
            },
            AgentMessage::User {
                content: "old request that must not appear in the checkpoint".into(),
            },
            AgentMessage::Assistant {
                content: Some("older response".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::Assistant {
                content: Some("a".repeat(2_000)),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::UserWithImages {
                content: "latest".into(),
                images: vec![ImageAttachment {
                    display_name: "small.png".into(),
                    data_url: "data:image/png;base64,AA==".into(),
                }],
            },
        ];

        let result = compact_for_budget(&messages, None, 500, &config).unwrap();
        assert!(matches!(
            result.messages.last(),
            Some(AgentMessage::UserWithImages { .. })
        ));
        assert!(matches!(
            result.messages.get(result.messages.len() - 2),
            Some(AgentMessage::Assistant { .. })
        ));
        let checkpoint = compaction_summary_text(&result.messages[1]).unwrap();
        assert!(!checkpoint.contains("old request"));
    }

    #[test]
    fn budget_compaction_retains_complete_tool_exchange() {
        let messages = tool_exchange_fixture(12_000);
        let result =
            compact_for_budget(&messages, None, 1_000, &CompactionParams::default()).unwrap();
        assert!(compaction_summary_text(&result.messages[1]).is_some());
        assert_valid_tool_pairs(&result.messages);
        assert!(result.post_tokens < result.pre_tokens);
        assert!(result.compacted_messages > 0);
        assert_eq!(result.retained_tail_target, 1_000);
        assert_ne!(result.retained_tail_tokens, result.retained_tail_target);
    }
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
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }]),
            stop_reason: None,
            deferred_handle: None,
        });
        msgs.push(AgentMessage::Tool {
            tool_call_id: "call_1".into(),
            name: "read_file".into(),
            content: "x".repeat(1_000),
            is_error: false,
            terminate: false,
            images: Vec::new(),
        });

        let compacted = compact_messages_to_token_budget(&msgs, 1);
        assert!(matches!(compacted[2], AgentMessage::Assistant { .. }));
        assert!(matches!(compacted[3], AgentMessage::Tool { .. }));
        assert_valid_tool_pairs(&compacted);
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
                images: Vec::new(),
            },
            AgentMessage::Assistant {
                content: Some("GPUI component guidelines must be followed.".into()),
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
            compact_messages_with_strategy(&msgs, 200, CompactionStrategy::SemanticKeyframes, &CompactionParams::default());
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
                images: Vec::new(),
            });
        }

        let pruned = prune_historical_tool_outputs(&msgs, 3);
        assert_eq!(pruned.len(), msgs.len());

        let full_count = pruned
            .iter()
            .filter(
                |m| matches!(m, AgentMessage::Tool { content, .. } if content.len() == 5_000),
            )
            .count();
        assert_eq!(full_count, 3);

        let pruned_count = pruned
            .iter()
            .filter(|m| matches!(m, AgentMessage::Tool { content, .. } if content.contains("characters pruned from the middle")))
            .count();
        assert_eq!(pruned_count, 7);
        // Aged outputs keep a bounded head/tail preview instead of a bare stub.
        for message in &pruned {
            if let AgentMessage::Tool { content, .. } = message {
                assert!(content.len() <= 5_000);
                if content.contains("characters pruned") {
                    assert!(content.starts_with("aaaaa"));
                    assert!(content.ends_with("aaaaa"));
                    assert!(content.len() < 1_600);
                }
            }
        }

        let optimal = prepare_token_optimal_context(&msgs, 10_000, &CompactionParams::default());
        assert!(!optimal.is_empty());
        assert_eq!(optimal[0].role_str(), "system");
    }

    #[test]
    fn prune_historical_tool_outputs_measures_limits_in_characters() {
        let mut msgs = vec![AgentMessage::User {
            content: "prompt".into(),
        }];
        for i in 0..6 {
            msgs.push(AgentMessage::Assistant {
                content: None,
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            });
            msgs.push(AgentMessage::Tool {
                tool_call_id: format!("call_{i}"),
                name: "run_command".into(),
                content: if i == 0 {
                    // 1_500 chars but >2_000 bytes: under the limit and
                    // must stay verbatim, never enter the preview path.
                    "🔥".repeat(1_500)
                } else {
                    "🔥".repeat(3_000)
                },
                is_error: false,
                terminate: false,
                images: Vec::new(),
            });
        }

        let pruned = prune_historical_tool_outputs(&msgs, 1);
        let contents: Vec<&str> = pruned
            .iter()
            .filter_map(|message| match message {
                AgentMessage::Tool { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(contents.len(), 6);
        assert_eq!(contents[0], "🔥".repeat(1_500));
        for content in &contents[1..4] {
            assert!(content.contains("characters pruned from the middle"));
            // Head and tail must not overlap or duplicate content for
            // multibyte bodies: the preview is strictly smaller than the
            // input and ends on the body's real tail.
            assert!(content.chars().count() < 3_000);
            assert!(content.starts_with('🔥'));
            assert!(content.ends_with('🔥'));
        }
        // The one kept-recent tool turn stays verbatim regardless of size.
        assert_eq!(contents[5], "🔥".repeat(3_000));
    }

    #[test]
    fn prune_historical_tool_outputs_preserving_keeps_referenced_anchors() {
        let mut msgs = vec![AgentMessage::User {
            content: "prompt".into(),
        }];
        for i in 0..6 {
            msgs.push(AgentMessage::Assistant {
                content: None,
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            });
            msgs.push(AgentMessage::Tool {
                tool_call_id: format!("call_{i}"),
                name: "read_file".into(),
                content: "a".repeat(5_000),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            });
        }

        let mut preserve = HashSet::new();
        preserve.insert("call_1");
        let pruned = prune_historical_tool_outputs_preserving(&msgs, 1, &preserve);
        let contents: Vec<&str> = pruned
            .iter()
            .filter_map(|message| match message {
                AgentMessage::Tool { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(contents.len(), 6);
        assert!(contents[0].contains("characters pruned from the middle"));
        // The referenced anchor stays full even though it is aged.
        assert_eq!(contents[1], "a".repeat(5_000));
        for content in &contents[2..5] {
            assert!(content.contains("characters pruned from the middle"));
        }
        assert_eq!(contents[5], "a".repeat(5_000));
    }

    #[test]
    fn checkpoint_condenses_failures_into_findings() {
        let config = CompactionParams::default();
        let messages = vec![
            AgentMessage::User {
                content: "fix it".into(),
            },
            AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "read_file".into(),
                content: "file not found: src/a.rs".into(),
                is_error: true,
                terminate: false,
                images: Vec::new(),
            },
            AgentMessage::Tool {
                tool_call_id: "call-2".into(),
                name: "read_file".into(),
                content: "file not found: src/a.rs".into(),
                is_error: true,
                terminate: false,
                images: Vec::new(),
            },
            AgentMessage::Assistant {
                content: Some("Stopped: `read_file` ran 5 times in a row.".into()),
                tool_calls: None,
                stop_reason: Some("loop_guard".into()),
                deferred_handle: None,
            },
        ];
        let checkpoint = build_checkpoint(&messages, &config);
        assert!(
            checkpoint.contains("Tried and failed"),
            "failures must survive as findings: {checkpoint}"
        );
        assert!(checkpoint.contains("`read_file` failed 2x"));
        assert!(checkpoint.contains("Stopped:"));
    }

    #[test]
    fn checkpoint_body_respects_small_budgets_with_unicode_failures() {
        let messages: Vec<_> = (0..8)
            .map(|index| AgentMessage::Tool {
                tool_call_id: format!("call-{index}"),
                name: "read_file".into(),
                content: format!("{index}: {}", "🦀".repeat(200)),
                is_error: true,
                terminate: false,
                images: vec![],
            })
            .collect();
        for budget in [0, 64, 512, 1_200, 2_000] {
            let config = CompactionParams {
                max_checkpoint_chars: budget,
                ..CompactionParams::default()
            };
            let checkpoint = build_checkpoint(&messages, &config);
            let body = checkpoint.split_once("\n\n").unwrap().1;
            assert!(body.chars().count() <= budget, "budget {budget}");
        }
        // Tiny excerpts also pay for their separators.
        let messages: Vec<_> = (0..100)
            .map(|_| AgentMessage::user("x", vec![]))
            .collect();
        let config = CompactionParams {
            max_checkpoint_chars: 512,
            ..CompactionParams::default()
        };
        let checkpoint = build_checkpoint(&messages, &config);
        assert!(checkpoint.split_once("\n\n").unwrap().1.chars().count() <= 512);
    }

    #[test]
    fn checkpoint_without_failures_keeps_legacy_shape() {
        let config = CompactionParams::default();
        let messages = vec![
            AgentMessage::User {
                content: "hi".into(),
            },
            AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "read_file".into(),
                content: "ok".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            },
        ];
        let checkpoint = build_checkpoint(&messages, &config);
        assert!(!checkpoint.contains("Tried and failed"));
        assert!(checkpoint.starts_with("Context checkpoint from"));
    }
}

#[cfg(test)]
mod image_accounting_tests {
    use super::*;

    fn imaged_user(content: &str) -> AgentMessage {
        AgentMessage::UserWithImages {
            content: content.into(),
            images: vec![ImageAttachment {
                display_name: "shot.png".into(),
                data_url: format!("data:image/png;base64,{}", "QUJD".repeat(10_000)),
            }],
        }
    }

    #[test]
    fn image_bytes_are_not_double_counted() {
        let config = CompactionParams {
            estimated_image_tokens: 1_200,
            ..CompactionParams::default()
        };
        let message = imaged_user("look");
        let tokens = estimate_message_tokens(&message, &config);
        // 40k of base64 must not inflate the estimate: text + one image rate.
        assert!(tokens < 10_000, "double-counted image bytes: {tokens}");
        assert!(tokens >= 1_200);
    }

    #[test]
    fn only_the_latest_user_images_survive_pruning() {
        let messages = vec![
            imaged_user("first"),
            AgentMessage::User {
                content: "middle".into(),
            },
            imaged_user("latest"),
        ];
        let pruned = prune_historical_tool_outputs(&messages, 3);
        let images: Vec<usize> = pruned
            .iter()
            .filter_map(|message| match message {
                AgentMessage::UserWithImages { images, .. } => Some(images.len()),
                _ => None,
            })
            .collect();
        assert_eq!(images, vec![0, 1], "older user images age out, latest stays");
    }
}
