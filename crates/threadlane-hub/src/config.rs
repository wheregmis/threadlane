//! Hub configuration, read from `hub.toml` in the hub's data directory.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use threadlane_backend_claude::ClaudeConfig;
use threadlane_backend_codex::CodexConfig;
use threadlane_core::model::{BackendKind, Mode, SpaceSettings};
use threadlane_runner::RunnerConfig;
use threadlane_store::Bootstrap;
use threadlane_transport_local::SOCKET_FILE;

use crate::settings::HubSettings;

/// Name of the configuration file inside the data directory.
pub const CONFIG_FILE: &str = "hub.toml";
/// Name of the database file inside the data directory.
pub const DATABASE_FILE: &str = "hub.db";
/// Name of the lock file that keeps a second process off the database.
pub const LOCK_FILE: &str = "hub.lock";
/// Name of the directory holding the embedded runner's lane directories.
pub const LANES_DIR: &str = "lanes";
/// Name of the directory holding the embedded runner's clones and task
/// worktrees.
pub const WORKSPACES_DIR: &str = "workspaces";
/// Name of the file holding the hub's iroh secret key, readable only by its owner.
pub const KEY_FILE: &str = "hub.key";

/// How the hub writes its logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// Human-readable lines.
    #[default]
    Pretty,
    /// One JSON object per line.
    Json,
}

/// The `[log]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    pub format: LogFormat,
    /// A `tracing` filter such as `info` or `threadlane_store=debug,info`.
    /// `RUST_LOG` overrides it.
    pub filter: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            format: LogFormat::Pretty,
            filter: "info".to_owned(),
        }
    }
}

/// The `[space]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpaceConfig {
    pub name: String,
    pub mode_ceiling: Mode,
    /// Name of the independent Reviewer for task submissions in this space.
    pub reviewer: Option<String>,
}

impl Default for SpaceConfig {
    fn default() -> Self {
        let defaults = SpaceSettings::default();
        Self {
            name: defaults.name,
            mode_ceiling: defaults.mode_ceiling,
            reviewer: None,
        }
    }
}

/// The `[timing]` table. Defaults are the starting points in Plan.md.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimingConfig {
    /// Lease length (Plan.md §6.4).
    pub lease_ttl_secs: u64,
    /// How often runners renew their leases (Plan.md §6.4).
    pub heartbeat_secs: u64,
    /// Slow poll that finds anything a lost wake hint missed (Plan.md §6.5).
    pub poll_secs: u64,
    /// Scheduler pass interval (Plan.md §7.3).
    pub scheduler_secs: u64,
    /// A lead lane resumes its latest session if it was active this recently
    /// (Plan.md §4.4 rule 5).
    pub idle_window_secs: u64,
    /// A session older than this is closed and a fresh one started
    /// (Plan.md §8.6).
    pub session_max_age_secs: u64,
    /// First retry backoff, doubled per attempt (Plan.md §4.9).
    pub retry_backoff_base_secs: u64,
    /// The longest retry backoff.
    pub retry_backoff_max_secs: u64,
    /// How long an account cools down after a usage limit that gave no
    /// reset time (Plan.md §5.3).
    pub usage_limit_fallback_secs: u64,
    /// How long a decision waits on you before it expires as declined with
    /// "no response" (docs/milestone-9.md decision 8).
    pub decision_expiry_secs: u64,
}

impl Default for TimingConfig {
    fn default() -> Self {
        Self {
            lease_ttl_secs: 60,
            heartbeat_secs: 20,
            poll_secs: 30,
            scheduler_secs: 10,
            idle_window_secs: 30 * 60,
            session_max_age_secs: 24 * 60 * 60,
            retry_backoff_base_secs: 10,
            retry_backoff_max_secs: 10 * 60,
            usage_limit_fallback_secs: 5 * 60,
            decision_expiry_secs: 24 * 60 * 60,
        }
    }
}

impl TimingConfig {
    /// Lease length as a duration.
    #[must_use]
    pub fn lease_ttl(&self) -> Duration {
        Duration::from_secs(self.lease_ttl_secs)
    }

    /// Poll interval as a duration.
    #[must_use]
    pub fn poll(&self) -> Duration {
        Duration::from_secs(self.poll_secs)
    }
}

/// The `[runs]` table: limits and retries of runs (Plan.md §4.5, §4.9, §8.5).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunsConfig {
    /// Events reaching this many failed attempts die.
    pub max_attempts: u32,
    /// From this many attempts on, events are retried one at a time.
    pub isolate_after_attempts: u32,
    /// Wall-time limit of one run.
    pub wall_time_limit_secs: u64,
    /// Turn limit of one run; `0` means none.
    pub max_turns: u32,
    /// How many recent messages a new lead session is shown.
    pub recent_messages: u32,
    /// An ended task's worktrees are removed once nothing in them would be
    /// lost; ones kept for uncommitted or unpushed work are checked again
    /// after this many days. The task branch stays. `0` keeps them all.
    pub worktree_cleanup_days: u32,
    /// Terminal tasks older than this many days have heavy artifacts compacted.
    /// `0` disables compaction.
    pub task_compaction_days: u32,
    /// Terminal tasks older than this many days are pruned. `0` disables
    /// age-based pruning.
    pub task_pruning_days: u32,
    /// Maximum terminal tasks retained per project. `0` disables the cap.
    pub max_terminal_tasks_per_project: u32,
}

impl Default for RunsConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            isolate_after_attempts: 2,
            // Engineering tasks that build and test a workspace need this much;
            // at 10 minutes and 40 turns most of them timed out at least once.
            wall_time_limit_secs: 30 * 60,
            max_turns: 150,
            recent_messages: 5,
            worktree_cleanup_days: 3,
            task_compaction_days: 14,
            task_pruning_days: 90,
            max_terminal_tasks_per_project: 500,
        }
    }
}

/// The `[runner]` table: the hub's embedded runner (docs/milestone-1.md §8).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunnerTable {
    /// How many lanes the embedded runner works at once.
    pub max_parallel: u32,
    /// How long to wait after a message from you before starting a run, so
    /// several messages in a row get one reply (Plan.md §4.8).
    pub debounce_secs: u64,
}

impl Default for RunnerTable {
    fn default() -> Self {
        let defaults = RunnerConfig::default();
        Self {
            max_parallel: defaults.max_parallel,
            debounce_secs: defaults.debounce.as_secs(),
        }
    }
}

/// The `[backends.claude]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeTable {
    /// The `claude` executable: a path, or a name looked up on `PATH`.
    pub program: PathBuf,
    /// Sets `CLAUDE_CONFIG_DIR`, for a separate login (Plan.md §9.3).
    pub config_dir: Option<PathBuf>,
    /// The model runs use when neither the agent nor its binding names one.
    /// Unset means Claude's own default.
    pub model: Option<String>,
}

impl Default for ClaudeTable {
    fn default() -> Self {
        Self {
            program: ClaudeConfig::default().program,
            config_dir: None,
            model: None,
        }
    }
}

impl ClaudeTable {
    /// How the embedded runner starts Claude.
    #[must_use]
    pub fn claude_config(&self) -> ClaudeConfig {
        ClaudeConfig {
            program: self.program.clone(),
            config_dir: self.config_dir.clone(),
        }
    }
}

/// The `[backends.codex]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexTable {
    /// The `codex` executable: a path, or a name looked up on `PATH`.
    pub program: PathBuf,
    /// Sets `CODEX_HOME`, for a separate login (Plan.md §9.3).
    pub home: Option<PathBuf>,
    /// The model runs use when neither the agent nor its binding names one.
    /// Unset means Codex's own default.
    pub model: Option<String>,
}

impl Default for CodexTable {
    fn default() -> Self {
        Self {
            program: CodexConfig::default().program,
            home: None,
            model: None,
        }
    }
}

impl CodexTable {
    /// How the embedded runner starts Codex.
    #[must_use]
    pub fn codex_config(&self) -> CodexConfig {
        CodexConfig {
            program: self.program.clone(),
            home: self.home.clone(),
        }
    }
}

/// Settings for the Antigravity CLI.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AntigravityTable {
    pub program: PathBuf,
    pub model: Option<String>,
}

impl Default for AntigravityTable {
    fn default() -> Self {
        Self {
            program: "agy".into(),
            model: None,
        }
    }
}

/// The `[backends]` table: one sub-table per backend.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BackendsConfig {
    pub claude: ClaudeTable,
    pub codex: CodexTable,
    pub antigravity: AntigravityTable,
}

/// The `[iroh]` table: the client interface over iroh (Plan.md §3.5).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IrohConfig {
    /// Serve clients over iroh, beside the Unix socket.
    pub enabled: bool,
    /// Announce the hub on the local network, so clients find it there even
    /// after its address changes.
    pub local_discovery: bool,
    /// The UDP address to listen on. Unset means every interface, IPv4 and
    /// IPv6, on a free port.
    pub bind: Option<SocketAddr>,
}

impl Default for IrohConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            local_discovery: true,
            bind: None,
        }
    }
}

/// The `[github]` table: how the hub reaches GitHub (Plan.md §6.6).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubConfig {
    /// The `gh` executable the GitHub poller and the embedded runner's
    /// forge run: a path, or a name looked up on `PATH`.
    pub gh: PathBuf,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self { gh: "gh".into() }
    }
}

/// The `[power]` table: how the hub keeps its machine working.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PowerConfig {
    /// On macOS, keep the machine from idle sleep while the hub runs, so
    /// timers fire on time and leases do not lapse.
    pub keep_awake: bool,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self { keep_awake: true }
    }
}

/// Everything in `hub.toml`. Every table is optional.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HubConfig {
    pub space: SpaceConfig,
    pub log: LogConfig,
    pub timing: TimingConfig,
    pub runs: RunsConfig,
    pub runner: RunnerTable,
    pub backends: BackendsConfig,
    pub iroh: IrohConfig,
    pub github: GithubConfig,
    pub power: PowerConfig,
}

impl HubConfig {
    /// Parses a configuration file's contents.
    ///
    /// # Errors
    ///
    /// Fails on invalid TOML, unknown keys, or inconsistent timing.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(text).context("invalid hub configuration")?;
        config.validate()?;
        Ok(config)
    }

    /// Reads `hub.toml` from `data_dir`, or the defaults if there is none.
    ///
    /// # Errors
    ///
    /// Fails if the file exists but cannot be read or parsed.
    pub fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let path = data_dir.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("in {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        let timing = &self.timing;
        anyhow::ensure!(
            timing.lease_ttl_secs > 0,
            "timing.lease_ttl_secs must be positive"
        );
        anyhow::ensure!(
            timing.heartbeat_secs > 0 && timing.heartbeat_secs < timing.lease_ttl_secs,
            "timing.heartbeat_secs must be positive and shorter than the lease"
        );
        anyhow::ensure!(timing.poll_secs > 0, "timing.poll_secs must be positive");
        anyhow::ensure!(
            timing.scheduler_secs > 0,
            "timing.scheduler_secs must be positive"
        );
        anyhow::ensure!(
            timing.retry_backoff_base_secs > 0
                && timing.retry_backoff_max_secs >= timing.retry_backoff_base_secs,
            "timing.retry_backoff_base_secs must be positive and at most retry_backoff_max_secs"
        );
        anyhow::ensure!(
            timing.usage_limit_fallback_secs > 0,
            "timing.usage_limit_fallback_secs must be positive"
        );
        anyhow::ensure!(
            timing.session_max_age_secs > 0,
            "timing.session_max_age_secs must be positive"
        );
        anyhow::ensure!(
            timing.decision_expiry_secs > 0,
            "timing.decision_expiry_secs must be positive"
        );
        let runs = &self.runs;
        anyhow::ensure!(runs.max_attempts > 0, "runs.max_attempts must be positive");
        anyhow::ensure!(
            runs.isolate_after_attempts > 0,
            "runs.isolate_after_attempts must be positive"
        );
        anyhow::ensure!(
            runs.wall_time_limit_secs > 0,
            "runs.wall_time_limit_secs must be positive"
        );
        anyhow::ensure!(
            !self.space.name.trim().is_empty(),
            "space.name must not be empty"
        );
        let runner = &self.runner;
        anyhow::ensure!(
            runner.max_parallel > 0,
            "runner.max_parallel must be positive"
        );
        anyhow::ensure!(
            runner.debounce_secs < timing.lease_ttl_secs,
            "runner.debounce_secs must be shorter than the lease"
        );
        anyhow::ensure!(
            !self.github.gh.as_os_str().is_empty(),
            "github.gh must not be empty"
        );
        let claude = &self.backends.claude;
        let codex = &self.backends.codex;
        for (name, program, dir, dir_key, model) in [
            (
                "claude",
                &claude.program,
                &claude.config_dir,
                "config_dir",
                &claude.model,
            ),
            (
                "antigravity",
                &self.backends.antigravity.program,
                &None,
                "",
                &self.backends.antigravity.model,
            ),
            ("codex", &codex.program, &codex.home, "home", &codex.model),
        ] {
            anyhow::ensure!(
                !program.as_os_str().is_empty(),
                "backends.{name}.program must not be empty"
            );
            anyhow::ensure!(
                dir.as_ref().is_none_or(|dir| !dir.as_os_str().is_empty()),
                "backends.{name}.{dir_key} must not be empty when set"
            );
            anyhow::ensure!(
                model
                    .as_ref()
                    .is_none_or(|model| !model.trim().is_empty() && model.trim() == model),
                "backends.{name}.model must be a model name without surrounding spaces"
            );
        }
        Ok(())
    }

    /// What a brand-new database is initialized with.
    #[must_use]
    pub fn bootstrap(&self) -> Bootstrap {
        Bootstrap {
            space: SpaceSettings {
                name: self.space.name.clone(),
                mode_ceiling: self.space.mode_ceiling,
                ..SpaceSettings::default()
            },
            ..Bootstrap::default()
        }
    }

    /// The settings `HubCore` runs with.
    #[must_use]
    pub fn settings(&self) -> HubSettings {
        let timing = &self.timing;
        let runs = &self.runs;
        HubSettings {
            reviewer: self.space.reviewer.clone(),
            lease_ttl: timing.lease_ttl(),
            poll: timing.poll(),
            idle_window: Duration::from_secs(timing.idle_window_secs),
            session_max_age: Duration::from_secs(timing.session_max_age_secs),
            max_attempts: runs.max_attempts,
            retry_backoff_base: Duration::from_secs(timing.retry_backoff_base_secs),
            retry_backoff_max: Duration::from_secs(timing.retry_backoff_max_secs),
            isolate_after_attempts: runs.isolate_after_attempts,
            wall_time_limit: Duration::from_secs(runs.wall_time_limit_secs),
            max_turns: (runs.max_turns > 0).then_some(runs.max_turns),
            recent_messages: runs.recent_messages,
            worktree_cleanup: (runs.worktree_cleanup_days > 0)
                .then(|| Duration::from_secs(u64::from(runs.worktree_cleanup_days) * 24 * 60 * 60)),
            task_compaction: (runs.task_compaction_days > 0)
                .then(|| Duration::from_secs(u64::from(runs.task_compaction_days) * 24 * 60 * 60)),
            task_pruning: (runs.task_pruning_days > 0)
                .then(|| Duration::from_secs(u64::from(runs.task_pruning_days) * 24 * 60 * 60)),
            max_terminal_tasks_per_project: (runs.max_terminal_tasks_per_project > 0)
                .then_some(runs.max_terminal_tasks_per_project as usize),
            pruning_interval: HubSettings::default().pruning_interval,
            pruning_batch_size: HubSettings::default().pruning_batch_size,
            usage_limit_fallback: Duration::from_secs(timing.usage_limit_fallback_secs),
            scheduler: Duration::from_secs(timing.scheduler_secs),
            decision_expiry: Duration::from_secs(timing.decision_expiry_secs),
            default_models: [
                (BackendKind::Claude, &self.backends.claude.model),
                (BackendKind::Codex, &self.backends.codex.model),
                (BackendKind::Antigravity, &self.backends.antigravity.model),
            ]
            .into_iter()
            .filter_map(|(kind, model)| Some((kind, model.clone()?)))
            .collect(),
        }
    }

    /// How the embedded runner works, with its lane directories under
    /// `lanes_dir`.
    #[must_use]
    pub fn runner_config(&self, lanes_dir: PathBuf) -> RunnerConfig {
        RunnerConfig {
            lanes_dir,
            max_parallel: self.runner.max_parallel,
            debounce: Duration::from_secs(self.runner.debounce_secs),
            heartbeat: Duration::from_secs(self.timing.heartbeat_secs),
            poll: self.timing.poll(),
        }
    }
}

/// Paths inside one hub's data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// The data directory at `root`. Touches nothing on disk.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The hub's database file.
    #[must_use]
    pub fn database(&self) -> PathBuf {
        self.root.join(DATABASE_FILE)
    }

    /// The lock file that keeps a second process out.
    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// The client interface's Unix socket.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.root.join(SOCKET_FILE)
    }

    /// The hub's iroh secret key.
    #[must_use]
    pub fn key(&self) -> PathBuf {
        self.root.join(KEY_FILE)
    }

    /// Where the embedded runner keeps one working directory per lane.
    #[must_use]
    pub fn lanes(&self) -> PathBuf {
        self.root.join(LANES_DIR)
    }

    /// Where the embedded runner keeps its clones and task worktrees.
    #[must_use]
    pub fn workspaces(&self) -> PathBuf {
        self.root.join(WORKSPACES_DIR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_plan_defaults() {
        let config = HubConfig::parse("").unwrap();
        assert_eq!(config, HubConfig::default());
        assert_eq!(config.timing.lease_ttl(), Duration::from_secs(60));
        assert_eq!(config.timing.poll(), Duration::from_secs(30));
        assert_eq!(config.settings(), HubSettings::default());
    }

    #[test]
    fn run_settings_parse() {
        let config = HubConfig::parse(
            r#"
            [timing]
            idle_window_secs = 600
            session_max_age_secs = 3600
            retry_backoff_base_secs = 5
            retry_backoff_max_secs = 60
            usage_limit_fallback_secs = 120

            [runs]
            max_attempts = 3
            isolate_after_attempts = 1
            wall_time_limit_secs = 300
            max_turns = 0
            recent_messages = 2
            worktree_cleanup_days = 0
            "#,
        )
        .unwrap();
        let settings = config.settings();
        assert_eq!(settings.idle_window, Duration::from_secs(600));
        assert_eq!(settings.session_max_age, Duration::from_secs(3600));
        assert_eq!(settings.retry_backoff_base, Duration::from_secs(5));
        assert_eq!(settings.retry_backoff_max, Duration::from_secs(60));
        assert_eq!(settings.usage_limit_fallback, Duration::from_secs(120));
        assert_eq!(settings.max_attempts, 3);
        assert_eq!(settings.isolate_after_attempts, 1);
        assert_eq!(settings.wall_time_limit, Duration::from_secs(300));
        assert_eq!(settings.max_turns, None, "0 means no turn limit");
        assert_eq!(settings.recent_messages, 2);
        assert_eq!(settings.worktree_cleanup, None);
        assert_eq!(
            HubConfig::default().settings().worktree_cleanup,
            Some(Duration::from_secs(3 * 24 * 60 * 60))
        );
        assert_eq!(settings.lease_ttl, Duration::from_secs(60));
    }

    #[test]
    fn bad_run_settings_are_rejected() {
        assert!(HubConfig::parse("[runs]\nmax_attempts = 0").is_err());
        assert!(HubConfig::parse("[runs]\nisolate_after_attempts = 0").is_err());
        assert!(HubConfig::parse("[runs]\nwall_time_limit_secs = 0").is_err());
        assert!(HubConfig::parse("[runs]\nmax_atempts = 3").is_err());
        assert!(HubConfig::parse("[timing]\nretry_backoff_base_secs = 0").is_err());
        assert!(HubConfig::parse("[timing]\nsession_max_age_secs = 0").is_err());
        assert!(
            HubConfig::parse("[timing]\nretry_backoff_base_secs = 60\nretry_backoff_max_secs = 10")
                .is_err()
        );
        assert!(HubConfig::parse("[timing]\nusage_limit_fallback_secs = 0").is_err());
    }

    #[test]
    fn full_config_parses() {
        let config = HubConfig::parse(
            r#"
            [space]
            name = "Work"
            mode_ceiling = "assisted"

            [log]
            format = "json"
            filter = "threadlane_store=debug,info"

            [timing]
            lease_ttl_secs = 90
            heartbeat_secs = 30
            "#,
        )
        .unwrap();
        assert_eq!(config.space.name, "Work");
        assert_eq!(config.space.mode_ceiling, Mode::Assisted);
        assert_eq!(config.log.format, LogFormat::Json);
        assert_eq!(config.timing.lease_ttl_secs, 90);
        assert_eq!(config.timing.poll_secs, 30, "unset keys keep defaults");
        assert_eq!(config.bootstrap().space.name, "Work");
    }

    #[test]
    fn bad_config_is_rejected() {
        assert!(HubConfig::parse("[space]\nnmae = \"typo\"").is_err());
        assert!(HubConfig::parse("[space]\nmode_ceiling = \"yolo\"").is_err());
        assert!(HubConfig::parse("[timing]\nheartbeat_secs = 60").is_err());
        assert!(HubConfig::parse("[space]\nname = \" \"").is_err());
    }

    #[test]
    fn runner_and_backend_settings_parse() {
        let config = HubConfig::parse(
            r#"
            [timing]
            heartbeat_secs = 15
            poll_secs = 45

            [runner]
            max_parallel = 3
            debounce_secs = 5

            [backends.claude]
            program = "/opt/claude/bin/claude"
            config_dir = "/Users/me/.claude-work"
            model = "haiku"

            [backends.codex]
            program = "/opt/codex/bin/codex"
            home = "/Users/me/.codex-work"
            model = "gpt-5-codex"
            "#,
        )
        .unwrap();
        let runner = config.runner_config(PathBuf::from("/data/lanes"));
        assert_eq!(
            runner,
            RunnerConfig {
                lanes_dir: PathBuf::from("/data/lanes"),
                max_parallel: 3,
                debounce: Duration::from_secs(5),
                heartbeat: Duration::from_secs(15),
                poll: Duration::from_secs(45),
            }
        );
        assert_eq!(
            config.backends.claude.claude_config(),
            ClaudeConfig {
                program: PathBuf::from("/opt/claude/bin/claude"),
                config_dir: Some(PathBuf::from("/Users/me/.claude-work")),
            }
        );
        assert_eq!(
            config.backends.codex.codex_config(),
            CodexConfig {
                program: PathBuf::from("/opt/codex/bin/codex"),
                home: Some(PathBuf::from("/Users/me/.codex-work")),
            }
        );
        let models = config.settings().default_models;
        assert_eq!(models.get(&BackendKind::Claude), Some(&"haiku".to_owned()));
        assert_eq!(
            models.get(&BackendKind::Codex),
            Some(&"gpt-5-codex".to_owned())
        );
    }

    #[test]
    fn runner_and_backend_defaults() {
        let config = HubConfig::default();
        let runner = config.runner_config(PathBuf::from("lanes"));
        assert_eq!(runner, RunnerConfig::new("lanes"));
        assert_eq!(
            config.backends.claude.claude_config(),
            ClaudeConfig::default()
        );
        assert_eq!(config.backends.codex.codex_config(), CodexConfig::default());
        assert!(config.settings().default_models.is_empty());
    }

    #[test]
    fn bad_runner_and_backend_settings_are_rejected() {
        for bad in [
            "[runner]\nmax_parallel = 0",
            "[runner]\ndebounce_secs = 60",
            "[runner]\ndebounce = 2",
            "[backends.claude]\nprogram = \"\"",
            "[backends.claude]\nconfig_dir = \"\"",
            "[backends.claude]\nmodel = \"\"",
            "[backends.claude]\nmodel = \" haiku\"",
            "[backends.claude]\nmodle = \"haiku\"",
            "[backends.antigravity]\nprogram = \"\"",
            "[backends.antigravity]\nmodel = \" bad\"",
            "[backends.codex]\nprogram = \"\"",
            "[backends.codex]\nhome = \"\"",
            "[backends.codex]\nmodel = \" gpt\"",
            "[backends.codex]\nconfig_dir = \"/x\"",
            "[backends.gemini]\nmodel = \"x\"",
        ] {
            assert!(HubConfig::parse(bad).is_err(), "{bad:?} is rejected");
        }
        assert!(
            HubConfig::parse("[runner]\ndebounce_secs = 0").is_ok(),
            "no debounce is allowed"
        );
    }

    #[test]
    fn power_settings_parse() {
        assert!(HubConfig::default().power.keep_awake);
        let config = HubConfig::parse("[power]\nkeep_awake = false").unwrap();
        assert!(!config.power.keep_awake);
        assert!(HubConfig::parse("[power]\nkeep_awake = \"yes\"").is_err());
        assert!(HubConfig::parse("[power]\nkeepawake = true").is_err());
    }

    #[test]
    fn iroh_settings_parse() {
        let config = HubConfig::default();
        assert!(config.iroh.enabled && config.iroh.local_discovery);
        assert_eq!(config.iroh.bind, None);
        let config = HubConfig::parse(
            "[iroh]\nenabled = false\nlocal_discovery = false\nbind = \"127.0.0.1:7720\"",
        )
        .unwrap();
        assert!(!config.iroh.enabled && !config.iroh.local_discovery);
        assert_eq!(config.iroh.bind, Some("127.0.0.1:7720".parse().unwrap()));
        assert!(HubConfig::parse("[iroh]\nrelay = true").is_err());
        assert!(HubConfig::parse("[iroh]\nbind = \"not an address\"").is_err());
    }

    #[test]
    fn missing_file_means_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(HubConfig::load(dir.path()).unwrap(), HubConfig::default());
        std::fs::write(dir.path().join(CONFIG_FILE), "[space]\nname = \"Personal\"").unwrap();
        assert_eq!(HubConfig::load(dir.path()).unwrap().space.name, "Personal");
    }
}
