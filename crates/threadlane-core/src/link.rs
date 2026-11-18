//! The runner's connection to its hub (Plan.md §5.6).
//!
//! The runner dials the hub; the hub never dials a machine. The hub decides
//! and the runner executes: session routing (§4.4), the initiative check
//! (§4.3 step 3), and context assembly (§8.5) all need hub state, so
//! [`HubLink::begin_run`] returns a ready [`RunAssignment`]. Every call that
//! changes state carries the lease, and a stale epoch is rejected.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::backend::NativeSession;
use crate::ids::{DecisionId, EventId, LaneId, ProjectId, RunId, SessionId, TaskId};
use crate::model::{
    BackendKind, Event, JournalEntry, LaneKind, Lease, MachineReport, Repository, RunOutcome,
};
use crate::runtime::Killed;
use crate::time::Timestamp;
use crate::tools::{ToolDefinition, ToolError};

/// The field the hub sets to `true` in the result of a memory call that
/// wrote the run's own task progress file. The runner reads it to know
/// whether a task run needs its final progress turn (docs/milestone-5.md §1
/// decision 7).
pub const PROGRESS_WRITTEN: &str = "progress_written";

/// The tool a task agent reports its task done with.
pub const FINISH_TASK: &str = "finish_task";

/// The field of a `finish_task` call the runner sets to where the task
/// branch stands against each repository's default branch, a list of
/// [`crate::workspace::MergeCheck`] (docs/merge-help.md decision 2). An
/// agent's own value is replaced or removed.
pub const MERGE_CHECKS: &str = "merge";

/// Everything a runner needs to execute one run. Built by the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunAssignment {
    /// A project memory pass, isolated from user turns.
    #[serde(default)]
    pub dreaming: bool,
    /// An ask from the space Lead. Keep unrelated user input for the next run.
    #[serde(default)]
    pub space_lead_ask: Option<EventId>,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub lane_kind: LaneKind,
    /// The task a task lane works.
    pub task_id: Option<TaskId>,
    /// The task's repositories, in the order it named them: one worktree
    /// each, the first one the working directory (Plan.md §5.5). Empty for a
    /// lead, and for a task that names none, which works in a plain
    /// directory.
    pub repositories: Vec<Repository>,
    /// The events this run consumes.
    pub events: Vec<EventId>,
    /// Decisions answered with approval among those events: the runner
    /// performs each stored call, through [`HubLink::begin_perform`], before
    /// the backend starts, and gives the session the outcome (Plan.md §9.4).
    pub perform: Vec<DecisionId>,
    /// Which backend to run on; one the machine reported as usable.
    pub backend: BackendKind,
    pub model: Option<String>,
    /// Whether to start the backend's session or resume it.
    pub native_session: NativeSession,
    /// Stable across runs until memory changes (Plan.md §8.5 items 1 to 3).
    pub appended_instructions: String,
    /// The per-run prompt (Plan.md §8.5 items 4 to 7).
    pub prompt: String,
    /// The system's tools this run may call.
    pub tools: Vec<ToolDefinition>,
    /// Remove every built-in tool of the backend (leads and dreaming, §5.1).
    pub remove_builtin_tools: bool,
    /// Direct commands the backend must refuse (Plan.md §9.2), as command
    /// prefixes ([`crate::backend::DENY_RULES`]).
    pub deny_rules: Vec<String>,
    pub wall_time_limit: Duration,
    pub max_turns: Option<u32>,
    /// The run's durable trace ID, for correlation with hub records.
    #[serde(default)]
    pub trace_id: String,
}

/// The hub's answer to [`HubLink::begin_run`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Begin {
    /// Execute this run, then call `begin_run` again.
    Run(Box<RunAssignment>),
    /// Nothing runnable is pending in the lane; release the lease.
    Idle,
}

/// The hub's answer to [`HubLink::begin_perform`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Perform {
    /// Perform this call now, exactly as given: it is the stored call, and
    /// its arguments still hash to what you approved. `retry` means an
    /// earlier attempt started and its outcome is unknown: check what it did
    /// before repeating an effect that is not safe to repeat.
    Call {
        tool: String,
        arguments: Json,
        retry: bool,
    },
    /// Perform this validated call on the remote selected by the hub.
    Repository {
        remote: String,
        action: crate::forge::RepositoryAction,
    },
    /// Nothing to perform: the recorded outcome of an earlier attempt, of a
    /// call the hub performed itself (a memory change, which has no effect
    /// on the machine), or why the call is refused. Give it to the session.
    Recorded { outcome: Json },
}

/// Why a run did not complete, as the runner reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunFailure {
    /// An ordinary failure: counts an attempt and backs the lane off (§4.9).
    Error { message: String },
    /// A wall-time or turn limit was hit (§4.5). Counts an attempt.
    LimitExceeded { message: String },
    /// The backend no longer has the native session. The hub closes the
    /// session and the events are retried at once in a fresh one, without
    /// counting an attempt (§8.6).
    SessionMissing,
    /// The account reached its usage limit, already reported with
    /// [`HubLink::account`]. No attempt is counted, and the events wait for
    /// a free account rather than a backoff (§5.3).
    UsageLimit,
    /// The user stopped the lane. The runner has checkpointed its work.
    Stopped,
}

/// Messages the hub pushes to a runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum HubMessage {
    /// Work may be available.
    Nudge,
    /// Stop a run.
    Cancel { lane_id: LaneId, epoch: i64 },
    /// Updated mode or rules for a lease in progress.
    Policy { lane_id: LaneId, policy: Json },
    /// Finish current runs, take no more.
    Drain,
    /// A copy of the hub's latest snapshot, for safekeeping.
    Snapshot { name: String, sha256: String },
}

/// Why a hub request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LinkError {
    /// The hub cannot be reached right now; retry later.
    #[error("hub unreachable")]
    Unreachable,
    /// The user interrupted the run under this lease.
    #[error("lane stopped by the user")]
    Stopped,
    /// The lease is no longer this runner's. Stop the run and write nothing more.
    #[error("stale lease on lane {lane_id}")]
    StaleLease { lane_id: LaneId },
    #[error("rejected: {0}")]
    Rejected(String),
    /// Crash injection stopped the hub mid-call. Only an in-process hub
    /// produces this; treat it as process death and propagate it.
    #[error(transparent)]
    Killed(#[from] Killed),
}

/// The runner's view of the hub.
#[async_trait]
pub trait HubLink: Send + Sync + fmt::Debug {
    /// Reports capabilities and backends when the runner starts.
    async fn hello(&self, report: &MachineReport) -> Result<(), LinkError>;

    /// Reports load and renews the leases held. Returns the renewed leases;
    /// one that is missing from the result has been lost.
    async fn heartbeat(
        &self,
        report: &MachineReport,
        leases: &[Lease],
    ) -> Result<Vec<Lease>, LinkError>;

    /// Asks placement for one lane (Plan.md §5.4).
    async fn request_work(&self) -> Result<Option<Lease>, LinkError>;

    /// Pending events in the leased lane, oldest first. Used for the
    /// debounce window (Plan.md §4.8); it changes nothing.
    async fn pending(&self, lease: &Lease) -> Result<Vec<Event>, LinkError>;

    /// Whether the leased task lane would be left with nothing to wake it
    /// if its run ended now: `finish_task` not called, and no pending
    /// decision, pending event or active timer of its own. The runner then
    /// takes one more turn then. It changes nothing.
    async fn left_unwoken(&self, lease: &Lease) -> Result<bool, LinkError>;

    /// Starts the next run in the lane: the hub reads pending events, applies
    /// the initiative check, routes to a session, opens the run record, and
    /// assembles context (Plan.md §4.3 steps 2 to 5.3).
    async fn begin_run(&self, lease: &Lease) -> Result<Begin, LinkError>;

    /// Checks for a user stop under the current lease.
    async fn check_running(&self, _lease: &Lease) -> Result<(), LinkError> {
        Ok(())
    }

    /// Associates user messages accepted by the backend with this run.
    async fn folded_in(
        &self,
        _lease: &Lease,
        _run: RunId,
        _events: &[EventId],
    ) -> Result<(), LinkError> {
        Err(LinkError::Rejected(
            "fold-in is unsupported by this link".to_owned(),
        ))
    }

    /// Records the backend's own session id once it is known.
    async fn native_session(
        &self,
        lease: &Lease,
        session_id: SessionId,
        native_session_id: &str,
    ) -> Result<(), LinkError>;

    /// Appends mirrored backend events in order (Plan.md §4.6).
    async fn append(&self, lease: &Lease, entries: &[JournalEntry]) -> Result<(), LinkError>;

    /// Calls one of the system's tools through the hub: gate, at-most-once
    /// record, execution (Plan.md §4.6, §9.1). `call_id` is the identifier
    /// the backend supplied, if it supplied one.
    async fn call_tool(
        &self,
        lease: &Lease,
        run_id: RunId,
        call_id: Option<&str>,
        tool: &str,
        arguments: &Json,
    ) -> Result<Json, ToolError>;

    /// Commits a finished run: events done, run closed (Plan.md §4.3 step
    /// 5.6). `final_text` is stored as the rationale. When a lead's run sent
    /// nothing and left no decision, the hub delivers it as the lead's
    /// message. The hub derives the outcome from what the run did.
    async fn complete(
        &self,
        lease: &Lease,
        run_id: RunId,
        final_text: Option<&str>,
    ) -> Result<RunOutcome, LinkError>;

    /// Reports that the account `backend` runs on reached its usage limit,
    /// resetting at `resets_at` if the backend said (Plan.md §5.3, §5.6
    /// `account`). `account_label` is the label the backend's probe
    /// reported, if any. Sent before failing the run with
    /// [`RunFailure::UsageLimit`].
    async fn account(
        &self,
        lease: &Lease,
        backend: BackendKind,
        account_label: Option<&str>,
        resets_at: Option<Timestamp>,
    ) -> Result<(), LinkError>;

    /// Records that the runner is about to perform the stored call of
    /// `decision`, answered with approval in this lane, and returns it once
    /// checked against its `arguments_hash` (invariant 7). The record makes
    /// the call take effect at most once across runs (invariant 5).
    async fn begin_perform(
        &self,
        lease: &Lease,
        run_id: RunId,
        decision: DecisionId,
    ) -> Result<Perform, LinkError>;

    /// Records what performing the stored call of `decision` did.
    async fn finish_perform(
        &self,
        lease: &Lease,
        run_id: RunId,
        decision: DecisionId,
        outcome: &Json,
    ) -> Result<(), LinkError>;

    /// Ends a run that did not complete (Plan.md §4.9).
    async fn fail(
        &self,
        lease: &Lease,
        run_id: RunId,
        failure: &RunFailure,
    ) -> Result<(), LinkError>;

    /// Gives the lane back.
    async fn release(&self, lease: &Lease) -> Result<(), LinkError>;

    /// The next message pushed by the hub, or `None` when the link closed.
    async fn next_message(&self) -> Option<HubMessage>;
}
