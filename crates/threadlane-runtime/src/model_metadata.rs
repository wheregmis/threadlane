//! Context-window budgeting, canonical in `threadlane-context`.
//!
//! The limit table, budget math, and `BudgetConfig` live in the leaf crate so
//! the engine, session, and UI share one definition. This module keeps the
//! historical `threadlane_runtime::model_metadata::…` paths working: types
//! and `model_context_limit` are re-exported, and `context_budget` adapts
//! the runtime `AgentConfig` into `BudgetConfig`. New code should import
//! `threadlane_context` directly.

pub use threadlane_context::{
    model_context_limit, BudgetConfig, ContextBudget, UNKNOWN_MODEL_CONTEXT_LIMIT,
};

use crate::config::AgentConfig;

impl From<&AgentConfig> for BudgetConfig {
    fn from(config: &AgentConfig) -> Self {
        Self {
            unknown_model_context_limit: config.unknown_model_context_limit,
            context_minimum_headroom_tokens: config.context_minimum_headroom_tokens,
            context_headroom_percent: config.context_headroom_percent,
            context_repeated_input_ceiling_tokens: config.context_repeated_input_ceiling_tokens,
            context_minimum_retained_tail_tokens: config.context_minimum_retained_tail_tokens,
            context_maximum_retained_tail_tokens: config.context_maximum_retained_tail_tokens,
            context_retained_tail_percent: config.context_retained_tail_percent,
        }
    }
}

pub fn context_budget(model: &str, config: &AgentConfig) -> ContextBudget {
    threadlane_context::context_budget(model, &BudgetConfig::from(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_config_default_tracks_agent_config_default() {
        assert_eq!(
            BudgetConfig::from(&AgentConfig::default()),
            BudgetConfig::default(),
            "BudgetConfig::default must mirror AgentConfig::default or the leaf drifted",
        );
    }

    #[test]
    fn adapter_matches_previous_agent_config_behavior() {
        let config = AgentConfig::default();
        let large = context_budget("antigravity/gemini-3.7-flash", &config);
        assert_eq!(large.limit, 1_000_000);
        assert!(!large.limit_is_estimate);

        let unknown = context_budget("unknown/model", &config);
        assert_eq!(unknown.limit, 128_000);
        assert!(unknown.limit_is_estimate);
    }
}
