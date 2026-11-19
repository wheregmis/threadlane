//! What a hub offers its clients (Plan.md §11.2).
//!
//! [`HubApi`] describes one space and has no notion of any other. The hub
//! implements it; a transport carries it to clients as [`ClientRequest`] and
//! [`ClientResponse`]. Later milestones add capabilities; so far: who am I
//! talking to, send a message, follow the feed, the organization, decisions,
//! pairing, the overview the app's screens show, memory, timers, and tasks.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::admission::AdmittedKey;
use crate::ids::{
    DecisionId, EventId, LaneId, MachineId, ProjectId, RunId, StandingRuleId, TaskId, TimerId,
};
use crate::model::{
    Account, Agent, BackendKind, Cell, Decision, EventKind, Machine, MemoryFile, Mode,
    OutboxMessage, Recurrence, Repository, Run, StandingRule, Task, Timer,
};
use crate::time::Timestamp;
use crate::tools::Tier;

/// What a hub says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubInfo {
    /// The space's name, for example `Work`.
    pub space: String,
    pub mode_ceiling: Mode,
    pub schema_version: i64,
    /// Running release version when the hub supports in-app updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_version: Option<String>,
}

/// An update request can select a release version, never a download location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HubUpdate {
    Check,
    Install { version: String },
}

/// A project as a client lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub name: String,
    pub is_general: bool,
    pub paused: bool,
    /// Its own ceiling, if set; the space's still caps it (Plan.md §2.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_ceiling: Option<Mode>,
    /// Whether this project is archived.
    #[serde(default)]
    pub archived: bool,
}

use crate::attachment::ImageAttachment;

/// A message from you to a project's lead, or to one of its tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessage {
    /// The project's name. `None` means the General project.
    pub project: Option<String>,
    pub text: String,
    /// Images attached to a message to a lead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
    /// Chosen by the client, unique per message. Sending the same id twice
    /// is a successful no-op, so a client may retry freely.
    pub client_message_id: String,
    /// The task whose lane it goes to instead of the lead's (Plan.md §4.2).
    /// While a decision is open there, it declines it with the message as
    /// the note (§9.4). The task names its project, so `project` is then
    /// ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
}

/// The hub's acknowledgement of a [`UserMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub event_id: EventId,
    pub project_id: ProjectId,
    /// `true` if this `client_message_id` had been accepted before.
    pub duplicate: bool,
}

/// One delivered message or decision card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedItem {
    /// Position in the feed. Pass the last one seen as `after`.
    pub seq: i64,
    pub message: OutboxMessage,
    pub project_name: Option<String>,
    pub delivered_at: Timestamp,
    /// Original results grouped into this digest, in delivery order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub digest: Vec<DigestResult>,
}

/// A result delivered as part of a digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestResult {
    /// Stable identity of the original outbox message.
    pub id: crate::ids::OutboxId,
    /// The project that sent it, if it came from a project.
    pub project: Option<String>,
    /// The original result text.
    pub text: String,
}

/// The results whose delivery is currently held by pacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldResults {
    /// Number of original results held, including those grouped into a digest.
    pub count: u64,
    /// The next scheduled release.
    pub next: Timestamp,
    /// The latest scheduled release currently recorded.
    pub latest: Timestamp,
}

/// An agent's binding to a project as a client lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingSummary {
    /// The agent's name.
    pub agent: String,
    pub role: String,
    pub mode: Mode,
    pub is_lead: bool,
    /// How many of its tasks may run at once in the project; 0 means no
    /// limit.
    #[serde(default)]
    pub max_parallel: u32,
    /// The cell each overridden tier is set to (Plan.md §2.3 "Overrides").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<Tier, Cell>,
}

/// The Organization capability (Plan.md §11.2): projects, repositories,
/// agents, bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OrgRequest {
    /// Creates a project with its lead lane and lead binding.
    CreateProject { name: String, description: String },
    /// Registers a repository in a project. `None` means the General
    /// project, and a default branch of `main`. A remote already registered
    /// in this space is refused.
    AddRepository {
        project: Option<String>,
        name: String,
        remote_url: String,
        default_branch: Option<String>,
    },
    /// The repositories of a project. `None` means the General project.
    Repositories { project: Option<String> },
    /// Every agent in the space.
    Agents,
    CreateAgent {
        name: String,
        role: String,
        instructions: String,
        /// The backends it may use, in order of preference; empty means
        /// either.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        backends: Vec<BackendKind>,
    },
    /// Distinct operation so older hubs reject model overrides before creating the agent.
    CreateAgentWithModels {
        name: String,
        role: String,
        instructions: String,
        backends: Vec<BackendKind>,
        models: BTreeMap<String, String>,
    },
    /// Personal skills installed on the hub's configured backends.
    InstalledSkills,
    CreateAgentWithSkills {
        name: String,
        role: String,
        instructions: String,
        backends: Vec<BackendKind>,
        models: BTreeMap<String, String>,
        skills: Vec<crate::model::SkillBinding>,
    },
    /// The bindings of a project. `None` means the General project.
    Bindings { project: Option<String> },
    /// Sets the agent's mode in a project, binding it there if it is not yet.
    /// `None` means the General project.
    SetMode {
        project: Option<String>,
        agent: String,
        mode: Mode,
    },
    /// Pauses a project, or resumes it with `paused: false`: a paused
    /// project's lanes are not granted. `None` means the General project.
    /// Answered with the project as it now stands.
    SetPaused {
        project: Option<String>,
        paused: bool,
    },
    /// Sets a project's own ceiling, which must be no higher than the
    /// space's. Bindings above it keep their stored mode and act at the
    /// ceiling. `None` means the General project.
    SetCeiling { project: Option<String>, mode: Mode },
    /// Sets the cell of one tier of the mode matrix for an agent's binding,
    /// or clears it with `None`. Stricter than its mode is always accepted;
    /// looser only up to what the project ceiling's column allows (Plan.md
    /// §2.3 "Overrides"). `None` as the project means the General project.
    SetOverride {
        project: Option<String>,
        agent: String,
        tier: Tier,
        cell: Option<Cell>,
    },
    /// Sets how many of an agent's tasks may run at once in a project; 0
    /// means no limit. Answered with the binding. `None` as the project
    /// means the General project.
    SetMaxParallel {
        project: Option<String>,
        agent: String,
        max_parallel: u32,
    },
    /// Sets this machine’s shared slot limit.
    SetMachineSlots { machine: MachineId, slots: u32 },
    /// Sets the space's own ceiling, which caps every project's (Plan.md
    /// §2.3): lowering it lowers what every binding acts at, and raising it
    /// lets projects without a stricter ceiling of their own go higher.
    SetSpaceCeiling { mode: Mode },
    /// Reads the space's delivery pacing.
    Pacing,
    /// Replaces delivery pacing for the space.
    SetPacing { pacing: crate::model::Pacing },
    /// Stops the target lane, retaining its work and pending events.
    Stop {
        target: LaneTarget,
        /// Stable identifier for a retried client request.
        client_request_id: String,
    },
    /// Makes a stopped lane eligible again.
    ResumeLane { target: LaneTarget },
    /// Archives or restores a project. Answered with its updated summary.
    SetArchived {
        project: Option<String>,
        archived: bool,
    },
    /// Permanently deletes the project if the exact name was confirmed.
    DeleteProject {
        project: String,
        confirm_name: String,
    },
    /// Counts a project's data and local workspaces before deletion.
    DeletePreview { project: String },
    /// The starter agents compiled into the hub (docs/agent-templates.md).
    Templates,
    /// Changes an agent. A field left out keeps its value; a model set to
    /// `""` goes back to the backend's default. It applies from the agent's
    /// next run. Answered with the agent.
    UpdateAgent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skills: Option<Vec<crate::model::SkillBinding>>,
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
        /// Backend names in order of preference; empty means either.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backends: Option<Vec<String>>,
        /// The model per backend name, changing only the backends named.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models: Option<BTreeMap<String, String>>,
    },
    /// Deletes an agent with its bindings and standing rules if the exact
    /// name was confirmed. Refused for the lead and while any of its tasks
    /// has not ended.
    DeleteAgent { agent: String, confirm_name: String },
}

/// What deleting one project removes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePreview {
    /// Task records of every status.
    pub tasks: u64,
    /// Tasks still queued.
    pub queued_tasks: u64,
    /// Inbox records.
    pub events: u64,
    /// Outbox records.
    pub outbox: u64,
    /// Decisions of every status.
    pub decisions: u64,
    /// Decisions still awaiting an answer.
    pub open_decisions: u64,
    /// Current project memory records, including generated indexes.
    pub memory_files: u64,
    /// Historical versions of project memory.
    pub memory_versions: u64,
    /// Routine records, including ended routines.
    pub routines: u64,
    /// Follow-up records, including ended follow-ups.
    pub follow_ups: u64,
    /// System dreaming timer records.
    pub dreaming_timers: u64,
    /// Standing rules, including revoked ones.
    pub standing_rules: u64,
    /// Agent bindings.
    pub bindings: u64,
    /// Registered repositories.
    pub repositories: u64,
    /// Local task worktree directories.
    pub worktrees: u64,
    /// Local clone and partial clone directories.
    pub clones: u64,
}

impl DeletePreview {
    /// A compact inventory for the confirmation and deletion result.
    #[must_use]
    pub fn inventory(&self) -> String {
        let count = |n, noun| format!("{n} {noun}{}", if n == 1 { "" } else { "s" });
        [
            format!(
                "{} ({} queued)",
                count(self.tasks, "task"),
                self.queued_tasks
            ),
            format!(
                "{} ({} and {} outbox rows)",
                count(self.events.saturating_add(self.outbox), "message"),
                count(self.events, "event"),
                self.outbox
            ),
            format!(
                "{} ({} open)",
                count(self.decisions, "decision"),
                self.open_decisions
            ),
            format!(
                "{} and {}",
                count(self.memory_files, "memory file"),
                count(self.memory_versions, "version")
            ),
            count(self.routines, "routine"),
            count(self.follow_ups, "follow-up"),
            count(self.dreaming_timers, "dreaming timer"),
            count(self.standing_rules, "standing rule"),
            count(self.bindings, "agent binding"),
            format!(
                "{} {}",
                self.repositories,
                if self.repositories == 1 {
                    "repository"
                } else {
                    "repositories"
                }
            ),
            count(self.worktrees, "worktree"),
            format!(
                "and {} {}",
                self.clones,
                if self.clones == 1 {
                    "clone directory"
                } else {
                    "clone directories"
                }
            ),
        ]
        .join(", ")
    }
}

/// A starter agent (docs/agent-templates.md §1). Adding one creates an
/// ordinary agent; later changes to the template do not touch it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTemplate {
    /// The file name without `.md`, for example `chief-of-staff`.
    pub id: String,
    pub name: String,
    /// The one-line role from the template's first line.
    pub role: String,
    /// The backends it may use, in order of preference.
    pub backends: Vec<BackendKind>,
    pub instructions: String,
}

/// The lane targeted by a stop or resume request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaneTarget {
    /// The named project's lead, or General when omitted.
    Lead { project: Option<String> },
    /// A task's lane.
    Task { task: TaskId },
}

/// The answer to an [`OrgRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
#[allow(
    clippy::large_enum_variant,
    reason = "only with serde_json's preserve_order, which GPUI turns on in the workspace build"
)]
pub enum OrgReply {
    InstalledSkills {
        skills: Vec<crate::model::SkillBinding>,
        /// Backends that failed to list theirs; their bindings can't be checked.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unavailable: Vec<BackendKind>,
    },
    MachineSlotsSet,
    Project {
        project: ProjectSummary,
    },
    Repository {
        repository: Repository,
    },
    Repositories {
        repositories: Vec<Repository>,
    },
    Agents {
        agents: Vec<Agent>,
    },
    Agent {
        agent: Agent,
    },
    Bindings {
        bindings: Vec<BindingSummary>,
    },
    Binding {
        binding: BindingSummary,
    },
    /// A project's ceiling after [`OrgRequest::SetCeiling`]: the stricter of
    /// its own and the space's.
    Ceiling {
        project: String,
        ceiling: Mode,
    },
    /// A binding's overrides after [`OrgRequest::SetOverride`].
    Overrides {
        agent: String,
        overrides: BTreeMap<Tier, Cell>,
    },
    /// The space's ceiling after [`OrgRequest::SetSpaceCeiling`].
    SpaceCeiling {
        ceiling: Mode,
    },
    /// The current pacing settings.
    Pacing {
        pacing: crate::model::Pacing,
    },
    /// A stop or resume was recorded.
    LaneUpdated,
    /// The project and local directories a deletion would remove.
    DeletePreview {
        preview: DeletePreview,
    },
    /// The committed deletion and directories that still need cleanup.
    ProjectDeleted {
        project: String,
        deleted: DeletePreview,
        leftovers: Vec<String>,
    },
    Templates {
        templates: Vec<AgentTemplate>,
    },
    /// The agent [`OrgRequest::DeleteAgent`] deleted.
    AgentDeleted {
        agent: String,
    },
}

/// The Decisions capability (Plan.md §9.4, §11.2): what waits on you, and
/// your answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DecisionRequest {
    /// Every decision in the space still waiting on you, oldest first.
    Pending,
    /// Answers a decision with one of its options, by label (any case).
    /// Answering is idempotent: a decision already answered keeps its
    /// first answer.
    Answer {
        decision: DecisionId,
        option: String,
        note: Option<String>,
    },
    /// Answers as [`DecisionRequest::Answer`] does with an option that
    /// approves, and creates a standing rule from the decision in the same
    /// transaction: "always allow this here". Refused for a decision that
    /// offers no rule, or an option that does not approve.
    AnswerAlways {
        decision: DecisionId,
        option: String,
        note: Option<String>,
    },
    /// The standing rules of a project, revoked ones included, oldest
    /// first. `None` means every project.
    StandingRules { project: Option<String> },
    /// Revokes a standing rule; the next matching call asks again.
    RevokeRule { rule: StandingRuleId },
    /// Claims the one reminder for a decision still waiting. Answered with
    /// [`DecisionReply::Reminder`], `first` only for the first claim.
    Remind { decision: DecisionId },
}

/// A standing rule as a client lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandingRuleSummary {
    pub rule: StandingRule,
    /// Its project's name.
    pub project: String,
    /// Its agent's name.
    pub agent: String,
}

/// The answer to a [`DecisionRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DecisionReply {
    Pending {
        decisions: Vec<Decision>,
    },
    /// The decision as it now stands. `recorded` is `false` when it had
    /// been answered before and this answer was ignored.
    Answered {
        decision: Box<Decision>,
        recorded: bool,
    },
    StandingRules {
        rules: Vec<StandingRuleSummary>,
    },
    /// The rule as it now stands.
    StandingRule {
        rule: StandingRuleSummary,
    },
    /// Whether this was the first claim of the decision's reminder.
    Reminder {
        first: bool,
    },
}

/// The Pairing capability (Plan.md §11.2): join codes and admitted clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PairingRequest {
    /// A new one-time join code for a client, valid for a few minutes.
    IssueJoinCode,
    /// Every admitted key, revoked ones included.
    Clients,
    /// Revokes the active key of this name and closes its connections.
    Revoke { name: String },
}

/// The answer to a [`PairingRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PairingReply {
    /// The code to give the new client. It carries the hub's id, how to reach
    /// it, and a one-time secret.
    JoinCode {
        code: String,
        expires_at: Timestamp,
    },
    Clients {
        clients: Vec<AdmittedKey>,
    },
    Revoked {
        client: AdmittedKey,
    },
}

/// A task as the board, the running list, and the task screen show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task: Task,
    /// Its project's name.
    pub project: String,
    /// Its agent's name.
    pub agent: String,
    /// The machine of its active run, else the one it last ran on.
    pub machine: Option<String>,
    /// The machine of its active run. Machine display names can repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_machine_id: Option<MachineId>,
    /// When its active run started; `None` while no run is active.
    pub running_since: Option<Timestamp>,
    /// The latest journal step in the active run, if recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_step: Option<RunStep>,
    /// When the task completed, if it is done and its result event is recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
}

/// A one-line description of a backend journal entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStep {
    /// The backend event kind.
    pub kind: String,
    /// A compact description without tool arguments or output.
    pub description: String,
    /// When the runner saw the step.
    pub at: Timestamp,
}

/// Usage reported by the backend for a finished run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunUsage {
    /// Input tokens as counted by the backend.
    pub input_tokens: Option<u64>,
    /// Output tokens as counted by the backend.
    pub output_tokens: Option<u64>,
    /// Cost in US dollars, if reported.
    pub cost_usd: Option<f64>,
}

/// One run of a task, with the name of the machine it ran on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run: Run,
    pub machine: String,
    /// The run's journal steps, in the order reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<RunStep>,
    /// The finished run's reported usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<RunUsage>,
}

/// Everything the task screen shows (Plan.md §11.1 "Task").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub summary: TaskSummary,
    /// The branch it works on.
    pub branch: String,
    /// The names of the repositories it touches.
    pub repositories: Vec<String>,
    /// Its runs, oldest first.
    pub runs: Vec<RunSummary>,
    /// The task's independent review rounds, in submission order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<TaskReviewActivity>,
    /// Every decision asked in its lane, oldest first.
    pub decisions: Vec<Decision>,
    /// The tasks it depends on and the tasks that depend on it.
    pub related: Vec<TaskSummary>,
    /// Your messages to its lane, oldest first: only
    /// [`ConversationEntry::You`] entries.
    #[serde(default)]
    pub messages: Vec<ConversationEntry>,
    /// The original proposer of accepted work, including its subtasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_by: Option<String>,
    /// Whether the task lane needs a new message or a resume before it can run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
}

/// A submitted revision and its committed review outcome, if available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskReviewActivity {
    pub submission: RunId,
    pub reviewer: crate::ids::AgentId,
    pub reviewer_name: String,
    pub revisions: Vec<crate::workspace::ReviewRevision>,
    pub submitted_at: Timestamp,
    pub verdict: Option<crate::model::TaskReviewVerdict>,
    pub feedback: Option<String>,
    pub decided_at: Option<Timestamp>,
}

/// Maximum chat history page that includes images. Full image messages fit within the remote
/// transport response limit at this page size.
pub const CONVERSATION_PAGE: u32 = 20;

/// One entry of the conversation with a project's lead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
#[non_exhaustive]
#[allow(
    clippy::large_enum_variant,
    reason = "only with serde_json's preserve_order, which GPUI turns on in the workspace build"
)]
pub enum ConversationEntry {
    /// A message you sent.
    You {
        event_id: EventId,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageAttachment>,
        at: Timestamp,
    },
    /// An ask the space lead, the General project's lead, handed to this
    /// project's lead (docs/one-place-to-talk.md).
    SpaceLead {
        event_id: EventId,
        /// What it asked.
        text: String,
        at: Timestamp,
    },
    /// In General's conversation: a project lead's reply to the space
    /// lead's ask.
    ProjectReply {
        event_id: EventId,
        /// The project whose lead replied.
        project: String,
        /// The ask it replies to, an event in that project's lead lane.
        ask_event_id: Option<EventId>,
        /// The reply as the space lead received it.
        text: String,
        at: Timestamp,
    },
    /// A delivered message, decision card, or notice, as the feed has it.
    Delivered { item: FeedItem },
    /// Delivery of an outbox message from this project failed permanently.
    DeliveryFailure {
        /// The event recorded by the dispatcher.
        event_id: EventId,
        /// What delivery failed and why.
        text: String,
        /// When it failed.
        at: Timestamp,
    },
}

/// The Overview capability (Plan.md §11.2 "Tasks", "Machines"): what the
/// app's screens show. Read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OverviewRequest {
    RoutineHistory {
        timer: TimerId,
        before: Option<EventId>,
    },
    Agenda {
        tomorrow_in: Option<String>,
    },
    SpaceUsage {
        days: u32,
    },
    Outputs {
        project: String,
        search: String,
    },
    OutputImage {
        project: String,
        event: EventId,
        index: u32,
    },
    ReviewRecipe,
    /// Every task of a project, oldest first. `None` means the General
    /// project.
    Tasks {
        project: Option<String>,
    },
    /// Every running task in the space, oldest first.
    Running,
    /// One task in full.
    Task {
        task: TaskId,
    },
    /// The task details used on the desktop before Activity is opened.
    TaskOverview {
        task: TaskId,
    },
    /// One page of a task's journal steps, newest first.
    TaskActivity {
        task: TaskId,
        before: Option<ActivityCursor>,
    },
    /// Active projects' task and lead summaries.
    Work,

    /// The newest `limit` entries of the conversation with a project's lead
    /// before `before` (the newest of all when `None`), oldest first. `None`
    /// as the project means the General project. Requests larger than
    /// [`CONVERSATION_PAGE`] return text history without image data.
    Conversation {
        project: Option<String>,
        before: Option<Timestamp>,
        limit: u32,
    },
    /// The newest `limit` delivered items in the space, of any project or
    /// none, oldest first.
    Recent {
        limit: u32,
    },
    /// The machines that joined the space, and its model accounts.
    Machines,
    /// Whether a project's lead is working: when the run in progress on its
    /// lead lane started. `None` as the project means the General project.
    Lead {
        project: Option<String>,
    },
    /// The newest `limit` events of a project that were recorded and acted
    /// on by nobody, because the work was unrequested (Plan.md §2.3)
    /// or the project was archived, newest first. `None` means the General project.
    Recorded {
        project: Option<String>,
        limit: u32,
    },
    /// How many such events the whole space has.
    RecordedCount,
    /// Runs on a project's lead lane, oldest first. `None` means General.
    LeadRuns {
        project: Option<String>,
    },
    /// Results held by delivery pacing in this space.
    HeldResults,
}

/// An event that arrived but started no run, for example a GitHub review
/// request for an archived project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedEvent {
    pub id: EventId,
    /// Which adapter produced it, for example `github` or `timer`.
    pub source: String,
    pub kind: EventKind,
    /// What it says, as a run would have read it.
    pub text: String,
    pub received_at: Timestamp,
}

/// Active projects' lead status and tasks, refreshed together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkOverview {
    pub projects: Vec<ProjectSummary>,
    pub tasks: Vec<TaskSummary>,
    pub leads: Vec<ProjectLeadStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectLeadStatus {
    pub project: String,
    pub running_since: Option<Timestamp>,
    pub stopped: bool,
}

/// Exclusive position in the newest-first task activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityCursor {
    pub run: RunId,
    pub seq: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskActivityEntry {
    pub position: ActivityCursor,
    pub step: RunStep,
}

/// The answer to an [`OverviewRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OverviewReply {
    RoutineHistory {
        rows: Vec<crate::work::Occurrence>,
    },
    Agenda {
        recent: Vec<crate::work::Occurrence>,
        upcoming: Vec<crate::work::Scheduled>,
    },
    SpaceUsage {
        groups: Vec<crate::work::UsageGroup>,
    },
    Outputs {
        rows: Vec<crate::work::ProjectOutput>,
    },
    OutputImage {
        image: ImageAttachment,
    },
    ReviewRecipe {
        recipe: crate::work::ReviewRecipe,
    },
    Work {
        overview: WorkOverview,
    },
    TaskActivity {
        entries: Vec<TaskActivityEntry>,
        next: Option<ActivityCursor>,
    },
    Tasks {
        tasks: Vec<TaskSummary>,
    },
    Task {
        task: Box<TaskDetail>,
    },
    Conversation {
        entries: Vec<ConversationEntry>,
    },
    Recent {
        items: Vec<FeedItem>,
    },
    Machines {
        machines: Vec<Machine>,
        accounts: Vec<Account>,
    },
    /// When the lead's run in progress started; `None` while it is idle.
    Lead {
        running_since: Option<Timestamp>,
        /// The latest journal step in the active lead run, if recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latest_step: Option<RunStep>,
        /// Whether a new message or a resume is needed before another run.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stopped: bool,
    },
    /// Events recorded and not acted on, newest first, and how many the
    /// project has in all.
    Recorded {
        events: Vec<RecordedEvent>,
        total: u64,
    },
    /// How many events the space recorded and did not act on.
    RecordedCount {
        total: u64,
    },
    /// Runs of the project's lead lane, oldest first.
    LeadRuns {
        /// Records and message markers from the lead lane.
        runs: Vec<LeadRun>,
    },
    /// The current pacing backlog, if any.
    HeldResults {
        /// Results whose next eligible time is in the future.
        held: Option<HeldResults>,
    },
}

/// A lead run with whether it put an ordinary message in the outbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeadRun {
    /// Its status, outcome, timestamps, and recorded rationale.
    pub run: Run,
    /// Whether it queued an ordinary message, including ones yet to be delivered.
    pub sent_message: bool,
    /// The latest journal step while the run is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_step: Option<RunStep>,
    /// The finished run's reported usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<RunUsage>,
}

/// The Memory capability (Plan.md §8.3, §11.1 "Memory"): the files a
/// project's runs can see, with the lead's reach, and your own changes.
/// Paths are as agents name them, `space/…` and `project/…`, and map to
/// stored ones by the same rules as the agents' memory tools, so no path
/// reaches another project (invariant 12). `None` as the project means the
/// General project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemoryRequest {
    /// Every current file the project's lead can see, by path.
    List { project: Option<String> },
    /// The current version of one file.
    Read {
        project: Option<String>,
        path: String,
    },
    /// Every version of one file, oldest first.
    History {
        project: Option<String>,
        path: String,
    },
    /// Writes a file as you: a new version with no run, refused like an
    /// agent's when over budget, without a title line where an index lists
    /// it, or at an `INDEX.md`.
    Write {
        project: Option<String>,
        path: String,
        content: String,
    },
    /// Deletes a file as you, as a new version.
    Delete {
        project: Option<String>,
        path: String,
    },
    /// The project's current cadence and last completed dream.
    Dreaming { project: Option<String> },
    /// Sets the cadence, or disables dreaming with `None`.
    SetDreaming {
        project: Option<String>,
        cadence: Option<Recurrence>,
    },
}

/// The answer to a [`MemoryRequest`]. Every file's `path` is as agents
/// name it, and `written_by_run` is `None` for a version you wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemoryReply {
    Files {
        files: Vec<MemoryFile>,
    },
    File {
        file: MemoryFile,
    },
    History {
        versions: Vec<MemoryFile>,
    },
    /// The setting and last completed dream.
    Dreaming {
        status: DreamingStatus,
    },
}

/// Why a client request failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The hub cannot be reached; the client shows its last known state.
    #[error("hub unreachable: {0}")]
    Unreachable(String),
    #[error("hub error: {0}")]
    Internal(String),
}

/// The hub interface for clients.
#[async_trait]
pub trait HubApi: Send + Sync + fmt::Debug {
    async fn info(&self) -> Result<HubInfo, ApiError>;
    /// Checks for or installs a signed hub release.
    async fn update_hub(&self, _request: HubUpdate) -> Result<Option<String>, ApiError> {
        Err(ApiError::Invalid(
            "This hub does not support in-app updates".into(),
        ))
    }
    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError>;
    /// Lists active projects and, when requested, archived ones as well.
    async fn project_list(&self, _include_archived: bool) -> Result<Vec<ProjectSummary>, ApiError> {
        self.projects().await
    }
    /// Accepts a message: route, insert with deduplication, wake, acknowledge
    /// (Plan.md §6.3). Returns as soon as the event is durable.
    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError>;
    /// Feed items with `seq` greater than `after`, oldest first. If there are
    /// none, waits up to `wait` for the next one, then returns what there is,
    /// possibly nothing.
    async fn feed(&self, after: i64, wait: Duration) -> Result<Vec<FeedItem>, ApiError>;
    /// Reads or changes the space's projects, agents, and bindings.
    async fn org(&self, request: OrgRequest) -> Result<OrgReply, ApiError>;
    /// Lists or answers the decisions waiting on you.
    async fn decisions(&self, request: DecisionRequest) -> Result<DecisionReply, ApiError>;
    /// Issues join codes, lists admitted keys, revokes them.
    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError>;
    /// Reads the tasks, conversation history, and machines the app shows.
    async fn overview(&self, request: OverviewRequest) -> Result<OverviewReply, ApiError>;
    /// Lists, creates, pauses, resumes, and cancels follow-ups and routines.
    /// The default refuses, for an implementation that offers none.
    async fn timers(&self, request: TimerRequest) -> Result<TimerReply, ApiError> {
        Err(ApiError::Invalid(format!(
            "timers are not offered here: {request:?}"
        )))
    }
    /// Reads a project's memory, and makes your own changes to it.
    async fn memory(&self, request: MemoryRequest) -> Result<MemoryReply, ApiError>;
    /// Creates a task of yours directly, or cancels a task.
    async fn tasks(&self, request: TaskRequest) -> Result<TaskReply, ApiError>;
    /// Reads operational records and performs explicit owner repairs.
    async fn operations(&self, request: OperationsRequest) -> Result<OperationsReply, ApiError> {
        Err(ApiError::Invalid(format!(
            "operations are not offered here: {request:?}"
        )))
    }
}

/// [`HubApi`] as it crosses a transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClientRequest {
    Info,
    UpdateHub {
        request: HubUpdate,
    },
    /// Lists projects, omitting archived projects by default.
    Projects {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        include_archived: bool,
    },
    SendMessage {
        message: UserMessage,
    },
    /// Separate from SendMessage so older hubs cannot silently discard images.
    SendMessageWithImages {
        message: UserMessage,
    },
    Feed {
        after: i64,
        wait_ms: u64,
    },
    Org {
        request: OrgRequest,
    },
    Decisions {
        request: DecisionRequest,
    },
    Pairing {
        request: PairingRequest,
    },
    Overview {
        request: OverviewRequest,
    },
    Timers {
        request: TimerRequest,
    },
    Memory {
        request: MemoryRequest,
    },
    Tasks {
        request: TaskRequest,
    },
    Operations {
        request: OperationsRequest,
    },
}

/// The answer to a [`ClientRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
#[allow(
    clippy::large_enum_variant,
    reason = "only with serde_json's preserve_order, which GPUI turns on in the workspace build"
)]
pub enum ClientResponse {
    Info { info: HubInfo },
    UpdateHub { version: Option<String> },
    Projects { projects: Vec<ProjectSummary> },
    Accepted { accepted: Accepted },
    Feed { items: Vec<FeedItem> },
    Org { reply: OrgReply },
    Decisions { reply: DecisionReply },
    Pairing { reply: PairingReply },
    Overview { reply: OverviewReply },
    Memory { reply: MemoryReply },
    Error { error: ApiError },
    Timers { reply: TimerReply },
    Tasks { reply: TaskReply },
    Operations { reply: OperationsReply },
}

/// Answers one request from an API. Shared by every transport's server side.
pub async fn dispatch(api: &dyn HubApi, request: ClientRequest) -> ClientResponse {
    let result = match request {
        ClientRequest::Info => api.info().await.map(|info| ClientResponse::Info { info }),
        ClientRequest::UpdateHub { request } => api
            .update_hub(request)
            .await
            .map(|version| ClientResponse::UpdateHub { version }),
        ClientRequest::Projects { include_archived } => api
            .project_list(include_archived)
            .await
            .map(|projects| ClientResponse::Projects { projects }),
        ClientRequest::SendMessage { message }
        | ClientRequest::SendMessageWithImages { message } => api
            .send_message(&message)
            .await
            .map(|accepted| ClientResponse::Accepted { accepted }),
        ClientRequest::Feed { after, wait_ms } => api
            .feed(after, Duration::from_millis(wait_ms))
            .await
            .map(|items| ClientResponse::Feed { items }),
        ClientRequest::Org { request } => api
            .org(request)
            .await
            .map(|reply| ClientResponse::Org { reply }),
        ClientRequest::Decisions { request } => api
            .decisions(request)
            .await
            .map(|reply| ClientResponse::Decisions { reply }),
        ClientRequest::Pairing { request } => api
            .pairing(request)
            .await
            .map(|reply| ClientResponse::Pairing { reply }),
        ClientRequest::Overview { request } => api
            .overview(request)
            .await
            .map(|reply| ClientResponse::Overview { reply }),
        ClientRequest::Timers { request } => api
            .timers(request)
            .await
            .map(|reply| ClientResponse::Timers { reply }),
        ClientRequest::Memory { request } => api
            .memory(request)
            .await
            .map(|reply| ClientResponse::Memory { reply }),
        ClientRequest::Tasks { request } => api
            .tasks(request)
            .await
            .map(|reply| ClientResponse::Tasks { reply }),
        ClientRequest::Operations { request } => api
            .operations(request)
            .await
            .map(|reply| ClientResponse::Operations { reply }),
    };
    result.unwrap_or_else(|error| ClientResponse::Error { error })
}

/// The Timers capability (Plan.md §7, docs/milestone-8.md §2): follow-ups
/// and routines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimerRequest {
    RunOnce {
        timer: TimerId,
        client_request_id: String,
    },
    /// The timers of a project that have not ended, active or paused, oldest
    /// first. `None` means the General project.
    List { project: Option<String> },
    /// Creates a routine of yours in a project's lead lane; its events count
    /// as work you requested. `None` means the General project. Creating
    /// with the same `client_request_id` twice is a successful no-op.
    CreateRoutine {
        project: Option<String>,
        instruction: String,
        recurrence: Recurrence,
        client_request_id: String,
    },
    /// Pauses a timer; it fires nothing until resumed.
    Pause { timer: TimerId },
    /// Resumes a paused timer: a routine at its next occurrence from now, a
    /// follow-up at its due time, or at once if that has passed.
    Resume { timer: TimerId },
    /// Cancels a timer for good.
    Cancel { timer: TimerId },
}

/// The answer to a [`TimerRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimerReply {
    Timers {
        timers: Vec<Timer>,
    },
    /// The timer as it now stands.
    Timer {
        timer: Timer,
    },
}

/// The Tasks capability (Plan.md §5.2): work you start or stop yourself,
/// without asking the lead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskRequest {
    SaveReviewRecipe {
        recipe: crate::work::ReviewRecipe,
    },
    StartReviewWorkflow {
        project: String,
        goal: String,
        engineer: String,
        reviewer: String,
        repository: String,
        version: i64,
        client_request_id: String,
    },
    InstallReviewTeam {
        project: String,
        engineer: String,
        reviewer: String,
        recurrence: Recurrence,
    },
    /// Creates a queued task of yours in a project, skipping the lead: work
    /// you requested. `agent` must be bound in the project and not Off;
    /// `repositories` are names registered in it and `depends_on` its tasks
    /// that have not ended. `None` means the General project. Creating with
    /// the same `client_request_id` twice is a successful no-op.
    Create {
        project: Option<String>,
        title: String,
        goal: String,
        agent: String,
        #[serde(default)]
        repositories: Vec<String>,
        #[serde(default)]
        depends_on: Vec<TaskId>,
        client_request_id: String,
    },
    /// Cancels a task that has not ended, and the tasks that wait on it;
    /// the lead hears of it once. Cancelling a cancelled task changes
    /// nothing.
    Cancel {
        task: TaskId,
    },
    /// Queues a failed task again, with a message in its lane quoting the
    /// last error and asking the agent to continue from the work already
    /// on the task branch. Only a failed task can be retried.
    Retry {
        task: TaskId,
    },
}

/// The answer to a [`TaskRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskReply {
    ReviewRecipe {
        recipe: crate::work::ReviewRecipe,
    },
    ReviewTeam {
        timer: Timer,
    },
    /// The task as it now stands.
    Task {
        task: Task,
    },
}

/// Read the hub's operational history or perform an owner repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperationsRequest {
    /// Read a space snapshot, optionally restricted to one project's records.
    Metrics { project: Option<String> },
    /// Inspect a lead lane by project name or a task lane by task ID.
    Lane { target: LaneTarget },
    /// Return a dead event to its lane once.
    Replay { event: EventId },
    /// Restore an earlier memory version as a new owner-authored version.
    Rollback {
        project: Option<String>,
        path: String,
        version: i64,
    },
    /// Read the records carrying this trace ID.
    Trace { id: String },
    /// Checks the project's notes for duplicates and stale references.
    CheckMemory { project: Option<String> },
}

/// The answer to an operations request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperationsReply {
    /// The snapshot at the hub's current time.
    Metrics { metrics: Box<OperationsMetrics> },
    /// Lease and history of the selected lane.
    Lane { lane: Box<LaneInspection> },
    /// The event after its status and attempts were reset.
    Replayed { event: crate::model::Event },
    /// The newly written version.
    RolledBack { file: MemoryFile },
    /// The records carrying the requested ID.
    Trace { trace: TraceInspection },
    /// Deterministic findings on the project's current notes.
    MemoryCheck { findings: Vec<MemoryFinding> },
}

/// Latency in milliseconds across all matching historical records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Latency {
    /// Number of observations.
    pub count: i64,
    /// Arithmetic mean, absent for an empty sample.
    pub mean_ms: Option<f64>,
    /// Maximum, absent for an empty sample.
    pub max_ms: Option<i64>,
}

/// A requested snapshot of the hub's operational metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationsMetrics {
    /// Time used for ages, cooldowns, and the rolling day.
    pub at: Timestamp,
    /// Age of the oldest pending event per lane with pending work.
    #[serde(deserialize_with = "deserialize_lane_lag")]
    pub lane_lag_ms: BTreeMap<LaneId, i64>,
    /// Pending events across all lanes of each included project, including zeros.
    pub queue_depth: BTreeMap<String, i64>,
    /// Time from start to end for every ended run.
    pub run_duration: Latency,
    /// Accounts currently cooling down, with their reset times.
    pub account_cooldowns: BTreeMap<String, Timestamp>,
    /// Space-wide elapsed time from recorded account-blocked notices to
    /// their matching resumed notices, or to the snapshot for an open interval.
    pub account_wait_ms: i64,
    /// Events currently dead.
    pub dead_events: i64,
    /// Time from due to fired for each recorded timer occurrence.
    pub timer_lateness: Latency,
    /// Age of the oldest pending or retrying outbox row, absent if none.
    pub outbox_lag_ms: Option<i64>,
    /// Ordinary outbox messages delivered per UTC date (YYYY-MM-DD).
    pub messages_per_day: BTreeMap<String, i64>,
    /// Silent committed runs divided by all committed runs, absent if none.
    pub silent_run_ratio: Option<f64>,
    /// Decisions created in the rolling 24 hours up to the snapshot.
    pub decisions_per_day: i64,
    /// Time from creation to answer for decisions with status answered.
    pub answer_latency: Latency,
    /// Standing rules ever created, including revoked rules.
    pub standing_rules_created: i64,
}

fn deserialize_lane_lag<'de, D>(deserializer: D) -> Result<BTreeMap<LaneId, i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Internally tagged replies buffer JSON object keys as strings.
    BTreeMap::<String, i64>::deserialize(deserializer)?
        .into_iter()
        .map(|(id, lag)| {
            id.parse::<i64>()
                .map(|id| (LaneId(id), lag))
                .map_err(serde::de::Error::custom)
        })
        .collect()
}

/// Operational records for a lane, oldest first by row ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneInspection {
    /// Lane record, including its lease owner, expiry and epoch.
    pub lane: crate::model::Lane,
    /// All accepted events and their current state.
    pub events: Vec<crate::model::Event>,
    /// All sessions in the lane.
    pub sessions: Vec<crate::model::Session>,
    /// All runs in the lane.
    pub runs: Vec<Run>,
    /// Decisions still waiting for an answer.
    pub open_decisions: Vec<Decision>,
    /// Timers belonging to the lane.
    pub timers: Vec<Timer>,
}

/// One stored tool call in a trace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TracedToolCall {
    /// The run that started the call.
    pub run_id: RunId,
    /// Idempotency key within that run.
    pub key: String,
    /// Name of the system tool.
    pub tool: String,
    /// The arguments supplied to the tool.
    pub arguments: serde_json::Value,
    /// Whether the call has finished.
    pub status: crate::model::ToolCallStatus,
    /// Recorded result, when available.
    pub result: Option<serde_json::Value>,
    /// When the call started.
    pub started_at: Timestamp,
    /// When its result was recorded.
    pub finished_at: Option<Timestamp>,
    /// Trace inherited from the run.
    pub trace_id: String,
}

/// All records carrying a trace ID, oldest first by row ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceInspection {
    /// The requested trace ID.
    pub id: String,
    /// Events that originated this trace.
    pub events: Vec<crate::model::Event>,
    /// Runs that inherited this trace.
    pub runs: Vec<Run>,
    /// Calls made by those runs.
    pub tool_calls: Vec<TracedToolCall>,
    /// Decisions opened by those runs.
    pub decisions: Vec<Decision>,
    /// Outbox rows attributed to those runs.
    pub outbox: Vec<OutboxMessage>,
}

/// Project dreaming state, including the last completed pass when one exists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamingStatus {
    /// The active cadence. `None` means dreaming is off.
    pub cadence: Option<Recurrence>,
    /// The last completed dream, retained when dreaming is turned off.
    pub last: Option<DreamSummary>,
}

/// The result of a committed dream run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamSummary {
    /// When the run committed.
    pub at: Timestamp,
    /// Versions written by the dream, including deletions.
    pub changes: Vec<DreamChange>,
}

/// A project memory version written by a dream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamChange {
    /// Project-relative memory path prefixed with `project/`.
    pub path: String,
    /// Version written by the dream.
    pub version: i64,
    /// Whether this version removed the file.
    pub deleted: bool,
}

/// One duplicate pair or dangling reference in project notes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryFinding {
    /// Two notes with identical normalized bodies.
    ExactDuplicate { first: String, second: String },
    /// Two notes with very similar normalized bodies.
    NearDuplicate { first: String, second: String },
    /// A reference to a task absent from this project.
    MissingTask { note: String, task: TaskId },
    /// A reference to a file absent from its known location.
    MissingFile { note: String, target: String },
}

impl fmt::Display for MemoryFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExactDuplicate { first, second } => {
                write!(f, "Duplicate notes: {first} and {second}")
            }
            Self::NearDuplicate { first, second } => {
                write!(f, "Similar notes: {first} and {second}")
            }
            Self::MissingTask { note, task } => write!(f, "{note} refers to missing task {task}"),
            Self::MissingFile { note, target } => {
                write!(f, "{note} refers to missing file {target}")
            }
        }
    }
}
