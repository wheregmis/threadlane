//! Durable state interfaces (Plan.md §3.2, §3.4).
//!
//! Each method is one domain operation and is atomic: when an operation spans
//! two stores (fire a timer and insert its event; commit a run and mark its
//! events done) the implementation does both in one transaction. Callers never
//! see a transaction. Every method that a run performs takes the run's
//! [`Lease`] and fails with [`StoreError::StaleLease`] if the lane has since
//! been granted to someone else (Plan.md §6.4, invariant 10).

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value as Json;

use crate::ids::{
    AgentId, BindingId, DecisionId, EventId, LaneId, MachineId, OutboxId, ProjectId, RunId,
    SessionId, StandingRuleId, TaskId, TimerId,
};
use crate::model::{
    Account, Agent, Answered, BackendKind, Binding, Decision, DecisionAnswer, Event, FailedRun,
    Inserted, JournalEntry, Lane, Lease, Machine, MachineReport, MachineStatus, MemoryAuthor,
    MemoryFile, Mode, NewAgent, NewBinding, NewDecision, NewEvent, NewMachine, NewOutboxMessage,
    NewProject, NewRepository, NewStandingRule, NewTask, NewTimer, OutboxMessage, Project,
    Repository, RetryPolicy, Run, RunOutcome, Session, SpaceSettings, StandingRule, StartRun, Task,
    TaskStatus, Timer, ToolCallBegin, ToolCallKey,
};
use crate::time::Timestamp;

/// A delivered lead message and its optional return to the space Lead.
#[derive(Debug)]
pub struct LeadReply {
    pub message: NewOutboxMessage,
    /// The event's source ID is assigned from the persisted outbox message.
    pub route: Option<NewEvent>,
}

/// Why a store operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The lane's lease epoch moved on; this writer no longer holds it.
    #[error("stale lease on lane {lane_id}: held epoch {held}, current epoch {current}")]
    StaleLease {
        lane_id: LaneId,
        held: i64,
        current: i64,
    },
    /// The user interrupted the run under this lease.
    #[error("lane stopped by the user")]
    Stopped,
    /// A referenced row does not exist.
    #[error("{kind} {id} not found")]
    NotFound { kind: &'static str, id: String },
    /// The operation would violate a uniqueness or state rule.
    #[error("conflict: {0}")]
    Conflict(String),
    /// The input was rejected before touching the database.
    #[error("invalid input: {0}")]
    Invalid(String),
    /// A stored value could not be decoded.
    #[error("corrupt row: {0}")]
    Corrupt(String),
    /// The database itself failed.
    #[error("database error: {0}")]
    Database(String),
}

impl StoreError {
    /// Builds a [`StoreError::NotFound`].
    pub fn not_found(kind: &'static str, id: impl ToString) -> Self {
        Self::NotFound {
            kind,
            id: id.to_string(),
        }
    }
}

/// Result alias for store operations.
pub type StoreResult<T> = Result<T, StoreError>;

/// Space settings, projects, repositories, agents, bindings (Plan.md §2.4).
#[async_trait]
pub trait OrgStore: Send + Sync {
    /// This hub's space settings.
    async fn space(&self) -> StoreResult<SpaceSettings>;
    /// Replaces this hub's space settings.
    async fn update_space(&self, settings: &SpaceSettings) -> StoreResult<()>;

    /// Creates a project together with its lead lane.
    async fn create_project(&self, project: &NewProject) -> StoreResult<Project>;
    async fn project(&self, id: ProjectId) -> StoreResult<Option<Project>>;
    async fn projects(&self) -> StoreResult<Vec<Project>>;
    /// The built-in General project, which every hub has from its first start.
    async fn general_project(&self) -> StoreResult<Project>;
    /// Pauses or resumes a project: a paused project's lanes are not granted.
    async fn set_project_paused(&self, id: ProjectId, paused: bool) -> StoreResult<()>;

    /// Archives an idle non-General project or restores it and returns its current state.
    async fn set_project_archived(&self, id: ProjectId, archived: bool) -> StoreResult<Project>;

    /// Registers a repository; its remote URL must be new to this space.
    async fn create_repository(&self, repository: &NewRepository) -> StoreResult<Repository>;
    async fn repositories(&self, project_id: ProjectId) -> StoreResult<Vec<Repository>>;
    /// The repository with this remote URL, used to route outside events.
    async fn repository_by_remote(&self, remote_url: &str) -> StoreResult<Option<Repository>>;

    async fn create_agent(&self, agent: &NewAgent) -> StoreResult<Agent>;
    async fn agent(&self, id: AgentId) -> StoreResult<Option<Agent>>;
    /// Every agent not deleted.
    async fn agents(&self) -> StoreResult<Vec<Agent>>;
    /// Every agent, deleted ones included, for naming them in history.
    async fn all_agents(&self) -> StoreResult<Vec<Agent>>;
    /// Stores an agent's name, role, instructions, backends and models. A
    /// name another agent has is a [`StoreError::Conflict`].
    async fn update_agent(&self, agent: &Agent) -> StoreResult<Agent>;
    /// Deletes an agent with its bindings and standing rules. Its row stays,
    /// renamed `<name> (deleted)`, so finished tasks still name it. Refused
    /// for a lead, and while any of its tasks has not ended.
    async fn delete_agent(&self, id: AgentId) -> StoreResult<()>;

    /// Binds an agent to a project. At most one binding per pair and one lead
    /// per project.
    async fn bind(&self, binding: &NewBinding) -> StoreResult<Binding>;
    /// Changes a binding's mode.
    async fn set_binding_mode(&self, id: BindingId, mode: Mode) -> StoreResult<Binding>;
    async fn bindings(&self, project_id: ProjectId) -> StoreResult<Vec<Binding>>;
    async fn lead_binding(&self, project_id: ProjectId) -> StoreResult<Option<Binding>>;
    /// Sets or clears a project's own ceiling, which the space's ceiling
    /// still caps (Plan.md §2.3 "Project ceiling").
    async fn set_project_ceiling(&self, id: ProjectId, ceiling: Option<Mode>) -> StoreResult<()>;
    /// Replaces a binding's overrides: a JSON object from a tier to the cell
    /// it is set to (Plan.md §2.3 "Overrides").
    async fn set_binding_overrides(&self, id: BindingId, overrides: &Json) -> StoreResult<Binding>;
    /// Changes how many of a binding's tasks may run at once; 0 means no
    /// limit.
    async fn set_binding_max_parallel(
        &self,
        id: BindingId,
        max_parallel: u32,
    ) -> StoreResult<Binding>;
}

/// Lanes, leases, and events (Plan.md §6).
#[async_trait]
pub trait InboxStore: Send + Sync {
    async fn lane(&self, id: LaneId) -> StoreResult<Option<Lane>>;

    /// Inserts a routed event, deduplicating on `(source, source_event_id)`.
    async fn insert_event(&self, event: &NewEvent) -> StoreResult<Inserted>;
    async fn event(&self, id: EventId) -> StoreResult<Option<Event>>;
    /// Pending events in a lane, oldest first.
    async fn pending_events(&self, lane_id: LaneId) -> StoreResult<Vec<Event>>;
    /// Marks pending events skipped, with a reason, under the lease.
    async fn skip_events(&self, lease: &Lease, events: &[EventId], reason: &str)
    -> StoreResult<()>;

    /// Lanes with pending events, no live lease, and no active backoff. A
    /// paused project's lead lane is left out; its task lanes are not, so
    /// that [`InboxStore::grant_lease`] records why they wait. A task lane
    /// with a pending decision is left out: its events wait for your answer.
    async fn ready_lanes(&self, now: Timestamp) -> StoreResult<Vec<LaneId>>;

    /// Grants the lane to a machine if no live lease exists, incrementing the
    /// epoch. Returns `None` if someone else holds it, or if the lane's task
    /// is not eligible (Plan.md §5.2): a dependency is not `done`, the project
    /// is paused, or the project's or the binding's parallel limit is reached.
    /// Nor is any lane, of either kind, granted when none of its agent's
    /// backends is usable on the machine with an account that is not cooling
    /// down (Plan.md §5.4 item 4); its task is then `blocked`.
    /// The task then carries the reason as `waiting_reason`, cleared on grant.
    /// A task that has ended is always eligible, so its events can be skipped.
    async fn grant_lease(
        &self,
        lane_id: LaneId,
        machine_id: MachineId,
        now: Timestamp,
        ttl: Duration,
    ) -> StoreResult<Option<Lease>>;
    /// Extends a lease the caller still holds.
    async fn renew_lease(&self, lease: &Lease, now: Timestamp, ttl: Duration)
    -> StoreResult<Lease>;
    /// Gives a lane back. Succeeds as a no-op if the lease is already gone.
    async fn release_lease(&self, lease: &Lease) -> StoreResult<()>;
    /// Releases every lease that expired at or before `now`. Their claimed
    /// events return to pending and their active runs are abandoned.
    async fn expire_leases(&self, now: Timestamp) -> StoreResult<Vec<LaneId>>;
    /// The cursor a pulling source keeps under `source` (Plan.md §6.6), if
    /// it has saved one.
    async fn source_cursor(&self, source: &str) -> StoreResult<Option<String>>;
    /// Saves a pulling source's cursor, replacing the one before.
    async fn set_source_cursor(
        &self,
        source: &str,
        cursor: &str,
        now: Timestamp,
    ) -> StoreResult<()>;
    /// Your messages in a lane, in any status, received before `before`
    /// (all of them when `None`), newest first, at most `limit`. The
    /// client's conversation history (Plan.md §11.1).
    async fn user_messages(
        &self,
        lane_id: LaneId,
        before: Option<Timestamp>,
        limit: u32,
    ) -> StoreResult<Vec<Event>>;
    /// Events of `kind` from `source` in a lane, received before `before`
    /// (all of them when `None`), newest first, at most `limit`.
    async fn lane_events_from(
        &self,
        lane_id: LaneId,
        kind: crate::model::EventKind,
        source: &str,
        before: Option<Timestamp>,
        limit: u32,
    ) -> StoreResult<Vec<Event>>;
    /// Events skipped for either reason, in one project or in the whole space
    /// when `None`: the newest `limit`, newest first, and how many there
    /// are in all.
    async fn skipped_events(
        &self,
        project_id: Option<ProjectId>,
        reasons: [&str; 2],
        limit: u32,
    ) -> StoreResult<(Vec<Event>, u64)>;
}

/// Sessions, runs, and calls to the system's tools (Plan.md §4).
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn session(&self, id: SessionId) -> StoreResult<Option<Session>>;
    /// The lane's most recently active open session, if any.
    async fn latest_open_session(&self, lane_id: LaneId) -> StoreResult<Option<Session>>;
    /// Records the backend's own session id for later native resume.
    async fn set_native_session(
        &self,
        lease: &Lease,
        session_id: SessionId,
        native_session_id: &str,
    ) -> StoreResult<()>;
    async fn close_session(&self, session_id: SessionId) -> StoreResult<()>;

    /// Opens a run record and marks its events claimed (Plan.md §4.3 step 5.1).
    async fn start_run(&self, lease: &Lease, start: &StartRun) -> StoreResult<Run>;
    async fn run(&self, id: RunId) -> StoreResult<Option<Run>>;
    /// The events a run consumed or is consuming, oldest first, whatever
    /// their current status.
    async fn run_events(&self, run_id: RunId) -> StoreResult<Vec<Event>>;
    /// Closes a run with its outcome and marks its events done, in one
    /// transaction (Plan.md §4.3 step 5.6). `reply`, if given, is queued in
    /// the same transaction: a lead's final text delivered as its message.
    async fn commit_run(
        &self,
        lease: &Lease,
        run_id: RunId,
        outcome: RunOutcome,
        rationale: Option<&str>,
        reply: Option<&LeadReply>,
        ended_at: Timestamp,
    ) -> StoreResult<()>;
    /// Ends a run as failed, in one transaction (Plan.md §4.9): its events
    /// return to pending, with one more attempt if `retry.count_attempt`, or
    /// die at the limit, and the lane backs off until `retry.backoff_until`.
    ///
    /// So that you hear about work that died: when events die in a task lane,
    /// the task fails and its `task_result` reaches the lead lane (see
    /// [`TaskStore`]); when they die in the lead lane itself, where no agent
    /// is left to tell you, a `system_notice` is queued in the outbox
    /// instead.
    async fn fail_run(
        &self,
        lease: &Lease,
        run_id: RunId,
        error: &str,
        retry: RetryPolicy,
        ended_at: Timestamp,
    ) -> StoreResult<FailedRun>;

    /// Records a call to one of the system's tools before it is performed.
    async fn begin_tool_call(
        &self,
        lease: &Lease,
        key: &ToolCallKey,
        tool: &str,
        arguments: &Json,
        safe_to_repeat: bool,
        now: Timestamp,
    ) -> StoreResult<ToolCallBegin>;
    /// Records the result of a call begun with [`SessionStore::begin_tool_call`].
    async fn finish_tool_call(
        &self,
        lease: &Lease,
        key: &ToolCallKey,
        result: &Json,
        now: Timestamp,
    ) -> StoreResult<()>;

    /// Appends mirrored backend events to an active run, in order (Plan.md
    /// §4.6). Entries already stored are skipped, so a retried append is
    /// harmless; an entry that would leave a gap in the sequence is a
    /// [`StoreError::Conflict`].
    async fn append_journal(&self, lease: &Lease, entries: &[JournalEntry]) -> StoreResult<()>;
    /// A run's mirrored events, in order.
    async fn journal(&self, run_id: RunId) -> StoreResult<Vec<JournalEntry>>;
    /// Every run in a lane, in any status, oldest first.
    async fn lane_runs(&self, lane_id: LaneId) -> StoreResult<Vec<Run>>;
}

/// Follow-ups and routines (Plan.md §7).
#[async_trait]
pub trait TimerStore: Send + Sync {
    /// Creates a timer; a repeated `created_by_call` returns the original.
    async fn create_timer(&self, timer: &NewTimer) -> StoreResult<Timer>;
    async fn timer(&self, id: TimerId) -> StoreResult<Option<Timer>>;
    /// Active timers due at or before `now`, earliest first.
    async fn due_timers(&self, now: Timestamp) -> StoreResult<Vec<Timer>>;
    /// Fires one occurrence in one transaction: a follow-up is marked done, a
    /// schedule moves to `next_due_at`, and a `timer_fired` event is inserted.
    /// Firing the same occurrence twice inserts nothing the second time. A
    /// decision's expiry timer instead expires its decision, if still
    /// pending, as declined with "no response", and inserts its answer event.
    async fn fire(
        &self,
        id: TimerId,
        occurrence: Timestamp,
        next_due_at: Option<Timestamp>,
        now: Timestamp,
    ) -> StoreResult<Inserted>;
    async fn cancel_timer(&self, id: TimerId) -> StoreResult<()>;
    /// A project's timers that have not ended, active or paused, oldest
    /// first, but not the system's own (a decision's expiry). Their number
    /// is what the cap on live timers counts.
    async fn project_timers(&self, project_id: ProjectId) -> StoreResult<Vec<Timer>>;
    /// Pauses an active timer; pausing a paused one changes nothing. A timer
    /// that has ended is a [`StoreError::Conflict`].
    async fn pause_timer(&self, id: TimerId) -> StoreResult<Timer>;
    /// Makes a paused timer active again, due at `due_at` if given;
    /// resuming an active one changes nothing. A timer that has ended is a
    /// [`StoreError::Conflict`].
    async fn resume_timer(&self, id: TimerId, due_at: Option<Timestamp>) -> StoreResult<Timer>;
}

/// Tasks and checkpoints (Plan.md §5.2, §5.5).
///
/// The run cycle moves a task through its lifecycle inside the lane's own
/// transactions: [`SessionStore::start_run`] makes it `running`,
/// [`SessionStore::commit_run`] `done` (if [`TaskStore::finish_task`] was
/// called) or `sleeping`, [`SessionStore::fail_run`] `queued` again or
/// `failed`, and a lost lease `queued`. Ending a task, by finishing,
/// failing, or cancelling it, inserts one `task_result` event (source
/// `task`, id `result:<task id>`) into the project's lead lane, requested
/// when the task's origin is ([`crate::model::TaskOrigin::is_requested`]).
///
/// Writes a run makes take the lease of the lane the run works in, which for
/// [`TaskStore::create_task`] and [`TaskStore::cancel_task`] is not the
/// task's own lane.
#[async_trait]
pub trait TaskStore: Send + Sync {
    /// Creates a task and its lane in one transaction; a queued task also
    /// gets its `task_start` event. A repeated `created_by_call` returns the
    /// original task. Dependencies and the parent must be tasks of the same
    /// project. `lease` is the creating run's, `None` when no run creates it.
    async fn create_task(&self, lease: Option<&Lease>, task: &NewTask) -> StoreResult<Task>;
    async fn task(&self, id: TaskId) -> StoreResult<Option<Task>>;

    /// When the hub recorded the result of a completed task, if it has that event.
    async fn task_completed_at(&self, id: TaskId) -> StoreResult<Option<Timestamp>>;
    /// Whether the lane's running task would be left with nothing to wake
    /// it if its run ended now: no result recorded with `finish_task`, no
    /// pending decision, no pending event, no active timer of its own and no
    /// published branch that GitHub's events reach.
    /// `false` for a lead lane.
    async fn left_unwoken(&self, lane_id: LaneId) -> StoreResult<bool>;
    async fn tasks(&self, project_id: ProjectId) -> StoreResult<Vec<Task>>;
    /// Changes what a task that has not ended is about. `None` keeps a field.
    async fn update_task(
        &self,
        lease: &Lease,
        id: TaskId,
        title: Option<&str>,
        goal: Option<&str>,
        priority: Option<i64>,
    ) -> StoreResult<Task>;
    /// Cancels a task that has not ended and reports it to the lead lane.
    /// Cancelling a cancelled task changes nothing. Pending events in its
    /// lane are skipped by the next run cycle.
    async fn cancel_task(
        &self,
        lease: Option<&Lease>,
        id: TaskId,
        now: Timestamp,
    ) -> StoreResult<Task>;
    /// Records the result of the running task whose lane `lease` holds. The
    /// task becomes `done`, and its result is reported, when the run commits.
    /// Recording again replaces the result. Refused with
    /// [`StoreError::Invalid`] while the task has no progress file
    /// ([`crate::memory::progress_path`]).
    async fn finish_task(&self, lease: &Lease, result: &Json) -> StoreResult<Task>;
    /// Counts one refusal of `finish_task` for merge conflicts on the
    /// running task whose lane `lease` holds, unless `limit` were already
    /// counted (docs/merge-help.md decision 5). Returns whether it counted
    /// one: `false` means finish anyway.
    async fn count_merge_refusal(&self, lease: &Lease, limit: i64) -> StoreResult<bool>;
    /// Records that a project lead created task `id` for the space lead's
    /// ask, event `ask` (docs/one-place-to-talk.md decision 2).
    async fn set_space_lead_ask(&self, id: TaskId, ask: EventId) -> StoreResult<()>;
    /// The space lead's ask task `id` was created for, if any.
    async fn space_lead_ask(&self, id: TaskId) -> StoreResult<Option<EventId>>;
    /// Moves a task along its lifecycle, refusing a move Plan.md §5.2 does
    /// not allow ([`TaskStatus::can_become`]) with [`StoreError::Conflict`].
    /// `proposed` to `queued` inserts the start event.
    async fn set_task_status(
        &self,
        id: TaskId,
        status: TaskStatus,
        waiting_reason: Option<&str>,
    ) -> StoreResult<()>;
}

/// Joined machines and model accounts (Plan.md §5.3).
#[async_trait]
pub trait MachineStore: Send + Sync {
    async fn register_machine(&self, machine: &NewMachine) -> StoreResult<Machine>;
    async fn machine(&self, id: MachineId) -> StoreResult<Option<Machine>>;
    async fn machine_by_endpoint(&self, endpoint_id: &str) -> StoreResult<Option<Machine>>;
    async fn machines(&self) -> StoreResult<Vec<Machine>>;
    /// Records a `hello` or `heartbeat`.
    async fn report(
        &self,
        id: MachineId,
        report: &MachineReport,
        now: Timestamp,
    ) -> StoreResult<()>;
    async fn set_machine_status(&self, id: MachineId, status: MachineStatus) -> StoreResult<()>;

    /// The account with this label, created if new.
    async fn ensure_account(&self, label: &str, backend: BackendKind) -> StoreResult<Account>;
    async fn accounts(&self) -> StoreResult<Vec<Account>>;
    /// Stops placing work on an account until `until` (Plan.md §5.3).
    async fn set_cooldown(&self, label: &str, until: Option<Timestamp>) -> StoreResult<()>;
    /// What a run reports when its account reaches its usage limit, under
    /// the run's lease: the account, created if new, cools down until
    /// `until`, or later if it already was.
    async fn cool_down(
        &self,
        lease: &Lease,
        label: &str,
        backend: BackendKind,
        until: Timestamp,
    ) -> StoreResult<Account>;
}

/// Versioned memory files (Plan.md §8.3). Scope is the caller's: the hub
/// maps every path an agent or you name to a stored one first (invariant 12).
///
/// Every change is atomic and versioned with its run, under the run's lease,
/// or, for a change you make yourself, with no run (`written_by_run` is
/// `None`).
/// It is checked against the rules of [`crate::memory::check`], and rewrites
/// the [`crate::memory::index_for`] index of every file it touches in the
/// same transaction. A change repeated with the same
/// [`MemoryAuthor::call_key`] writes nothing and returns what the first one
/// wrote. A refused change is [`StoreError::Invalid`] with a message for the
/// agent; a missing file is [`StoreError::NotFound`].
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// The current version of a file, unless it is absent or deleted.
    async fn read(&self, path: &str) -> StoreResult<Option<MemoryFile>>;
    /// Creates a file, or replaces its content, and returns the new version.
    async fn write(&self, by: &MemoryAuthor, path: &str, content: &str) -> StoreResult<MemoryFile>;
    /// Replaces the one occurrence of `old` in a file with `new`. Refused
    /// when `old` is empty, absent, or occurs more than once.
    async fn edit(
        &self,
        by: &MemoryAuthor,
        path: &str,
        old: &str,
        new: &str,
    ) -> StoreResult<MemoryFile>;
    /// Records a deletion as a new version, and returns it.
    async fn delete(&self, by: &MemoryAuthor, path: &str) -> StoreResult<MemoryFile>;
    /// Moves a file to a path where none is: a deletion at `from` and a new
    /// version at `to`, so each path keeps its history. Returns the version
    /// at `to`.
    async fn rename(&self, by: &MemoryAuthor, from: &str, to: &str) -> StoreResult<MemoryFile>;
    /// Your own write (Plan.md §8.3: you own the space, so instructions
    /// included): [`MemoryStore::write`] with no run, lease or call key,
    /// under the same rules and with the same indexes.
    async fn write_by_owner(
        &self,
        path: &str,
        content: &str,
        at: Timestamp,
    ) -> StoreResult<MemoryFile>;
    /// Your own deletion: [`MemoryStore::delete`] with no run, lease or
    /// call key.
    async fn delete_by_owner(&self, path: &str, at: Timestamp) -> StoreResult<MemoryFile>;
    /// Current, non-deleted files whose path starts with `prefix`, by path.
    async fn list(&self, prefix: &str) -> StoreResult<Vec<MemoryFile>>;
    /// Every version of a file, oldest first.
    async fn history(&self, path: &str) -> StoreResult<Vec<MemoryFile>>;
}

/// Paused tool calls shown to you (Plan.md §9.4).
///
/// While a decision is pending in a task lane, the lane is not granted
/// (Plan.md §9.4 "other events wait"), and its task is `waiting_on_you`
/// once the run that asked has ended. A `user_message` arriving in a task
/// lane declines every pending decision there, with the message as the note.
#[async_trait]
pub trait DecisionStore: Send + Sync {
    /// Opens a decision and queues its card on the client channel, in one
    /// transaction (Plan.md §10.1, `kind` `decision`). A repeated
    /// `(run_id, tool_call_id)` returns the original and queues nothing.
    /// `lease` is the asking run's, `None` when no run asks. A decision with
    /// `expires_at` gets a system timer, in the same transaction, that
    /// expires it then ([`TimerStore::fire`]).
    async fn open_decision(
        &self,
        lease: Option<&Lease>,
        decision: &NewDecision,
    ) -> StoreResult<Decision>;
    async fn decision(&self, id: DecisionId) -> StoreResult<Option<Decision>>;
    async fn pending_decisions(&self, lane_id: LaneId) -> StoreResult<Vec<Decision>>;
    /// Every pending decision in the space, oldest first.
    async fn open_decisions(&self) -> StoreResult<Vec<Decision>>;
    /// Every decision asked in a lane, in any status, oldest first.
    async fn lane_decisions(&self, lane_id: LaneId) -> StoreResult<Vec<Decision>>;
    /// Records your answer and inserts the answer event into the decision's
    /// lane, in one transaction. A second answer is ignored.
    async fn answer(&self, id: DecisionId, answer: &DecisionAnswer) -> StoreResult<Answered>;
    /// Records, under the lease of the decision's lane, that its approved
    /// call is about to be performed. [`ToolCallBegin::Finished`] carries
    /// the recorded outcome; [`ToolCallBegin::OutcomeUnknown`] means an
    /// earlier attempt began and recorded none.
    async fn begin_perform(
        &self,
        lease: &Lease,
        id: DecisionId,
        now: Timestamp,
    ) -> StoreResult<ToolCallBegin>;
    /// Records the outcome of performing the call, or of refusing to, under
    /// the lease of the decision's lane. An outcome already recorded is kept.
    async fn finish_perform(
        &self,
        lease: &Lease,
        id: DecisionId,
        outcome: &Json,
    ) -> StoreResult<()>;
    /// [`DecisionStore::answer`], and with `rule`, in the same transaction,
    /// the standing rule your "always" made, created from the decision. A
    /// second answer is ignored and creates no rule.
    async fn answer_with_rule(
        &self,
        id: DecisionId,
        answer: &DecisionAnswer,
        rule: Option<&NewStandingRule>,
    ) -> StoreResult<Answered>;
    /// Records a call that standing rule `rule` allowed as a decision the rule
    /// answered with its first option, counts the use, and inserts the answer
    /// event, in one transaction and with no card. A repeated
    /// `(run_id, tool_call_id)` returns the original and counts nothing.
    async fn allow_by_rule(
        &self,
        lease: &Lease,
        decision: &NewDecision,
        rule: StandingRuleId,
    ) -> StoreResult<Decision>;
    /// The standing rules of a project, or of every project when `None`,
    /// revoked ones included, oldest first.
    async fn standing_rules(&self, project_id: Option<ProjectId>)
    -> StoreResult<Vec<StandingRule>>;
    /// Revokes a standing rule; revoking it again changes nothing.
    async fn revoke_standing_rule(
        &self,
        id: StandingRuleId,
        now: Timestamp,
    ) -> StoreResult<StandingRule>;
}

/// Outgoing messages (Plan.md §10).
#[async_trait]
pub trait OutboxStore: Send + Sync {
    /// Queues a message. A repeated `tool_call_id` returns the original row
    /// and inserts nothing. If the repeat comes from a different run and the
    /// run that first queued the row never committed, the row is handed to
    /// the new run, so the run that commits is recorded as the sender.
    async fn enqueue(&self, message: &NewOutboxMessage) -> StoreResult<OutboxMessage>;
    async fn message(&self, id: OutboxId) -> StoreResult<Option<OutboxMessage>>;
    /// Pending messages deliverable at `now`, oldest first.
    async fn deliverable(&self, now: Timestamp, limit: u32) -> StoreResult<Vec<OutboxMessage>>;
    /// Marks a pending message sent and gives it the next `delivery_seq`.
    async fn mark_sent(
        &self,
        id: OutboxId,
        channel_message_id: Option<&str>,
        now: Timestamp,
    ) -> StoreResult<()>;
    /// Most recent sent messages in a project, newest first (Plan.md §8.5).
    async fn recent_messages(
        &self,
        project_id: ProjectId,
        limit: u32,
    ) -> StoreResult<Vec<OutboxMessage>>;
    /// How many ordinary messages a run has queued, in any status. A run
    /// with at least one ended as `sent` (Plan.md §4.5).
    async fn count_for_run(&self, run_id: RunId) -> StoreResult<u32>;
    /// Queues the system notice that no model account is free anywhere in
    /// the space (`blocked`), or that one is again, unless the last such
    /// notice already said so (Plan.md §5.3). Returns the queued message, or
    /// `None` when nothing changed. One notice per change, however often it
    /// is called and by whom.
    async fn account_notice(
        &self,
        blocked: bool,
        body: &Json,
        now: Timestamp,
    ) -> StoreResult<Option<OutboxMessage>>;
    /// Sent messages on a channel with `delivery_seq` greater than `after`,
    /// in delivery order. This is the client's feed (Plan.md §11.2).
    async fn delivered_after(
        &self,
        channel: &str,
        after: i64,
        limit: u32,
    ) -> StoreResult<Vec<OutboxMessage>>;
    /// Sent messages on a channel, of one project or of any when `None`,
    /// sent before `before` (all of them when `None`), newest first, at
    /// most `limit`. The client's history (Plan.md §11.1).
    async fn sent_before(
        &self,
        channel: &str,
        project_id: Option<ProjectId>,
        before: Option<Timestamp>,
        limit: u32,
    ) -> StoreResult<Vec<OutboxMessage>>;
}
