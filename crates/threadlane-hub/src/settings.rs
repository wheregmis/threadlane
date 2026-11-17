//! What a hub's roles are tuned with (docs/milestone-1.md §9).

use std::collections::HashMap;
use std::time::Duration;

use threadlane_core::model::BackendKind;

/// Timing and retry settings of one hub. `hub.toml` sets them through
/// [`crate::config::HubConfig::settings`]; tests build them directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSettings {
    /// Lease length (Plan.md §6.4).
    pub lease_ttl: Duration,
    /// Sweep and dispatcher fallback interval (Plan.md §6.5).
    pub poll: Duration,
    /// A lead lane resumes its latest session if it was active this recently
    /// (Plan.md §4.4 rule 5).
    pub idle_window: Duration,
    /// Events reaching this many failed attempts die (Plan.md §4.9).
    pub max_attempts: u32,
    /// First retry backoff, doubled per attempt.
    pub retry_backoff_base: Duration,
    /// The longest retry backoff.
    pub retry_backoff_max: Duration,
    /// From this many attempts on, events are retried one at a time (§4.9).
    pub isolate_after_attempts: u32,
    /// Wall-time limit of one run (Plan.md §4.5).
    pub wall_time_limit: Duration,
    /// Turn limit of one run, where the backend offers one.
    pub max_turns: Option<u32>,
    /// How many recent messages a new lead session is shown (Plan.md §8.5).
    pub recent_messages: u32,
    /// Backoff after a usage limit that gave no reset time.
    pub usage_limit_fallback: Duration,
    /// The model a backend runs when neither the agent nor its binding names
    /// one. A backend missing here runs its own default model.
    pub default_models: HashMap<BackendKind, String>,
}

impl Default for HubSettings {
    fn default() -> Self {
        Self {
            lease_ttl: Duration::from_secs(60),
            poll: Duration::from_secs(30),
            idle_window: Duration::from_secs(30 * 60),
            max_attempts: 5,
            retry_backoff_base: Duration::from_secs(10),
            retry_backoff_max: Duration::from_secs(10 * 60),
            isolate_after_attempts: 2,
            wall_time_limit: Duration::from_secs(10 * 60),
            max_turns: Some(40),
            recent_messages: 5,
            usage_limit_fallback: Duration::from_secs(5 * 60),
            default_models: HashMap::new(),
        }
    }
}

impl HubSettings {
    /// The backoff after a counted failure, given the highest attempt count
    /// among the run's events before it: `min(base * 2^attempts, max)`.
    #[must_use]
    pub fn retry_backoff(&self, attempts: u32) -> Duration {
        let factor = 2_u32.checked_pow(attempts).unwrap_or(u32::MAX);
        self.retry_backoff_base
            .checked_mul(factor)
            .unwrap_or(Duration::MAX)
            .min(self.retry_backoff_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_per_attempt_up_to_the_cap() {
        let settings = HubSettings::default();
        assert_eq!(settings.retry_backoff(0), Duration::from_secs(10));
        assert_eq!(settings.retry_backoff(1), Duration::from_secs(20));
        assert_eq!(settings.retry_backoff(2), Duration::from_secs(40));
        assert_eq!(settings.retry_backoff(6), Duration::from_secs(600));
        assert_eq!(settings.retry_backoff(200), Duration::from_secs(600));
    }
}
