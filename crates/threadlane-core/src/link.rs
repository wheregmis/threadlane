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
use crate::ids::{EventId, LaneId, ProjectId, RunId, SessionId};
use crate::model::{BackendKind, Event, JournalEntry, LaneKind, Lease, MachineReport, RunOutcome};
use crate::runtime::Killed;
use crate::time::Timestamp;
use crate::tools::{ToolDefinition, ToolError};

/// Everything a runner needs to execute one run. Built by the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunAssignment {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub lane_kind: LaneKind,
    /// The events this run consumes.
    pub events: Vec<EventId>,
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
    /// Direct commands the backend must refuse (Plan.md §9.2).
    pub deny_rules: Vec<String>,
    pub wall_time_limit: Duration,
    pub max_turns: Option<u32>,
}

/// The hub's answer to [`HubLink::begin_run`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Begin {
    /// Execute this run, then call `begin_run` again.
    Run(Box<RunAssignment>),
    /// Nothing runnable is pending in the lane; release the lease.
    Idle,
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
    /// The account reached its usage limit. No attempt is counted (§5.3).
    UsageLimit {
        account_label: Option<String>,
        resets_at: Option<Timestamp>,
    },
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

    /// Starts the next run in the lane: the hub reads pending events, applies
    /// the initiative check, routes to a session, opens the run record, and
    /// assembles context (Plan.md §4.3 steps 2 to 5.3).
    async fn begin_run(&self, lease: &Lease) -> Result<Begin, LinkError>;

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
    /// 5.6). `final_text` is stored as the rationale and never delivered.
    /// The hub derives the outcome from what the run did.
    async fn complete(
        &self,
        lease: &Lease,
        run_id: RunId,
        final_text: Option<&str>,
    ) -> Result<RunOutcome, LinkError>;

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
