//! Driving one command-line agent (Plan.md §5.7).

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::ids::MachineId;
use crate::model::BackendKind;
use crate::time::Timestamp;
use crate::tools::ToolEndpoint;

/// What [`AgentBackend::probe`] found on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub installed: bool,
    pub version: Option<String>,
    pub logged_in: bool,
    /// The account label this membership uses, if known.
    pub account_label: Option<String>,
}

impl Probe {
    /// Whether runs can be started on this backend.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.installed && self.logged_in
    }
}

/// Encodes probe results as the `backends` object of a
/// [`MachineReport`](crate::model::MachineReport): backend name to [`Probe`].
#[must_use]
pub fn encode_backends(probes: &[(BackendKind, Probe)]) -> Json {
    let map = probes
        .iter()
        .map(|(kind, probe)| {
            let probe = serde_json::to_value(probe).unwrap_or(Json::Null);
            (kind.as_str().to_owned(), probe)
        })
        .collect();
    Json::Object(map)
}

/// The backends a machine reported as installed and logged in, in the order
/// of [`BackendKind::ALL`]. Entries that do not decode are ignored.
#[must_use]
pub fn usable_backends(backends: &Json) -> Vec<BackendKind> {
    usable_probes(backends)
        .into_iter()
        .map(|(kind, _)| kind)
        .collect()
}

fn usable_probes(backends: &Json) -> Vec<(BackendKind, Probe)> {
    BackendKind::ALL
        .iter()
        .filter_map(|kind| {
            let probe =
                serde_json::from_value::<Probe>(backends.get(kind.as_str())?.clone()).ok()?;
            probe.usable().then_some((*kind, probe))
        })
        .collect()
}

/// The label of the account `kind` runs on, on `machine`: the one its probe
/// reported, else one for this backend on this machine, so that its usage
/// limit can still be recorded (Plan.md §5.3).
#[must_use]
pub fn account_label(kind: BackendKind, reported: Option<&str>, machine: MachineId) -> String {
    match reported {
        Some(label) if !label.trim().is_empty() => label.trim().to_owned(),
        _ => format!("{kind}@machine-{machine}"),
    }
}

/// The usable backends of `machine` whose account is not in `cooling`, the
/// labels of the accounts cooling down now (Plan.md §5.3, §5.4 item 4).
#[must_use]
pub fn free_backends(backends: &Json, machine: MachineId, cooling: &[String]) -> Vec<BackendKind> {
    usable_probes(backends)
        .into_iter()
        .filter(|(kind, probe)| {
            let label = account_label(*kind, probe.account_label.as_deref(), machine);
            !cooling.contains(&label)
        })
        .map(|(kind, _)| kind)
        .collect()
}

/// The direct forms of shared effects every task run must refuse (Plan.md
/// §9.2 layer 2), as command prefixes: a command is refused when its words
/// start with one of these. Force variants (`git push --force`, `gh pr merge
/// --admin`) start with the same words. Written once here; each driver
/// translates them into its backend's own rules.
pub const DENY_RULES: &[&str] = &[
    "git push",
    "gh pr create",
    "gh pr comment",
    "gh issue comment",
    "gh issue create",
    "gh issue new",
    "gh pr merge",
    "gh release",
    "gh repo delete",
];

/// Whether a run starts the backend's session or resumes it (Plan.md §4.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum NativeSession {
    /// Start a new session. `id` is the identifier to give it where the
    /// backend lets the caller choose (Claude); a backend that assigns its
    /// own (Codex) ignores it and reports the real one in
    /// [`BackendEvent::SessionStarted`].
    Start { id: String },
    /// Resume this session. If the backend no longer has it, the run ends
    /// with [`BackendEvent::SessionMissing`] and nothing else.
    Resume { id: String },
}

/// Everything needed to start or resume one run (Plan.md §5.7 "Run").
#[derive(Clone)]
pub struct RunRequest {
    /// Must be the same directory every time a session is resumed: backends
    /// file their sessions by working directory.
    pub working_dir: PathBuf,
    /// The per-run prompt (Plan.md §8.5 items 4 to 7).
    pub prompt: String,
    /// Stable across runs until memory changes (Plan.md §8.5 items 1 to 3).
    pub appended_instructions: String,
    pub model: Option<String>,
    /// Start the backend's session or resume it.
    pub session: NativeSession,
    /// The system's tools, served to the agent through the tool server.
    pub tools: Option<Arc<dyn ToolEndpoint>>,
    /// Direct commands the backend must refuse (Plan.md §9.2), as command
    /// prefixes like those in [`DENY_RULES`]; the driver translates them.
    pub deny_rules: Vec<String>,
    /// Remove every built-in tool of the backend, leaving only the system's
    /// tools (leads and dreaming, Plan.md §5.1).
    pub remove_builtin_tools: bool,
    pub wall_time_limit: Duration,
    pub max_turns: Option<u32>,
}

impl fmt::Debug for RunRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunRequest")
            .field("working_dir", &self.working_dir)
            .field("prompt_len", &self.prompt.len())
            .field(
                "appended_instructions_len",
                &self.appended_instructions.len(),
            )
            .field("model", &self.model)
            .field("session", &self.session)
            .field("has_tools", &self.tools.is_some())
            .field("deny_rules", &self.deny_rules)
            .field("remove_builtin_tools", &self.remove_builtin_tools)
            .field("wall_time_limit", &self.wall_time_limit)
            .field("max_turns", &self.max_turns)
            .finish()
    }
}

/// One thing the agent did, mirrored to the hub (Plan.md §4.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BackendEvent {
    /// The backend reported its own session id.
    SessionStarted { native_session_id: String },
    /// The session to resume does not exist on this machine. Terminal.
    SessionMissing,
    /// Assistant text.
    Text { text: String },
    /// The agent used a tool (its own or the system's). For the system's
    /// tools `name` is the bare tool name (`send_message`), without any
    /// prefix the backend adds.
    ToolUse {
        call_id: Option<String>,
        name: String,
        input: Json,
    },
    /// A tool returned.
    ToolResult {
        call_id: Option<String>,
        output: Json,
        is_error: bool,
    },
    /// The account reached its usage limit (Plan.md §5.3).
    UsageLimit { resets_at: Option<Timestamp> },
    /// The run finished. `final_text` becomes the run's rationale.
    Finished {
        final_text: Option<String>,
        usage: Json,
    },
    /// The run failed.
    Failed { error: String },
    /// The backend is between turns and can accept another user message.
    TurnBoundary,
}

impl BackendEvent {
    /// Whether this event ends the run.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Finished { .. }
                | Self::Failed { .. }
                | Self::UsageLimit { .. }
                | Self::SessionMissing
        )
    }
}

/// A one-shot question with a structured answer and no tools (Plan.md §5.7 "Ask").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskRequest {
    pub question: String,
    /// JSON schema the answer must satisfy.
    pub answer_schema: Json,
    pub model: Option<String>,
    pub timeout: Duration,
}

/// Why a backend operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BackendError {
    #[error("backend not installed")]
    NotInstalled,
    #[error("backend not logged in")]
    NotLoggedIn,
    #[error("usage limit reached")]
    UsageLimit { resets_at: Option<Timestamp> },
    #[error("timed out")]
    Timeout,
    #[error("backend failed: {0}")]
    Failed(String),
}

/// A run in progress.
#[async_trait]
pub trait BackendRun: Send + fmt::Debug {
    /// The next event, or `None` once the run has ended.
    async fn next_event(&mut self) -> Option<BackendEvent>;
    /// Offers trusted user input after a turn boundary. Returns false when unsupported.
    async fn fold_in(&mut self, _message: &str) -> Result<bool, BackendError> {
        Ok(false)
    }

    /// Halts the run on cancel or limit.
    async fn stop(&mut self) -> Result<(), BackendError>;
}

/// Drives one command-line agent: probe, run, events, ask, stop.
#[async_trait]
pub trait AgentBackend: Send + Sync + fmt::Debug {
    fn kind(&self) -> BackendKind;
    async fn probe(&self) -> Result<Probe, BackendError>;
    /// Starts a new session or resumes `request.resume`.
    async fn run(&self, request: RunRequest) -> Result<Box<dyn BackendRun>, BackendError>;
    async fn ask(&self, request: AskRequest) -> Result<Json, BackendError>;
}
