//! Serializable token accounting shared by native and browser presentation.
use crate::TokenUsage;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EfficiencyUsage {
    pub uncached_input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
}

impl EfficiencyUsage {
    pub fn add(&mut self, usage: &TokenUsage) {
        self.uncached_input_tokens += u64::from(usage.input_tokens);
        self.cache_read_tokens += u64::from(usage.cache_read_tokens);
        self.cache_write_tokens += u64::from(usage.cache_write_tokens);
        self.output_tokens += u64::from(usage.output_tokens);
    }

    fn input_tokens(&self) -> u64 {
        self.uncached_input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    /// Processed context and output, including cache reads; not a billed-cost total.
    pub fn processed_tokens(&self) -> u64 {
        self.input_tokens() + self.output_tokens
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ContextContribution {
    pub estimated_tokens: u64,
    /// Repeated content is an opportunity to inspect, not necessarily waste.
    pub repeated_estimated_tokens: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LaneEfficiency {
    pub provider_requests: u64,
    pub requests_with_usage: u64,
    pub failed_provider_requests: u64,
    pub reduced_context_items: u64,
    pub usage: EfficiencyUsage,
    pub context: BTreeMap<String, ContextContribution>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TokenEfficiencyReport {
    pub usage: EfficiencyUsage,
    pub completed_foreground_runs: u64,
    /// Session-wide usage, including failed attempts and children, divided by
    /// completed foreground runs. None when no foreground run completed.
    pub session_tokens_per_completed_foreground_run: Option<f64>,
    pub lanes: BTreeMap<String, LaneEfficiency>,
    pub snapshot_loads: u64,
    pub stale_snapshot_loads: u64,
    pub repeated_snapshot_reads: u64,
    pub compactions: u64,
    pub calibrated_requests: u64,
    pub calibrated_estimated_input_tokens: u64,
    pub calibrated_actual_input_tokens: u64,
}
