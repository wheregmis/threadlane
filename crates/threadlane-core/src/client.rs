//! What a hub offers its clients (Plan.md §11.2).
//!
//! [`HubApi`] describes one space and has no notion of any other. The hub
//! implements it; a transport carries it to clients as [`ClientRequest`] and
//! [`ClientResponse`]. Later milestones add capabilities; so far: who am I
//! talking to, send a message, follow the feed, the organization, decisions,
//! pairing, the overview the app's screens show, and memory.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::admission::AdmittedKey;
use crate::ids::{DecisionId, EventId, ProjectId, TaskId, TimerId};
use crate::model::{
    Account, Agent, Decision, Machine, MemoryFile, Mode, OutboxMessage, Recurrence, Repository,
    Run, Task, Timer,
};
use crate::time::Timestamp;

/// What a hub says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubInfo {
    /// The space's name, for example `Work`.
    pub space: String,
    pub mode_ceiling: Mode,
    pub schema_version: i64,
}

/// A project as a client lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub name: String,
    pub is_general: bool,
    pub paused: bool,
}

/// A message from you to a project's lead, or to one of its tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessage {
    /// The project's name. `None` means the General project.
    pub project: Option<String>,
    pub text: String,
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
}

/// An agent's binding to a project as a client lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingSummary {
    /// The agent's name.
    pub agent: String,
    pub role: String,
    pub mode: Mode,
    pub is_lead: bool,
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
    Project { project: ProjectSummary },
    Repository { repository: Repository },
    Repositories { repositories: Vec<Repository> },
    Agents { agents: Vec<Agent> },
    Agent { agent: Agent },
    Bindings { bindings: Vec<BindingSummary> },
    Binding { binding: BindingSummary },
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
    /// When its active run started; `None` while no run is active.
    pub running_since: Option<Timestamp>,
}

/// One run of a task, with the name of the machine it ran on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub run: Run,
    pub machine: String,
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
    /// Every decision asked in its lane, oldest first.
    pub decisions: Vec<Decision>,
    /// The tasks it depends on and the tasks that depend on it.
    pub related: Vec<TaskSummary>,
    /// Your messages to its lane, oldest first: only
    /// [`ConversationEntry::You`] entries.
    #[serde(default)]
    pub messages: Vec<ConversationEntry>,
}

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
        at: Timestamp,
    },
    /// A delivered message, decision card, or notice, as the feed has it.
    Delivered { item: FeedItem },
}

/// The Overview capability (Plan.md §11.2 "Tasks", "Machines"): what the
/// app's screens show. Read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OverviewRequest {
    /// Every task of a project, oldest first. `None` means the General
    /// project.
    Tasks { project: Option<String> },
    /// Every running task in the space, oldest first.
    Running,
    /// One task in full.
    Task { task: TaskId },
    /// The newest `limit` entries of the conversation with a project's lead
    /// before `before` (the newest of all when `None`), oldest first. `None`
    /// as the project means the General project.
    Conversation {
        project: Option<String>,
        before: Option<Timestamp>,
        limit: u32,
    },
    /// The newest `limit` delivered items in the space, of any project or
    /// none, oldest first.
    Recent { limit: u32 },
    /// The machines that joined the space, and its model accounts.
    Machines,
    /// Whether a project's lead is working: when the run in progress on its
    /// lead lane started. `None` as the project means the General project.
    Lead { project: Option<String> },
}

/// The answer to an [`OverviewRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OverviewReply {
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
    },
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
}

/// The answer to a [`MemoryRequest`]. Every file's `path` is as agents
/// name it, and `written_by_run` is `None` for a version you wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemoryReply {
    Files { files: Vec<MemoryFile> },
    File { file: MemoryFile },
    History { versions: Vec<MemoryFile> },
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
    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError>;
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
}

/// [`HubApi`] as it crosses a transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClientRequest {
    Info,
    Projects,
    SendMessage { message: UserMessage },
    Feed { after: i64, wait_ms: u64 },
    Org { request: OrgRequest },
    Decisions { request: DecisionRequest },
    Pairing { request: PairingRequest },
    Overview { request: OverviewRequest },
    Timers { request: TimerRequest },
    Memory { request: MemoryRequest },
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
}

/// Answers one request from an API. Shared by every transport's server side.
pub async fn dispatch(api: &dyn HubApi, request: ClientRequest) -> ClientResponse {
    let result = match request {
        ClientRequest::Info => api.info().await.map(|info| ClientResponse::Info { info }),
        ClientRequest::Projects => api
            .projects()
            .await
            .map(|projects| ClientResponse::Projects { projects }),
        ClientRequest::SendMessage { message } => api
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
    };
    result.unwrap_or_else(|error| ClientResponse::Error { error })
}

/// The Timers capability (Plan.md §7, docs/milestone-8.md §2): follow-ups
/// and routines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimerRequest {
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
