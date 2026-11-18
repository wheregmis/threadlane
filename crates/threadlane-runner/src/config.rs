//! [`RunnerConfig`]: where lanes work and how often the runner talks to its hub.

use std::path::PathBuf;
use std::time::Duration;

/// Default number of lanes worked at once.
const DEFAULT_MAX_PARALLEL: u32 = 2;
/// Default wait after a user message before a run starts (Plan.md §4.8).
const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(2);
/// Default heartbeat interval: a 60 s lease renewed every 20 s (Plan.md §6.4).
const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(20);
/// Default interval for asking for work without a nudge (Plan.md §6.5).
const DEFAULT_POLL: Duration = Duration::from_secs(30);

/// How a [`Runner`](crate::Runner) works lanes (docs/milestone-1.md §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerConfig {
    /// Parent of the per-lane working directories, `<lanes_dir>/<lane id>`.
    pub lanes_dir: PathBuf,
    /// How many lanes run at once.
    pub max_parallel: u32,
    /// How long to wait after a user message before starting a run.
    pub debounce: Duration,
    /// How often to report load and renew the leases held.
    pub heartbeat: Duration,
    /// How often to ask for work when no nudge arrives.
    pub poll: Duration,
}

impl RunnerConfig {
    /// The default timings, with lane directories under `lanes_dir`.
    #[must_use]
    pub fn new(lanes_dir: impl Into<PathBuf>) -> Self {
        Self {
            lanes_dir: lanes_dir.into(),
            ..Self::default()
        }
    }
}

impl Default for RunnerConfig {
    /// The default timings, with lane directories under `./lanes`.
    fn default() -> Self {
        Self {
            lanes_dir: PathBuf::from("lanes"),
            max_parallel: DEFAULT_MAX_PARALLEL,
            debounce: DEFAULT_DEBOUNCE,
            heartbeat: DEFAULT_HEARTBEAT,
            poll: DEFAULT_POLL,
        }
    }
}
