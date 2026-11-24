//! What a hub's roles are tuned with (docs/milestone-1.md §9).

use std::collections::HashMap;
use std::time::Duration;

use threadlane_core::model::BackendKind;

/// Timing and retry settings of one hub. `hub.toml` sets them through
/// [`crate::config::HubConfig::settings`]; tests build them directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSettings {
    /// The space's independent Reviewer, when configured.
    pub reviewer: Option<String>,
    /// Lease length (Plan.md §6.4).
    pub lease_ttl: Duration,
    /// Sweep and dispatcher fallback interval (Plan.md §6.5).
    pub poll: Duration,
    /// A lead lane resumes its latest session if it was active this recently
    /// (Plan.md §4.4 rule 5).
    pub idle_window: Duration,
    /// A session older than this is closed and a fresh one started, from
    /// the task's progress file for a task (Plan.md §8.6).
    pub session_max_age: Duration,
    /// Events and outbox deliveries die after this many failed attempts.
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
    /// How long an account cools down after a usage limit that gave no
    /// reset time (Plan.md §5.3).
    pub usage_limit_fallback: Duration,
    /// Scheduler pass interval (Plan.md §7.3).
    pub scheduler: Duration,
    /// The model a backend runs when neither the agent nor its binding names
    /// one. A backend missing here runs its own default model.
    pub default_models: HashMap<BackendKind, String>,
    /// How long a decision waits on you before it expires as declined with
    /// "no response" (docs/milestone-9.md decision 8).
    pub decision_expiry: Duration,
    /// An ended task's worktrees are removed once nothing in them would be
    /// lost; worktrees kept for uncommitted or unpushed work are checked
    /// again after this long. `None` keeps them all.
    pub worktree_cleanup: Option<Duration>,
    /// Terminal tasks older than this have heavy artifacts (journal, tool calls,
    /// checkpoints, progress file) compacted; `None` disables compaction.
    pub task_compaction: Option<Duration>,
    /// Terminal tasks older than this are pruned from the database; `None`
    /// disables age-based pruning.
    pub task_pruning: Option<Duration>,
    /// Maximum number of terminal tasks kept per project (oldest pruned first);
    /// `None` disables count-based pruning.
    pub max_terminal_tasks_per_project: Option<usize>,
    /// Minimum interval between automatic pruning passes in `sweep_once`.
    pub pruning_interval: Duration,
    /// Maximum number of tasks compacted or pruned per pass.
    pub pruning_batch_size: usize,
}

impl Default for HubSettings {
    fn default() -> Self {
        Self {
            reviewer: None,
            lease_ttl: Duration::from_secs(60),
            poll: Duration::from_secs(30),
            idle_window: Duration::from_secs(30 * 60),
            session_max_age: Duration::from_secs(24 * 60 * 60),
            max_attempts: 5,
            retry_backoff_base: Duration::from_secs(10),
            retry_backoff_max: Duration::from_secs(10 * 60),
            isolate_after_attempts: 2,
            wall_time_limit: Duration::from_secs(30 * 60),
            max_turns: Some(150),
            recent_messages: 5,
            usage_limit_fallback: Duration::from_secs(30 * 60),
            scheduler: Duration::from_secs(10),
            default_models: HashMap::new(),
            decision_expiry: Duration::from_secs(24 * 60 * 60),
            worktree_cleanup: Some(Duration::from_secs(3 * 24 * 60 * 60)),
            task_compaction: Some(Duration::from_secs(14 * 24 * 60 * 60)),
            task_pruning: Some(Duration::from_secs(90 * 24 * 60 * 60)),
            max_terminal_tasks_per_project: Some(500),
            pruning_interval: Duration::from_secs(60 * 60),
            pruning_batch_size: 50,
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
