//! Token-budget compaction, canonical in `threadlane-compaction`.
//!
//! Pure message math lives in the leaf crate over `threadlane-protocol`
//! contracts and `threadlane-context` budgets. This module re-exports the
//! leaf items and adapts the runtime `AgentConfig` into `CompactionParams`
//! (pinned by a default-drift test); callers map durable `Entry` records to
//! message pairs at the boundary. New code should import
//! `threadlane_compaction` directly.

pub use threadlane_compaction::{
    compact_messages, compact_messages_with_strategy, compaction_checkpoint_text,
    compaction_summary_text, prepare_token_optimal_context, prune_historical_tool_outputs,
    shake_historical_tool_outputs, CompactionOptions, CompactionParams, CompactionStrategy,
    PreparedCompaction, MAX_CONTEXT_SNAPSHOT_INDEX_CHARS, MAX_CONTEXT_SNAPSHOT_INDEX_ENTRIES,
};

use crate::config::AgentConfig;

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
