//! Token-budget compaction, canonical in `threadlane-compaction`.
//!
//! Pure message math lives in the leaf crate over `threadlane-protocol`
//! contracts and `threadlane-context` budgets. This module keeps the
//! historical `threadlane_runtime::compaction::…` paths working: items that
//! never touched engine state are re-exported, while config-taking functions
//! adapt the runtime `AgentConfig` into `CompactionParams` (pinned by a
//! default-drift test) and the entry-based checkpoint maps durable `Entry`
//! records to message pairs at the boundary. New code should import
//! `threadlane_compaction` directly.

pub use threadlane_compaction::{
    compact_messages, compact_messages_with_strategy, compaction_checkpoint_text,
    compaction_summary_text, prepare_token_optimal_context, prune_historical_tool_outputs,
    shake_historical_tool_outputs, CompactionOptions, CompactionParams, CompactionStrategy,
    PreparedCompaction, MAX_CONTEXT_SNAPSHOT_INDEX_CHARS, MAX_CONTEXT_SNAPSHOT_INDEX_ENTRIES,
};

use crate::config::AgentConfig;
use crate::harness::Entry;
use threadlane_compaction::compact_messages_to_token_budget as leaf_compact_to_budget;
use threadlane_context::ContextBudget;
use threadlane_protocol::AgentMessage;

impl From<&AgentConfig> for CompactionParams {
    fn from(config: &AgentConfig) -> Self {
        Self {
            auto_compaction_threshold_tokens: config.auto_compaction_threshold_tokens,
            auto_compaction_keep_recent_tokens: config.auto_compaction_keep_recent_tokens,
            max_checkpoint_chars: config.max_checkpoint_chars,
            estimated_image_tokens: config.estimated_image_tokens,
        }
    }
}

pub(crate) fn estimate_message_tokens(message: &AgentMessage, config: &AgentConfig) -> usize {
    threadlane_compaction::estimate_message_tokens(message, &CompactionParams::from(config))
}

pub fn estimate_request_tokens(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    config: &AgentConfig,
) -> usize {
    threadlane_compaction::estimate_request_tokens(
        messages,
        tool_schema_json,
        &CompactionParams::from(config),
    )
}

pub fn compact_for_budget(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    retained_tail_target: usize,
    config: &AgentConfig,
) -> Option<PreparedCompaction> {
    threadlane_compaction::compact_for_budget(
        messages,
        tool_schema_json,
        retained_tail_target,
        &CompactionParams::from(config),
    )
}

pub fn should_speculatively_compact(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    budget: &ContextBudget,
    config: &AgentConfig,
) -> bool {
    threadlane_compaction::should_speculatively_compact(
        messages,
        tool_schema_json,
        budget,
        &CompactionParams::from(config),
    )
}

pub(crate) fn should_auto_compact(messages: &[AgentMessage], config: &AgentConfig) -> bool {
    threadlane_compaction::should_auto_compact(messages, &CompactionParams::from(config))
}

pub(crate) fn compact_messages_to_token_budget(
    messages: &[AgentMessage],
    keep_recent_tokens: usize,
) -> Vec<AgentMessage> {
    leaf_compact_to_budget(messages, keep_recent_tokens)
}

pub(crate) fn serialized_message(message: &AgentMessage) -> Vec<u8> {
    threadlane_compaction::serialized_message(message)
}

pub(crate) fn provider_normalized_message(message: &AgentMessage) -> Option<AgentMessage> {
    threadlane_compaction::provider_normalized_message(message)
}

pub(crate) fn is_context_overflow_error(error: &str) -> bool {
    threadlane_compaction::is_context_overflow_error(error)
}

pub fn build_checkpoint_omitting_tool_outputs(
    entries: &[Entry],
    omitted_source_entry_ids: &[String],
    config: &AgentConfig,
) -> String {
    let params = CompactionParams::from(config);
    let pairs: Vec<(&AgentMessage, bool)> = entries
        .iter()
        .map(|entry| (&entry.message, omitted_source_entry_ids.contains(&entry.id)))
        .collect();
    threadlane_compaction::build_checkpoint_omitting_tool_outputs(&pairs, &params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_params_default_tracks_agent_config_default() {
        assert_eq!(
            CompactionParams::from(&AgentConfig::default()),
            CompactionParams::default(),
            "CompactionParams::default must mirror AgentConfig::default or the leaf drifted",
        );
    }
}
