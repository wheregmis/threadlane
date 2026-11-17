//! Records and enumerations that mirror the hub's tables (Plan.md §2.4, §5, §6.2,
//! §7.2, §8, §9.4, §10.1). No record has a space column: a hub serves one space.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::ids::{
    AccountId, AgentId, BindingId, CheckpointId, DecisionId, EventId, LaneId, MachineId,
    MemoryFileId, OutboxId, ProjectId, RepositoryId, RunId, SessionId, TaskId, TimerId,
};
use crate::time::Timestamp;

/// A stored string did not name any variant of the expected enumeration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} value {value:?}")]
pub struct ParseEnumError {
    kind: &'static str,
    value: String,
}

macro_rules! str_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The value stored in the database and used on the wire.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
        }

        impl FromStr for $name {
            type Err = ParseEnumError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($text => Ok(Self::$variant),)+
                    _ => Err(ParseEnumError {
                        kind: stringify!($name),
                        value: value.to_owned(),
                    }),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum! {
    /// How much an agent may start and do without asking (Plan.md §2.3).
    ///
    /// Ordered from least to most independent, so `min` gives the stricter mode.
    #[derive(PartialOrd, Ord)]
    pub enum Mode {
        Off => "off",
        OnRequest => "on_request",
        Assisted => "assisted",
        Autonomous => "autonomous",
    }
}

impl Mode {
    /// The stricter of two modes.
    #[must_use]
    pub fn stricter(self, other: Self) -> Self {
        self.min(other)
    }

    /// One mode lower, to a minimum of On request (Plan.md §9.1, tainted runs).
    #[must_use]
    pub fn stepped_down(self) -> Self {
        match self {
            Self::Off => Self::Off,
            Self::OnRequest | Self::Assisted => Self::OnRequest,
            Self::Autonomous => Self::Assisted,
        }
    }
}

str_enum! {
    /// Which lane an event lands in (Plan.md §4.1).
    pub enum LaneKind {
        Lead => "lead",
        Task => "task",
    }
}

str_enum! {
    /// What kind of thing happened (Plan.md §6.2).
    pub enum EventKind {
        UserMessage => "user_message",
        OutsideEvent => "outside_event",
        TimerFired => "timer_fired",
        DecisionAnswer => "decision_answer",
        TaskStart => "task_start",
        TaskResult => "task_result",
        JobResult => "job_result",
        DeliveryFailure => "delivery_failure",
        System => "system",
    }
}

str_enum! {
    /// Where content came from, for screening and rendering (Plan.md §6.2, §8.5).
    pub enum Trust {
        User => "user",
        System => "system",
        External => "external",
    }
}

str_enum! {
    /// Event lifecycle (Plan.md §6.2).
    pub enum EventStatus {
        Pending => "pending",
        Claimed => "claimed",
        Done => "done",
        Skipped => "skipped",
        Dead => "dead",
    }
}

str_enum! {
    /// Whether a session can still be resumed.
    pub enum SessionStatus {
        Open => "open",
        Closed => "closed",
    }
}

str_enum! {
    /// Run lifecycle. A run that is not `Active` has ended.
    pub enum RunStatus {
        Active => "active",
        Committed => "committed",
        Failed => "failed",
        Abandoned => "abandoned",
    }
}

str_enum! {
    /// How a committed run ended (Plan.md §4.5).
    pub enum RunOutcome {
        Sent => "sent",
        Silent => "silent",
        Deferred => "deferred",
    }
}

str_enum! {
    /// Progress of one call to one of the system's tools (Plan.md §4.6).
    pub enum ToolCallStatus {
        Started => "started",
        Finished => "finished",
    }
}

str_enum! {
    /// Where a task came from; drives the initiative rule (Plan.md §5.2).
    pub enum TaskOrigin {
        User => "user",
        UserRoutine => "user_routine",
        Event => "event",
        Agent => "agent",
    }
}

impl TaskOrigin {
    /// Whether work of this origin counts as requested by you (initiative
    /// rule, docs/milestone-3.md §1 item 2).
    #[must_use]
    pub const fn is_requested(self) -> bool {
        matches!(self, Self::User | Self::UserRoutine)
    }
}

str_enum! {
    /// Task lifecycle (Plan.md §5.2).
    pub enum TaskStatus {
        Proposed => "proposed",
        Queued => "queued",
        Running => "running",
        WaitingOnYou => "waiting_on_you",
        Sleeping => "sleeping",
        Done => "done",
        Failed => "failed",
        Cancelled => "cancelled",
    }
}

impl TaskStatus {
    /// Whether the task has ended for good.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// Whether the lifecycle of Plan.md §5.2 allows moving from `self` to
    /// `next`. Any task that has not ended can be cancelled.
    #[must_use]
    pub const fn can_become(self, next: Self) -> bool {
        use TaskStatus::{
            Cancelled, Done, Failed, Proposed, Queued, Running, Sleeping, WaitingOnYou,
        };
        match (self, next) {
            (from, Cancelled) => !from.is_terminal(),
            (Proposed, Queued)
            | (Queued, Running)
            | (Running, WaitingOnYou | Sleeping | Queued | Done | Failed)
            | (WaitingOnYou | Sleeping, Queued) => true,
            _ => false,
        }
    }
}

str_enum! {
    /// The command-line agent that executes runs (Plan.md §5.7).
    pub enum BackendKind {
        Claude => "claude",
        Codex => "codex",
    }
}

str_enum! {
    /// Whether a machine receives memory and unassigned work (Plan.md §5.3).
    pub enum MachineTrust {
        Trusted => "trusted",
        Guest => "guest",
    }
}

str_enum! {
    /// Machine membership state (Plan.md §5.3).
    pub enum MachineStatus {
        Online => "online",
        Offline => "offline",
        Draining => "draining",
        Revoked => "revoked",
    }
}

str_enum! {
    /// Follow-up or routine (Plan.md §7.1).
    pub enum TimerCategory {
        FollowUp => "follow_up",
        Schedule => "schedule",
    }
}

str_enum! {
    /// Timer lifecycle (Plan.md §7.2).
    pub enum TimerStatus {
        Active => "active",
        Done => "done",
        Cancelled => "cancelled",
        Paused => "paused",
    }
}

str_enum! {
    /// Who created a timer; sets `requested` on the event it fires (Plan.md §7.2).
    pub enum TimerCreator {
        User => "user",
        Agent => "agent",
        System => "system",
    }
}

str_enum! {
    /// A day of the week, Monday first, for [`Rule::Weekdays`].
    pub enum Weekday {
        Mon => "mon",
        Tue => "tue",
        Wed => "wed",
        Thu => "thu",
        Fri => "fri",
        Sat => "sat",
        Sun => "sun",
    }
}

str_enum! {
    /// The three uses of one decision mechanism (Plan.md §9.4).
    pub enum DecisionKind {
        Approval => "approval",
        Input => "input",
        Proposal => "proposal",
    }
}

str_enum! {
    /// Decision lifecycle (Plan.md §9.4).
    pub enum DecisionStatus {
        Pending => "pending",
        Answered => "answered",
        Expired => "expired",
        Cancelled => "cancelled",
    }
}

str_enum! {
    /// An ordinary message or a decision card (Plan.md §10.1).
    pub enum OutboxKind {
        Message => "message",
        Decision => "decision",
        SystemNotice => "system_notice",
    }
}

str_enum! {
    /// Delivery state (Plan.md §10.1).
    pub enum OutboxStatus {
        Pending => "pending",
        Sent => "sent",
        Suppressed => "suppressed",
        Failed => "failed",
        Dead => "dead",
    }
}

str_enum! {
    /// How urgently a message should reach you (Plan.md §10.1).
    pub enum Urgency {
        Low => "low",
        Normal => "normal",
        High => "high",
    }
}

/// Quiet hours in the space's local time, as minutes after midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietHours {
    /// Start, in minutes after local midnight.
    pub start_minute: u16,
    /// End, in minutes after local midnight; may be before `start_minute`.
    pub end_minute: u16,
    /// IANA time zone name, for example `Europe/Berlin`.
    pub time_zone: String,
}

/// The single row of space settings (Plan.md §2.4 `space`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceSettings {
    /// Shown in the client, for example `Work`.
    pub name: String,
    /// Default ceiling for this space's projects.
    pub mode_ceiling: Mode,
    /// Notification quiet hours, if any.
    pub quiet_hours: Option<QuietHours>,
    /// Channel names messages are delivered on, in order of preference.
    pub channels: Vec<String>,
}

impl Default for SpaceSettings {
    fn default() -> Self {
        Self {
            name: "Space".to_owned(),
            mode_ceiling: Mode::OnRequest,
            quiet_hours: None,
            channels: vec!["client".to_owned()],
        }
    }
}

/// A project (Plan.md §2.4 `projects`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub description: String,
    /// The built-in project for requests not tied to a repository.
    pub is_general: bool,
    /// Stricter ceiling than the space's, if set.
    pub mode_ceiling: Option<Mode>,
    /// How many of the project's tasks may run at once; 0 means no limit.
    /// Projects created through the client carry 0 (`NewProject::default`),
    /// so 0 must not block them.
    pub max_parallel_tasks: u32,
    pub paused: bool,
    /// The project's lead lane, created with the project.
    pub lead_lane_id: LaneId,
}

/// Input for [`crate::store::OrgStore::create_project`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewProject {
    pub name: String,
    pub description: String,
    pub mode_ceiling: Option<Mode>,
    pub max_parallel_tasks: u32,
}

/// A repository registered to exactly one project (Plan.md §2.4 `repositories`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub id: RepositoryId,
    pub project_id: ProjectId,
    pub name: String,
    /// Unique within the space.
    pub remote_url: String,
    pub default_branch: String,
    /// Pushing to these is tier 3.
    pub protected_branches: Vec<String>,
}

/// Input for [`crate::store::OrgStore::create_repository`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRepository {
    pub project_id: ProjectId,
    pub name: String,
    pub remote_url: String,
    pub default_branch: String,
    pub protected_branches: Vec<String>,
}

/// A configured role (Plan.md §2.4 `agents`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub role: String,
    pub instructions: String,
    /// Backends it may use, in order of preference. Empty means either.
    pub backends: Vec<BackendKind>,
    /// Model per backend; absent means the backend's default.
    pub models: Json,
    /// The system's tools it is given and the built-in tools removed.
    pub tools: Json,
    /// Capabilities a machine must have to run it.
    pub requirements: Json,
}

/// Input for [`crate::store::OrgStore::create_agent`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewAgent {
    pub name: String,
    pub role: String,
    pub instructions: String,
    pub backends: Vec<BackendKind>,
    pub models: Json,
    pub tools: Json,
    pub requirements: Json,
}

impl NewAgent {
    /// An agent with a name and role and every other field at its default.
    #[must_use]
    pub fn named(name: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            role: role.into(),
            instructions: String::new(),
            backends: Vec::new(),
            models: Json::Object(serde_json::Map::new()),
            tools: Json::Object(serde_json::Map::new()),
            requirements: Json::Object(serde_json::Map::new()),
        }
    }
}

/// An agent's settings in one project (Plan.md §2.4 `bindings`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub id: BindingId,
    pub project_id: ProjectId,
    pub agent_id: AgentId,
    pub mode: Mode,
    pub is_lead: bool,
    pub models: Json,
    /// How many of this agent's tasks in the project may run at once; 0
    /// means no limit, as for [`Project::max_parallel_tasks`].
    pub max_parallel: u32,
    pub overrides: Json,
}

/// Input for [`crate::store::OrgStore::bind`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewBinding {
    pub project_id: ProjectId,
    pub agent_id: AgentId,
    pub mode: Mode,
    pub is_lead: bool,
    pub max_parallel: u32,
}

/// One slice of the inbox (Plan.md §6.2 `lanes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub id: LaneId,
    pub project_id: ProjectId,
    pub kind: LaneKind,
    pub task_id: Option<TaskId>,
    pub lease_owner: Option<MachineId>,
    pub lease_expires_at: Option<Timestamp>,
    /// Increments on every grant; the fencing token.
    pub lease_epoch: i64,
    pub backoff_until: Option<Timestamp>,
    pub last_machine_id: Option<MachineId>,
}

/// Proof of holding a lane. Every write a run makes carries it (Plan.md §6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Lease {
    pub lane_id: LaneId,
    pub machine_id: MachineId,
    pub epoch: i64,
    pub expires_at: Timestamp,
}

/// Input for [`crate::store::InboxStore::insert_event`]: an already routed event.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub project_id: ProjectId,
    pub lane_id: LaneId,
    /// Which adapter produced it.
    pub source: String,
    /// Identifier from the source; unique with `source`.
    pub source_event_id: String,
    pub kind: EventKind,
    pub payload: Json,
    pub trust: Trust,
    /// Whether this counts as work you requested (Plan.md §2.3).
    pub requested: bool,
    pub session_ref: Option<SessionId>,
    pub decision_ref: Option<DecisionId>,
    pub reply_to_ref: Option<OutboxId>,
    pub received_at: Timestamp,
}

/// An inbox event (Plan.md §6.2 `events`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub project_id: ProjectId,
    pub lane_id: LaneId,
    pub source: String,
    pub source_event_id: String,
    pub kind: EventKind,
    pub payload: Json,
    pub trust: Trust,
    pub requested: bool,
    pub session_ref: Option<SessionId>,
    pub decision_ref: Option<DecisionId>,
    pub reply_to_ref: Option<OutboxId>,
    pub status: EventStatus,
    pub claimed_by_run: Option<RunId>,
    pub attempts: u32,
    pub received_at: Timestamp,
    /// Why it was skipped or died, if it was.
    pub status_reason: Option<String>,
}

/// Result of inserting an event. A duplicate is a successful no-op (Plan.md §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inserted {
    /// A new event was written.
    New(EventId),
    /// An event with the same source and source id already existed.
    Duplicate(EventId),
}

impl Inserted {
    /// The event's id either way.
    #[must_use]
    pub fn id(self) -> EventId {
        match self {
            Self::New(id) | Self::Duplicate(id) => id,
        }
    }
}

/// A session: the transcript runs work inside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub lane_id: LaneId,
    pub agent_id: Option<AgentId>,
    pub backend: Option<BackendKind>,
    /// The backend's own session id, for native resume.
    pub native_session_id: Option<String>,
    pub machine_id: Option<MachineId>,
    pub status: SessionStatus,
    pub created_at: Timestamp,
    pub last_active_at: Timestamp,
}

/// Which session a run should work in (Plan.md §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionChoice {
    /// Start a fresh session.
    New,
    /// Resume an existing open session in the same lane.
    Resume(SessionId),
}

/// Input for [`crate::store::SessionStore::start_run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRun {
    pub session: SessionChoice,
    pub agent_id: Option<AgentId>,
    pub backend: Option<BackendKind>,
    /// With [`SessionChoice::New`]: the backend session id chosen up front
    /// for the new session, recorded with it. Ignored when resuming.
    pub native_session_id: Option<String>,
    /// Events this run consumes; they must be pending in the lease's lane.
    pub events: Vec<EventId>,
    pub started_at: Timestamp,
}

/// One mirrored backend event (Plan.md §4.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub run_id: RunId,
    /// Sequence number within the run, starting at 1, without gaps.
    pub seq: i64,
    pub at: Timestamp,
    pub entry: Json,
}

/// One run record (Plan.md §14.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub lane_id: LaneId,
    pub session_id: SessionId,
    pub machine_id: MachineId,
    pub lease_epoch: i64,
    pub agent_id: Option<AgentId>,
    pub backend: Option<BackendKind>,
    pub status: RunStatus,
    pub outcome: Option<RunOutcome>,
    /// The final free text, never delivered (Plan.md §4.5).
    pub rationale: Option<String>,
    pub error: Option<String>,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
}

/// How a failed run's events are retried (Plan.md §4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Whether this failure uses up one of the events' attempts. `false`
    /// for failures that are nobody's fault, such as a usage limit (§5.3).
    pub count_attempt: bool,
    /// Events reaching this many failed attempts are marked dead.
    pub max_attempts: u32,
    /// No run starts on the lane before this time.
    pub backoff_until: Timestamp,
}

/// What [`crate::store::SessionStore::fail_run`] did with the run's events.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FailedRun {
    /// Returned to pending for another attempt.
    pub retried: Vec<EventId>,
    /// Marked dead after exhausting their attempts.
    pub dead: Vec<EventId>,
}

/// Identifies one call to one of the system's tools within a run (Plan.md §4.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallKey {
    pub run_id: RunId,
    /// The backend's call id, or the tool name and a hash of the arguments.
    pub key: String,
}

/// What happened when a tool call was recorded before being performed.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallBegin {
    /// First time: perform it, then record the result.
    Perform,
    /// Already finished: return the recorded result.
    Finished(Json),
    /// Started before but never finished, and not safe to repeat.
    OutcomeUnknown,
}

/// A task (Plan.md §5.2 `tasks`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub project_id: ProjectId,
    pub lane_id: LaneId,
    pub agent_id: AgentId,
    pub title: String,
    pub goal: String,
    pub repositories: Vec<RepositoryId>,
    pub parent_task_id: Option<TaskId>,
    pub depends_on: Vec<TaskId>,
    pub origin: TaskOrigin,
    pub status: TaskStatus,
    pub priority: i64,
    pub created_by_call: Option<String>,
    pub last_machine_id: Option<MachineId>,
    pub waiting_reason: Option<String>,
    pub result: Option<Json>,
    pub created_at: Timestamp,
}

/// Input for [`crate::store::TaskStore::create_task`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewTask {
    pub project_id: ProjectId,
    pub agent_id: AgentId,
    pub title: String,
    pub goal: String,
    pub repositories: Vec<RepositoryId>,
    pub parent_task_id: Option<TaskId>,
    pub depends_on: Vec<TaskId>,
    pub origin: TaskOrigin,
    /// `Proposed` or `Queued`. A queued task gets a start event.
    pub status: TaskStatus,
    pub priority: i64,
    /// Tool call id; makes creation idempotent.
    pub created_by_call: Option<String>,
    pub created_at: Timestamp,
}

/// A checkpoint record; the bundle lives beside the database (Plan.md §5.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub task_id: TaskId,
    pub run_id: RunId,
    pub repository_id: RepositoryId,
    pub base_commit: String,
    pub branch: String,
    pub bundle_ref: String,
    pub bundle_sha256: String,
    pub created_at: Timestamp,
}

/// A machine membership (Plan.md §5.3 `machines`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub id: MachineId,
    pub name: String,
    /// The membership's public key; its identity on the transport.
    pub endpoint_id: String,
    pub is_hub: bool,
    pub trust: MachineTrust,
    pub status: MachineStatus,
    pub last_seen: Option<Timestamp>,
    pub capabilities: Json,
    pub backends: Json,
    pub gh_login: bool,
    pub max_parallel: u32,
    pub load: u32,
}

/// Input for [`crate::store::MachineStore::register_machine`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewMachine {
    pub name: String,
    pub endpoint_id: String,
    pub is_hub: bool,
    pub trust: MachineTrust,
    pub max_parallel: u32,
}

/// What a runner reports about its machine in `hello` and `heartbeat`.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineReport {
    pub capabilities: Json,
    pub backends: Json,
    pub gh_login: bool,
    pub load: u32,
}

/// A backend account the space uses (Plan.md §5.3 `accounts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub label: String,
    pub backend: BackendKind,
    pub max_parallel: Option<u32>,
    pub cooldown_until: Option<Timestamp>,
}

/// A future wake-up (Plan.md §7.2 `timers`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timer {
    pub id: TimerId,
    pub project_id: ProjectId,
    pub lane_id: LaneId,
    pub agent_id: Option<AgentId>,
    pub task_ref: Option<TaskId>,
    pub category: TimerCategory,
    pub due_at: Timestamp,
    /// Schedules only: a recurrence rule plus a named time zone.
    pub recurrence: Option<Recurrence>,
    pub instruction: String,
    pub status: TimerStatus,
    pub created_by: TimerCreator,
    pub created_by_call: Option<String>,
    pub last_fired_at: Option<Timestamp>,
    pub fire_count: u32,
    /// Whether the events it fires count as work you requested, taken from
    /// its creator (docs/milestone-8.md §1 decision 3).
    pub requested: bool,
}

/// The most timers a project may have that have not ended, active or paused
/// (docs/milestone-8.md §1 decision 5).
pub const MAX_LIVE_TIMERS: usize = 20;
/// The shortest interval an agent's routine may repeat at.
pub const MIN_SCHEDULE_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// The soonest an agent's follow-up may be due.
pub const MIN_FOLLOW_UP_DELAY: Duration = Duration::from_secs(60);

/// When a routine fires: a rule of our own, not RRULE, evaluated in a named
/// time zone so "9am daily" stays 9am across daylight-saving changes
/// (docs/milestone-8.md §1 decision 1). Stored as JSON in
/// `timers.recurrence`, for example
/// `{"rule": "weekdays", "days": ["mon", "fri"], "hour": 8, "minute": 30,
/// "zone": "Europe/Berlin"}`. The hub computes the occurrences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recurrence {
    #[serde(flatten)]
    pub rule: Rule,
    /// IANA time zone name, for example `Europe/Berlin`.
    pub zone: String,
}

/// The cadence of a [`Recurrence`]. Times are local to its zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum Rule {
    /// Every `minutes` minutes of elapsed time.
    Interval { minutes: u32 },
    /// Every day at a local time.
    Daily { hour: u8, minute: u8 },
    /// On the named days of the week at a local time.
    Weekdays {
        days: Vec<Weekday>,
        hour: u8,
        minute: u8,
    },
    /// Every month on `day` at a local time; in a shorter month, on its
    /// last day.
    Monthly { day: u8, hour: u8, minute: u8 },
}

impl Recurrence {
    /// Checks the rule's fields; the zone is checked by the hub, which has
    /// the zone database.
    ///
    /// # Errors
    ///
    /// What is wrong, in a sentence.
    pub fn check(&self) -> Result<(), String> {
        if self.zone.trim().is_empty() {
            return Err("a recurrence needs a time zone, for example Europe/Berlin".into());
        }
        let time = |hour: u8, minute: u8| {
            if hour < 24 && minute < 60 {
                Ok(())
            } else {
                Err(format!("{hour:02}:{minute:02} is not a time of day"))
            }
        };
        match &self.rule {
            Rule::Interval { minutes } if *minutes == 0 => {
                Err("an interval must be at least one minute".into())
            }
            Rule::Interval { .. } => Ok(()),
            Rule::Daily { hour, minute } => time(*hour, *minute),
            Rule::Weekdays { days, .. } if days.is_empty() => {
                Err("name at least one day of the week".into())
            }
            Rule::Weekdays { hour, minute, .. } => time(*hour, *minute),
            Rule::Monthly { day, .. } if !(1..=31).contains(day) => {
                Err(format!("{day} is not a day of the month"))
            }
            Rule::Monthly { hour, minute, .. } => time(*hour, *minute),
        }
    }

    /// The shortest time between two occurrences, near enough to compare
    /// with [`MIN_SCHEDULE_INTERVAL`]: a calendar rule repeats at most daily.
    #[must_use]
    pub fn min_interval(&self) -> Duration {
        match self.rule {
            Rule::Interval { minutes } => Duration::from_secs(u64::from(minutes) * 60),
            _ => Duration::from_secs(23 * 60 * 60),
        }
    }
}

impl fmt::Display for Recurrence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.rule {
            Rule::Interval { minutes } => return write!(f, "every {minutes} minutes"),
            Rule::Daily { hour, minute } => write!(f, "daily at {hour:02}:{minute:02}")?,
            Rule::Weekdays { days, hour, minute } => {
                let days: Vec<&str> = days.iter().map(|day| day.as_str()).collect();
                write!(f, "on {} at {hour:02}:{minute:02}", days.join(","))?;
            }
            Rule::Monthly { day, hour, minute } => {
                write!(f, "monthly on day {day} at {hour:02}:{minute:02}")?;
            }
        }
        write!(f, " {}", self.zone)
    }
}

/// Input for [`crate::store::TimerStore::create_timer`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewTimer {
    pub project_id: ProjectId,
    pub lane_id: LaneId,
    pub agent_id: Option<AgentId>,
    pub task_ref: Option<TaskId>,
    pub category: TimerCategory,
    pub due_at: Timestamp,
    pub recurrence: Option<Recurrence>,
    pub instruction: String,
    pub created_by: TimerCreator,
    pub created_by_call: Option<String>,
    /// See [`Timer::requested`].
    pub requested: bool,
}

/// One option on a decision; each carries one complete call (Plan.md §9.4).
/// An approval's options say in `effect` whether choosing them performs the
/// stored call: `{"approve": true}` or `{"approve": false}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionOption {
    pub label: String,
    pub effect: Json,
}

impl DecisionOption {
    /// Whether choosing this option performs the stored call.
    #[must_use]
    pub fn approves(&self) -> bool {
        self.effect.get("approve") == Some(&Json::Bool(true))
    }
}

/// A paused tool call shown to you (Plan.md §9.4 `decisions`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub id: DecisionId,
    pub project_id: ProjectId,
    pub lane_id: LaneId,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub tool_call_id: Option<String>,
    pub tool: String,
    pub arguments: Json,
    pub arguments_hash: String,
    pub kind: DecisionKind,
    pub question: String,
    pub context: String,
    pub options: Vec<DecisionOption>,
    pub recommended: Option<u32>,
    pub offers_standing_rule: bool,
    pub status: DecisionStatus,
    /// A [`DecisionAnswer`], once answered.
    pub answer: Option<Json>,
    pub expires_at: Option<Timestamp>,
    pub created_at: Timestamp,
    /// What performing the approved call did, once it was performed or
    /// refused: `{"performed": true, ...}` or `{"performed": false, "error": ...}`.
    pub outcome: Option<Json>,
}

impl Decision {
    /// The option the answer chose, if answered.
    #[must_use]
    pub fn chosen(&self) -> Option<&DecisionOption> {
        let option = self.answer.as_ref()?.get("option")?.as_u64()?;
        self.options.get(usize::try_from(option).ok()?)
    }

    /// Whether the decision was answered with an option that performs the
    /// stored call.
    #[must_use]
    pub fn approved(&self) -> bool {
        self.status == DecisionStatus::Answered
            && self.chosen().is_some_and(DecisionOption::approves)
    }
}

/// Input for [`crate::store::DecisionStore::open_decision`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewDecision {
    pub project_id: ProjectId,
    pub lane_id: LaneId,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub tool_call_id: Option<String>,
    pub tool: String,
    pub arguments: Json,
    pub arguments_hash: String,
    pub kind: DecisionKind,
    pub question: String,
    pub context: String,
    pub options: Vec<DecisionOption>,
    pub recommended: Option<u32>,
    pub offers_standing_rule: bool,
    pub expires_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// Your answer to a decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionAnswer {
    /// Index into the decision's options.
    pub option: u32,
    pub note: Option<String>,
    /// "Always allow this here".
    pub standing_rule: bool,
    pub answered_at: Timestamp,
}

/// Result of answering a decision. Answering is idempotent (Plan.md §9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answered {
    /// The answer was recorded and an answer event inserted.
    Recorded(EventId),
    /// The decision was no longer pending; nothing changed.
    Ignored,
}

/// An outgoing message (Plan.md §10.1 `outbox`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxMessage {
    pub id: OutboxId,
    pub project_id: Option<ProjectId>,
    pub lane_id: Option<LaneId>,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub tool_call_id: Option<String>,
    pub kind: OutboxKind,
    pub decision_id: Option<DecisionId>,
    pub channel: String,
    pub body: Json,
    pub urgency: Urgency,
    pub status: OutboxStatus,
    pub not_before: Option<Timestamp>,
    pub attempts: u32,
    pub channel_message_id: Option<String>,
    pub created_at: Timestamp,
    pub sent_at: Option<Timestamp>,
    /// Position in the order messages were delivered, set when sent. The
    /// client's feed cursor; ids are not used because pacing can deliver a
    /// later message first.
    pub delivery_seq: Option<i64>,
}

/// Input for [`crate::store::OutboxStore::enqueue`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewOutboxMessage {
    pub project_id: Option<ProjectId>,
    pub lane_id: Option<LaneId>,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    /// Unique; makes sending idempotent (Plan.md invariant 6).
    pub tool_call_id: Option<String>,
    pub kind: OutboxKind,
    pub decision_id: Option<DecisionId>,
    pub channel: String,
    pub body: Json,
    pub urgency: Urgency,
    pub not_before: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// One version of one memory file (Plan.md §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFile {
    pub id: MemoryFileId,
    /// Path relative to the memory root, for example `projects/3/notes/build.md`.
    pub path: String,
    pub content: String,
    /// Increments on every write, starting at 1.
    pub version: i64,
    /// The run that wrote this version; `None` when you wrote it yourself
    /// ([`crate::store::MemoryStore::write_by_owner`]), or for a row from
    /// before milestone 5.
    pub written_by_run: Option<RunId>,
    pub deleted: bool,
    pub written_at: Timestamp,
}

/// Who changes memory, for every write of [`crate::store::MemoryStore`]
/// (Plan.md §8.3 "every write is versioned with the run that made it").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAuthor {
    /// The writing run's lease; a stale one writes nothing (invariant 10).
    pub lease: Lease,
    pub run_id: RunId,
    /// What the change is keyed on: a change repeated with the same key
    /// writes nothing more and returns what the first wrote (invariant 5).
    pub call_key: String,
    pub at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_through_their_stored_form() {
        for mode in Mode::ALL {
            assert_eq!(mode.as_str().parse::<Mode>(), Ok(*mode));
        }
        for kind in EventKind::ALL {
            assert_eq!(kind.as_str().parse::<EventKind>(), Ok(*kind));
            let json = serde_json::to_value(kind).expect("serializes");
            assert_eq!(json, Json::String(kind.as_str().to_owned()));
        }
        assert!("bogus".parse::<TaskStatus>().is_err());
    }

    #[test]
    fn tasks_move_only_along_the_lifecycle_of_plan_5_2() {
        use TaskStatus::*;
        let allowed = [
            (Proposed, Queued),
            (Proposed, Cancelled),
            (Queued, Running),
            (Queued, Cancelled),
            (Running, WaitingOnYou),
            (Running, Sleeping),
            (Running, Queued),
            (Running, Done),
            (Running, Failed),
            (Running, Cancelled),
            (WaitingOnYou, Queued),
            (WaitingOnYou, Cancelled),
            (Sleeping, Queued),
            (Sleeping, Cancelled),
        ];
        for &from in TaskStatus::ALL {
            for &to in TaskStatus::ALL {
                assert_eq!(
                    from.can_become(to),
                    allowed.contains(&(from, to)),
                    "{from} to {to}"
                );
            }
        }
        assert!(TaskOrigin::User.is_requested() && TaskOrigin::UserRoutine.is_requested());
        assert!(!TaskOrigin::Agent.is_requested() && !TaskOrigin::Event.is_requested());
    }

    #[test]
    fn a_recurrence_is_plain_json_with_its_zone() {
        let weekly = Recurrence {
            rule: Rule::Weekdays {
                days: vec![Weekday::Mon, Weekday::Fri],
                hour: 8,
                minute: 30,
            },
            zone: "Europe/Berlin".into(),
        };
        let json = serde_json::json!({
            "rule": "weekdays", "days": ["mon", "fri"], "hour": 8, "minute": 30,
            "zone": "Europe/Berlin",
        });
        assert_eq!(serde_json::to_value(&weekly).unwrap(), json);
        assert_eq!(serde_json::from_value::<Recurrence>(json).unwrap(), weekly);
        assert_eq!(weekly.to_string(), "on mon,fri at 08:30 Europe/Berlin");
        assert!(weekly.check().is_ok());
        let every = |minutes| Recurrence {
            rule: Rule::Interval { minutes },
            zone: "UTC".into(),
        };
        assert_eq!(every(20).min_interval(), Duration::from_secs(1200));
        assert!(every(0).check().is_err());
        for bad in [
            Rule::Daily {
                hour: 24,
                minute: 0,
            },
            Rule::Monthly {
                day: 0,
                hour: 9,
                minute: 0,
            },
            Rule::Weekdays {
                days: Vec::new(),
                hour: 9,
                minute: 0,
            },
        ] {
            let recurrence = Recurrence {
                rule: bad,
                zone: "UTC".into(),
            };
            assert!(recurrence.check().is_err(), "{recurrence:?}");
        }
    }

    #[test]
    fn modes_order_from_least_to_most_independent() {
        assert_eq!(Mode::Autonomous.stricter(Mode::Assisted), Mode::Assisted);
        assert_eq!(Mode::Autonomous.stepped_down(), Mode::Assisted);
        assert_eq!(Mode::Assisted.stepped_down(), Mode::OnRequest);
        assert_eq!(Mode::OnRequest.stepped_down(), Mode::OnRequest);
        assert_eq!(Mode::Off.stepped_down(), Mode::Off);
    }
}
