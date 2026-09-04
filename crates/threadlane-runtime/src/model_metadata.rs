use crate::AgentConfig;

pub const UNKNOWN_MODEL_CONTEXT_LIMIT: usize = 128_000;

pub fn model_context_limit(model: &str) -> Option<usize> {
    let unadorned = model
        .strip_prefix("antigravity/")
        .or_else(|| model.strip_prefix("opencode-go/"))
        .unwrap_or(model);
    match unadorned {
        "gemini-3.7-flash" | "gemini-3.6-flash" | "gemini-3.5-flash" => Some(1_000_000),
        "gemini-3.1-pro" => Some(2_000_000),
        "gpt-5.6-luna" | "gpt-5.4" | "gpt-5.5" | "gpt-5.6-sol" | "gpt-5.6-terra" => Some(1_000_000),
        "gpt-5.4-mini" | "gpt-4o" | "gpt-4o-mini" => Some(128_000),
        "claude-sonnet-4-6" | "claude-opus-4-6" => Some(200_000),
        "gpt-oss-120b" => Some(128_000),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextBudget {
    pub limit: usize,
    pub limit_is_estimate: bool,
    pub trigger_tokens: usize,
    pub retained_tail_tokens: usize,
    pub strict_retained_tail_tokens: usize,
}

impl ContextBudget {
    fn from_limit(limit: Option<usize>, config: &AgentConfig) -> Self {
        let minimum_valid = config.context_minimum_headroom_tokens.saturating_mul(2);
        let known = limit.filter(|value| *value >= minimum_valid);
        let fallback = config
            .unknown_model_context_limit
            .max(UNKNOWN_MODEL_CONTEXT_LIMIT);
        let limit = known.unwrap_or(fallback);
        let proportional_headroom = limit
            .saturating_mul(config.context_headroom_percent)
            .div_ceil(100);
        let headroom = proportional_headroom.max(config.context_minimum_headroom_tokens);
        let safe_budget = limit.saturating_sub(headroom);
        let trigger_tokens = safe_budget.min(config.context_repeated_input_ceiling_tokens);
        let proportional_tail = limit
            .saturating_mul(config.context_retained_tail_percent)
            .div_ceil(100);
        let mut retained_tail_tokens = proportional_tail
            .min(config.context_maximum_retained_tail_tokens)
            .min(trigger_tokens.saturating_sub(1));
        if trigger_tokens > config.context_minimum_retained_tail_tokens {
            retained_tail_tokens =
                retained_tail_tokens.max(config.context_minimum_retained_tail_tokens);
        }
        let strict_retained_tail_tokens = retained_tail_tokens
            .div_ceil(2)
            .max(config.context_minimum_retained_tail_tokens)
            .min(trigger_tokens.saturating_sub(1));
        Self {
            limit,
            limit_is_estimate: known.is_none(),
            trigger_tokens,
            retained_tail_tokens,
            strict_retained_tail_tokens,
        }
    }

    /// Speculative trigger token threshold (~85% of trigger_tokens)
    /// to trigger background pre-compaction / output shaking before hard boundary stalls.
    pub fn speculative_trigger_tokens(&self) -> usize {
        self.trigger_tokens.saturating_mul(85) / 100
    }
}

pub fn context_budget(model: &str, config: &AgentConfig) -> ContextBudget {
    ContextBudget::from_limit(model_context_limit(model), config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentConfig;

    #[test]
    fn adaptive_budget_balances_known_large_and_unknown_models() {
        let config = AgentConfig::default();
        let large = context_budget("antigravity/gemini-3.7-flash", &config);
        assert_eq!(large.limit, 1_000_000);
        assert!(!large.limit_is_estimate);
        assert_eq!(large.trigger_tokens, 256_000);
        assert_eq!(large.retained_tail_tokens, 64_000);
        assert_eq!(large.strict_retained_tail_tokens, 32_000);

        let unknown = context_budget("unknown/model", &config);
        assert_eq!(unknown.limit, 128_000);
        assert!(unknown.limit_is_estimate);
        assert_eq!(unknown.trigger_tokens, 96_000);
        assert_eq!(unknown.retained_tail_tokens, 32_000);
        assert_eq!(unknown.strict_retained_tail_tokens, 20_000);
    }

    #[test]
    fn invalid_known_limit_uses_saturating_fallback_policy() {
        let mut config = AgentConfig::default();
        config.unknown_model_context_limit = 16_000;
        let budget = ContextBudget::from_limit(None, &config);
        assert_eq!(budget.limit, 128_000);
        assert_eq!(budget.trigger_tokens, 96_000);
    }
}
