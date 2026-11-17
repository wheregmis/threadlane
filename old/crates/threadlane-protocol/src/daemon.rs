//! Daemon wire contract (issue #79): every type that crosses between the
//! daemon-owned session core and thin clients.
//!
//! Everything in this module is plain data — `Serialize + Deserialize`,
//! ID- or path-referenced, free of process handles — so a client can speak
//! the contract over a channel today (the GPUI app embeds `threadlane-daemon`
//! in-process) or over a socket tomorrow (`threadlane-daemon` the process).
//!
//! Surface map:
//! - [`SessionCommand`]: every request a client can issue. Commands that need
//!   a live session address it by `session_id`; the daemon resolves the
//!   owning runtime internally (handles never cross the wire).
//! - [`SessionEvent`]: the live event stream. `Agent` wraps the shared
//!   [`AgentEvent`] vocabulary (turn lifecycle, message deltas, tool calls,
//!   permission/question requests, subagent and fusion updates); the rest are
//!   daemon-level events for worktree setup, titles, scheduled results, ACP
//!   config options, and session/project deltas.
//! - [`SessionSnapshot`] / [`ProjectInfo`]: attach-mid-run semantics — a
//!   client that connects or selects a session gets one snapshot and then
//!   tails live `SessionEvent`s.
//! - [`CommandRequest`] / [`CommandReply`] / [`CommandResponse`]: the
//!   optional request/reply pair for commands that must return a payload
//!   (e.g. the staged content of a cancelled queued message). Replies are
//!   point-to-point on the requesting connection — never journaled or
//!   broadcast, so a `?since=` replay never resends them.
//!
//! Session files stay `PathBuf`s on the wire: the daemon owns the filesystem
//! and paths are the canonical identity handle. Consumers must not assume
//! the file is readable from their own process; `session_id` is the key.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::acp::AcpConfigOption;
use crate::events::{AgentEvent, SubagentIsolation};
use crate::interaction::QuestionAnswer;
use crate::messages::{ImageAttachment, ReasoningEffort, RetryPrompt, SessionPlan, TokenUsage};
use crate::orchestration::{ModelRoles, OrchestratorMode};
use crate::repo::{GitOperation, GitResponse, ProjectFileNode};

/// Daemon wire protocol version, announced by the server in the
/// [`PROTOCOL_VERSION_HEADER`] WebSocket handshake response header.
///
/// Version 1 is the pre-announcement wire: bare `SessionCommand` frames
/// inbound, `{"seq", "event"}` frames outbound. A daemon that sends no
/// header is version 1 — it cannot decode a [`CommandRequest`] envelope
/// (it rejects the frame as an undecodable command and never dispatches
/// it), so clients must gate request/reply calls on
/// [`COMMAND_REQUEST_PROTOCOL_VERSION`]. Version 2 adds the
/// `CommandRequest`/`CommandReply` pair and the journaled
/// [`SessionEvent::QueuedEntryCancelled`]. Version 3 adds the project-io
/// surface — `ListProjectFiles`/`ReadProjectFile`/`WriteProjectFile`/
/// `ProjectFileExists`/`GitRequest`/`WatchProject`/`UnwatchProject`/
/// `GetWorktreeBases` commands, their `CommandResponse` payloads, and the
/// ephemeral [`SessionEvent::WorkspaceChanged`]. Version 4 adds draft creation
/// and project-scoped composer options. Version 5 adds the GitHub (forge)
/// and automation surfaces — `GitHubRequest`/`AutomationRequest` commands
/// and the journaled [`SessionEvent::AutomationChanged`]. Version 6 adds
/// ephemeral saved-file search and daemon-host target validation. Version 7
/// adds versioned file reads and guarded file writes. Version 8 adds
/// ephemeral session-scoped language services for unsaved editor buffers.
pub const WIRE_PROTOCOL_VERSION: u64 = 8;
/// Session-scoped language services for unsaved editor buffers.
pub const EDITOR_LSP_PROTOCOL_VERSION: u64 = 8;
/// Saved-file content search and host-validated navigation.
pub const FILE_SEARCH_PROTOCOL_VERSION: u64 = 6;
/// Versioned file reads and writes guarded by a previously read version.
pub const GUARDED_SAVE_PROTOCOL_VERSION: u64 = 7;

/// Draft creation and composer catalog requests.
pub const COMPOSER_PROTOCOL_VERSION: u64 = 4;

/// Wire floor for the GitHub (forge) and automation surfaces:
/// `GitHubRequest`/`AutomationRequest` commands, their `CommandResponse`
/// payloads, and [`SessionEvent::AutomationChanged`]. Gate on this constant,
/// not on [`WIRE_PROTOCOL_VERSION`] itself: when this build's version
/// advances past it, older-but-still-capable daemons must keep qualifying.
pub const GITHUB_AUTOMATION_PROTOCOL_VERSION: u64 = 5;

/// The lowest protocol version able to serve project filesystem and Git
/// requests on the daemon's host. Gate file/git calls on this constant, not
/// on [`WIRE_PROTOCOL_VERSION`] itself: when this build's version advances
/// past 3, older-but-still-capable daemons must keep qualifying.
pub const PROJECT_IO_PROTOCOL_VERSION: u64 = 3;

/// The lowest protocol version able to decode a [`CommandRequest`]
/// envelope. Gate request/reply calls on this constant, not on
/// [`WIRE_PROTOCOL_VERSION`] itself: when this build's version advances
/// past 2, older-but-still-capable daemons must keep qualifying.
pub const COMMAND_REQUEST_PROTOCOL_VERSION: u64 = 2;

/// The handshake response header carrying [`WIRE_PROTOCOL_VERSION`].
/// Absent on pre-2 daemons, which is exactly how a client learns it is
/// talking to one — the answer arrives with the socket upgrade, before
/// any command frame is risked.
pub const PROTOCOL_VERSION_HEADER: &str = "x-threadlane-protocol";

/// A command a client sends to the daemon.
///
/// Every variant is accepted in any session state; the daemon is the one
/// place that knows whether a runtime exists, is generating, or must be
/// constructed first, so commands never carry runtime handles.
// No `PartialEq`: `WorktreeSetup::cancelled` is a process-local flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionCommand {
    /// Begin a draft in an attached project; persisted by the first prompt.
    BeginSession { work_dir: PathBuf },
    /// Desktop's configured model inventory and composer defaults.
    GetComposerOptions {
        work_dir: PathBuf,
        session_id: Option<String>,
    },
    /// Submit a user prompt. `work_dir` is the effective execution directory
    /// (the worktree for worktree sessions); `acp_config` holds pending ACP
    /// agent settings to apply before the first turn. `model` re-seeds the
    /// daemon's current selection before the runtime is built — required on
    /// the first prompt of a daemon that has not seen a `SetModel` yet.
    /// `effort` likewise re-seeds the daemon's current effort; `None` keeps
    /// the selection already held (e.g. from a `SetReasoningEffort`).
    SubmitPrompt {
        session_id: String,
        work_dir: PathBuf,
        text: String,
        #[serde(default)]
        images: Vec<ImageAttachment>,
        #[serde(default)]
        effort: Option<ReasoningEffort>,
        #[serde(default)]
        acp_config: Vec<(String, String)>,
        #[serde(default)]
        model: Option<String>,
    },
    /// Cancel the session's in-flight turn.
    CancelRun { session_id: String },
    /// Resolve a permission prompt raised by an `AgentEvent::PermissionRequested`.
    AnswerPermission {
        session_id: String,
        request_id: String,
        decision: PermissionDecision,
    },
    /// Resolve (or dismiss) a question raised by `AgentEvent::QuestionRequested`.
    AnswerQuestion {
        session_id: String,
        answer: QuestionAnswer,
    },
    /// Switch the session's model. The daemon persists the selection and
    /// rebuilds the runtime so provider credentials re-resolve.
    SetModel { session_id: String, model: String },
    /// Switch reasoning effort; applies to the next turn when a run is live.
    SetReasoningEffort {
        session_id: String,
        effort: ReasoningEffort,
    },
    /// Replace the session's model-role routing (fast model, fallback chain).
    SetModelRoles { session_id: String, roles: ModelRoles },
    /// Switch the orchestration mode (Agent vs Fusion) for the session's
    /// project and rebuild the live runtime so the next turn routes anew.
    SetOrchestratorMode {
        session_id: String,
        mode: OrchestratorMode,
    },
    /// Ask a session's external ACP agent what settings it offers. Starting
    /// the agent is the point: it reports settings on `session/new`, so this
    /// is how the picker learns the option set before the first turn.
    LoadAcpConfigOptions { session_id: String },
    /// Apply one of the agent's own settings; the refreshed set arrives as
    /// `SessionEvent::AcpConfigOptions`.
    SetAcpConfigOption {
        session_id: String,
        config_id: String,
        value: String,
    },
    /// Hydrate a session: project its durable transcript (and optionally
    /// rebuild a live runtime) so the client can render it.
    HydrateSession { request: SessionHydrationRequest },
    /// Prepare a worktree session: name the branch, create the checkout,
    /// construct the runtime, then submit `setup.text` as the first turn.
    PrepareWorktree { setup: WorktreeSetup },
    /// Cancel an in-flight worktree preparation.
    CancelWorktreeSetup { session_id: String },
    /// Remove a session (transcript and runtime). `delete_worktree` also
    /// removes a session-owned checkout that has no unrecorded work.
    DeleteSession {
        session_id: String,
        session_file: PathBuf,
        #[serde(default)]
        delete_worktree: bool,
    },
    /// Attach a project directory to the workspace.
    AddProject { work_dir: PathBuf },
    /// Detach a project directory.
    RemoveProject { work_dir: PathBuf },
    /// Refresh the provider/model catalog (live discovery merged into the
    /// picker). `work_dir` scopes project-level model overrides.
    RefreshCatalog { work_dir: Option<PathBuf> },
    /// Open a daemon-hosted PTY: an interactive shell in `cwd` at
    /// `cols`×`rows`. `terminal_id` is client-chosen and unique per spawn;
    /// output, resize notices, and exit lifecycle stream back as
    /// `SessionEvent::TerminalEvent` frames carrying the same id.
    TerminalOpen {
        terminal_id: String,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    },
    /// Kill and release a daemon-hosted PTY.
    TerminalClose { terminal_id: String },
    /// Forward keyboard input to a daemon-owned PTY.
    TerminalInput { terminal_id: String, data: String },
    /// Resize a daemon-owned PTY.
    TerminalResize {
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    /// Steer the session's live turn: the message reaches the model during
    /// the current turn instead of queueing behind it.
    SteerMessage {
        session_id: String,
        text: String,
        #[serde(default)]
        images: Vec<ImageAttachment>,
    },
    /// Re-route a still-pending queued follow-up into the live steer queue
    /// so it reaches the model during the current turn instead of after it.
    SteerQueuedMessage { session_id: String, entry_id: String },
    /// Drop a still-pending queued input. Sent inside a [`CommandRequest`]
    /// the reply carries the entry's staged text and images as
    /// `CommandResponse::CancelledQueuedMessage`. `work_dir` pins the
    /// project the session belongs to: when present the daemon resolves
    /// the runtime only inside that project, so a same-named session in
    /// another project cannot be cancelled in its place.
    CancelQueuedMessage {
        session_id: String,
        entry_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        work_dir: Option<PathBuf>,
    },
    /// Request the full attached-project list; answered by one
    /// `SessionEvent::ProjectChanged` per attached project. A freshly
    /// attached thin client sends this instead of relying on the bounded
    /// journal still holding the original attach events.
    GetProjects,
    /// Request a project snapshot; answered by `SessionEvent::ProjectChanged`.
    GetProjectState { work_dir: PathBuf },
    /// Request a session snapshot; answered by `SessionEvent::SessionSnapshot`.
    GetSessionSnapshot { session_id: String },
    /// List the project's file tree on the daemon's filesystem; answered by
    /// `CommandResponse::ProjectFiles`.
    ListProjectFiles { work_dir: PathBuf, limit: usize },
    /// Ephemeral read-only search; never journal the query or results.
    SearchProjectFiles { work_dir: PathBuf, query: String },
    /// Ephemeral language-server query; never journal unsaved buffer contents.
    EditorLsp { request: crate::editor_lsp::EditorLspRequest },
    ValidateSearchTarget { work_dir: PathBuf, path: String },
    /// Read one project file as UTF-8 text; answered by
    /// `CommandResponse::FileContent`.
    ReadProjectFile { work_dir: PathBuf, path: String },
    /// Read one existing project file as UTF-8 text with its content hash;
    /// answered by `CommandResponse::VersionedFile`.
    ReadProjectFileVersioned { work_dir: PathBuf, path: String },
    /// Write one project file as UTF-8 text; answered by `Ack`/`Err`.
    WriteProjectFile {
        work_dir: PathBuf,
        path: String,
        content: String,
    },
    /// Write an existing project file only if its current content hash
    /// matches `expected_version`; answered by `CommandResponse::GuardedFileWrite`.
    WriteProjectFileGuarded {
        work_dir: PathBuf,
        path: String,
        content: String,
        expected_version: String,
    },
    /// Whether `path` names an existing entry under `work_dir`; answered by
    /// `CommandResponse::FileExists`.
    ProjectFileExists { work_dir: PathBuf, path: String },
    /// One Git repository operation — query or mutation — against the
    /// checkout at `work_dir`; answered by `CommandResponse::Git`.
    GitRequest {
        work_dir: PathBuf,
        operation: GitOperation,
    },
    /// Subscribe this client to `SessionEvent::WorkspaceChanged` for the
    /// project's filesystem. Watchers are refcounted — a second `Watch`
    /// for the same root just bumps the count; answered by `Ack`.
    WatchProject { work_dir: PathBuf },
    /// Release one `WatchProject` subscription; answered by `Ack`.
    UnwatchProject { work_dir: PathBuf },
    /// Answer `SessionEvent::WorktreeBases` with the project's base
    /// branches for a new-task worktree picker; answered by `Ack`.
    GetWorktreeBases { work_dir: PathBuf },
    /// One GitHub (forge) operation — issue/pull-request reads and
    /// mutations — against the checkout at `work_dir`; answered by
    /// `CommandResponse::GitHub`. Wire capability
    /// [`GITHUB_AUTOMATION_PROTOCOL_VERSION`].
    GitHubRequest {
        work_dir: PathBuf,
        operation: crate::repo::GitHubOperation,
    },
    /// One automation-store operation; answered by
    /// `CommandResponse::Automation` carrying the post-command projection.
    /// Wire capability [`GITHUB_AUTOMATION_PROTOCOL_VERSION`].
    AutomationRequest {
        command: crate::automation::AutomationCommand,
    },
}

/// An event the daemon broadcasts to clients.
///
/// Ordering follows the producing session's journal: events for one
/// `session_id` arrive in emission order; cross-session ordering is not
/// guaranteed beyond causality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// A turn-level event from the session's agent. [`AgentEvent`] carries
    /// the full vocabulary: start/end, message and reasoning deltas, tool
    /// calls, permission/question requests, subagent and fusion updates.
    Agent {
        session_id: String,
        event: AgentEvent,
    },
    /// The session's generation finished (completed, errored, or cancelled).
    Finished {
        session_id: String,
        session_file: PathBuf,
    },
    /// A scheduled (non-interactive) run of the session settled.
    /// `result` is `None` when the work produced no summary to surface.
    Scheduled {
        session_id: String,
        session_file: PathBuf,
        result: Option<Result<String, String>>,
    },
    /// The session's automatic title was generated and persisted.
    TitleGenerated {
        session_id: String,
        session_file: PathBuf,
    },
    /// Available base branches for a new worktree task in `project`.
    WorktreeBases {
        project: PathBuf,
        result: Result<(String, Vec<String>), String>,
    },
    /// Progress of an in-flight worktree preparation.
    WorktreeProgress {
        session_id: String,
        stage: SetupStage,
        branch: Option<String>,
    },
    /// Worktree preparation settled. On success the daemon has already
    /// registered the new runtime; `session` is the discovered metadata.
    WorktreePrepared {
        session_id: String,
        result: Result<SessionInfo, String>,
    },
    /// Settings an external ACP agent exposes, as it reports them.
    ///
    /// `runtime_instance` identifies the daemon-side runtime generation the
    /// options came from — a client must apply them only while that instance
    /// is still the registered runtime for `session_file` (the check that
    /// used to ride a `Weak<SessionRuntime>` in-process).
    AcpConfigOptions {
        session_id: String,
        session_file: PathBuf,
        runtime_instance: u64,
        options: Vec<AcpConfigOption>,
        error: Option<String>,
        /// Restores a New-task picker selection when applying it failed.
        failed_config: Option<(String, String)>,
    },
    /// Output or lifecycle event from a daemon-hosted PTY. Routing is by
    /// the `terminal_id` inside the event — terminals belong to the host,
    /// not to a session.
    TerminalEvent { event: TerminalEvent },
    /// `SubmitPrompt` landed mid-turn and the daemon queued the text as a
    /// follow-up: `entry_id` is the durable queue entry a remote client
    /// binds to its optimistic `queued-user-{session}` echo so the row's
    /// steer/edit/remove controls become usable.
    FollowUpQueued { session_id: String, entry_id: String },
    /// A project snapshot or delta. Sent on attach and whenever the project's
    /// session list changes (new session, title update, health transition).
    ProjectChanged { project: ProjectInfo },
    /// Attach-mid-run snapshot: the session's durable projection as of the
    /// snapshot point. Live `Agent`/`Finished`/… events continue after it;
    /// the client renders snapshot + tail.
    SessionSnapshot {
        session_id: String,
        snapshot: Box<SessionSnapshot>,
    },
    /// A daemon-level error not attributable to an agent event.
    DaemonError {
        session_id: Option<String>,
        message: String,
    },
    /// Acknowledgement that `DeleteSession` completed: the transcript is
    /// archived or removed and the runtime dropped. Clients drop their
    /// local bookkeeping for the session only now — a rejected delete
    /// arrives as `DaemonError` instead, and the session row returns.
    SessionRemoved {
        session_id: String,
        session_file: PathBuf,
    },
    /// A reply to a [`CommandRequest`] this client issued. Client
    /// transports synthesize it for the requester — the daemon answers
    /// requests point-to-point on the requesting connection, never
    /// through the journal broadcast — so it never appears in a replayed
    /// tail.
    CommandResult {
        request_id: u64,
        result: Result<CommandResponse, String>,
    },
    /// `CancelQueuedMessage` dropped the entry, carrying the same staged
    /// payload a `CommandReply` would — journaled so a requester whose
    /// reply was lost to a disconnect still resolves its request from the
    /// replayed tail (`request_id` correlates it; `None` for
    /// fire-and-forget cancels) and every client learns the entry is gone.
    QueuedEntryCancelled {
        session_id: String,
        entry_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<u64>,
        text: String,
        #[serde(default)]
        images: Vec<ImageAttachment>,
    },
    /// A watched project's filesystem changed. Ephemeral — never
    /// journaled, like `TerminalEvent` output: each client refreshes from a
    /// fresh `ListProjectFiles`/`GitRequest` rather than replayed state.
    /// `git_dirty` means the `.git` index or refs changed (refresh status
    /// surfaces); `files_dirty` means working-tree content changed
    /// (refresh the file tree).
    WorkspaceChanged {
        work_dir: PathBuf,
        git_dirty: bool,
        files_dirty: bool,
    },
    /// The automation store's projection changed — published by the
    /// daemon's automation bridge whenever the service's watch channel
    /// updates. Journaled, so an attach/reconnect replays the latest known
    /// state. Wire capability [`GITHUB_AUTOMATION_PROTOCOL_VERSION`].
    AutomationChanged {
        projection: crate::automation::AutomationProjection,
    },
}

/// A `SessionCommand` that expects a reply, sent as a
/// `{"request_id": N, "command": {...}}` frame instead of a bare command.
/// The daemon answers on the same connection with a `{"response": ...}`
/// [`CommandReply`] — point-to-point, never journaled or broadcast.
/// `request_id` is caller-chosen, unique per connection, and correlates
/// the reply.
// No `PartialEq`: `SessionCommand` has none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRequest {
    pub request_id: u64,
    pub command: SessionCommand,
}

/// Payload a [`CommandRequest`] resolves to. `Ack` is the reply for
/// commands that carry no return value — the payload-carrying variants
/// are the point of the request/reply channel. Only a peer advertising
/// a protocol version of at least [`COMMAND_REQUEST_PROTOCOL_VERSION`]
/// can decode the envelope; on a version-1 daemon the frame is rejected
/// as an undecodable command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandResponse {
    EditorLsp { result: Result<crate::editor_lsp::EditorLspResponse, String> },
    SessionDraft {
        session: SessionInfo,
    },
    ComposerOptions {
        models: Vec<ComposerModel>,
        model: String,
        effort: ReasoningEffort,
        mode: OrchestratorMode,
    },
    /// Completed; no payload.
    Ack,
    /// `CancelQueuedMessage` dropped the entry: its staged content so the
    /// requesting client can restore text and images into the composer —
    /// a bare `CancelQueuedMessage` command cannot deliver them back.
    CancelledQueuedMessage {
        session_id: String,
        entry_id: String,
        text: String,
        #[serde(default)]
        images: Vec<ImageAttachment>,
    },
    /// `ListProjectFiles` result: the project tree up to `limit` entries.
    ProjectFiles { nodes: Vec<ProjectFileNode> },
    FileSearch { result: crate::repo::FileSearchResult },
    /// `ReadProjectFile` result: the file's UTF-8 content.
    FileContent { content: String },
    /// `ReadProjectFileVersioned` result, including typed filesystem errors.
    VersionedFile {
        result: Result<VersionedFile, ProjectFileError>,
    },
    /// `WriteProjectFileGuarded` result: the new content hash on success.
    GuardedFileWrite {
        result: Result<String, ProjectFileError>,
    },
    /// `ProjectFileExists` result.
    FileExists { exists: bool },
    /// `GitRequest` result: the operation's payload.
    Git { response: GitResponse },
    /// `GitHubRequest` result: the operation's payload.
    GitHub {
        response: crate::repo::GitHubResponse,
    },
    /// `AutomationRequest` result: the post-command projection.
    Automation {
        response: crate::automation::AutomationResponse,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionedFile {
    pub content: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectFileError {
    Changed,
    Deleted,
    Io { message: String },
    Unsupported,
    Disconnected,
}

impl std::fmt::Display for ProjectFileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Changed => formatter.write_str("file changed since it was read"),
            Self::Deleted => formatter.write_str("file was deleted"),
            Self::Io { message } => formatter.write_str(message),
            Self::Unsupported => formatter.write_str("guarded file saves are unsupported"),
            Self::Disconnected => formatter.write_str("daemon is disconnected"),
        }
    }
}

impl std::error::Error for ProjectFileError {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComposerModel {
    pub id: String,
    pub label: String,
    pub efforts: Vec<ReasoningEffort>,
}

/// The daemon's reply to a [`CommandRequest`]: one `{"response": ...}`
/// frame on the requesting connection only — never journaled, so a
/// `?since=` replay never re-delivers it. A disconnect can therefore eat
/// the reply to an already-executed command; payload commands pair the
/// reply with a journaled [`SessionEvent::QueuedEntryCancelled`] the
/// requester recovers from on reconnect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandReply {
    pub request_id: u64,
    pub result: Result<CommandResponse, String>,
}

/// A permission decision the client sends back to the daemon.
///
/// Canonical definition; `threadlane_permission` re-exports it so the
/// command surface and the permission manager share one type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    AllowOnce,
    /// In-memory grant for the rest of the session (never persisted).
    AllowSession,
    AllowAlways,
    Deny,
}

/// Terminal output or lifecycle event streamed from a daemon-owned PTY.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalEvent {
    Output { terminal_id: String, data: String },
    Resized {
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    Exited {
        terminal_id: String,
        exit_code: Option<i32>,
    },
    /// A `Terminal*` command failed before producing output (e.g. the
    /// requested cwd or the shell does not exist). Scoped to the terminal
    /// so only the owning view learns about it.
    Failed {
        terminal_id: String,
        message: String,
    },
}

/// Stages of first-send worktree preparation, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SetupStage {
    Naming,
    Creating,
    Starting,
}

impl SetupStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Naming => "Naming the worktree",
            Self::Creating => "Creating the worktree",
            Self::Starting => "Starting the session",
        }
    }
}

/// A durable first-send worktree preparation. Persisted into the session
/// metadata stub (`worktree_setup` fact) so an interrupted setup can be
/// retried or recovered; also the `PrepareWorktree` command payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeSetup {
    pub project: PathBuf,
    pub session_id: String,
    pub session_file: PathBuf,
    pub worktree: PathBuf,
    pub base: String,
    pub stage: SetupStage,
    pub branch: Option<String>,
    pub error: Option<String>,
    /// Client-side cancel flag: meaningful only in-process, never serialized.
    #[serde(skip)]
    pub cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub text: String,
    pub images: Vec<ImageAttachment>,
    pub model: String,
    pub effort: ReasoningEffort,
    pub acp_config: Vec<(String, String)>,
}

/// Runtime construction inputs for [`SessionHydrationRequest`].
///
/// Carries identity and configuration only — the daemon resolves ancillary
/// resources (browser bridge, provider credentials, subagent settings)
/// internally when it builds the runtime.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HydrationRuntimeOptions {
    /// Effective worktree directory for agent execution.
    pub work_dir: PathBuf,
    pub model: String,
    pub model_roles: ModelRoles,
}

/// A session whose durable projections a client wants computed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionHydrationRequest {
    pub session_id: String,
    pub session_file: PathBuf,
    pub reload_messages: bool,
    /// When present, the daemon also (re)builds the session's runtime.
    pub runtime_options: Option<HydrationRuntimeOptions>,
}

/// Point-in-time snapshot of one session, served on attach so a client can
/// render without waiting for new events.
///
/// Session-diagnostics and token-efficiency projections stay daemon-local
/// for now (they carry engine-owned types a remote client does not need);
/// extend this struct when a remote diagnostics surface exists.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session: SessionInfo,
    pub messages: Vec<ChatMessageInfo>,
    pub trajectory: Vec<TrajectoryEntry>,
    pub subagents: Vec<SubagentActivityInfo>,
    pub plan: SessionPlan,
    pub metrics: SessionMetricsInfo,
    pub token_usage: TokenUsage,
    pub context_window: Option<ContextWindowInfo>,
    pub run_timing: Option<RunTiming>,
}

/// Reference to a GitHub issue attached to a session or project view.
///
/// Moved verbatim from `threadlane-git` (which re-exports it) so the session
/// list contract stays inside the protocol crate.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubIssueRef {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionHealth {
    #[default]
    Healthy,
    Working,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionAttention {
    NeedsYou,
    Working,
    Ready,
    Idle,
}

impl SessionAttention {
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Ready => "Ready",
            Self::Idle => "Idle",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WorkMode {
    #[default]
    Local,
    Worktree,
}

impl WorkMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Worktree => "Worktree",
        }
    }
}

/// Identity of one successful main-lane Run completion in a session journal:
/// the `OperationFinished` record id, the run it closed, and its journal seq.
/// Persisted inside `session_seen.json`, so the shape and field names are a
/// stable on-disk contract.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunCompletionToken {
    pub record_id: String,
    pub run_id: String,
    pub seq: u64,
}

/// What discovery could prove about a session's latest successful main-lane
/// Run completion. `Unknown` deliberately stays distinct from `None`: an
/// unreadable stub must never be baselined as acknowledged, while a parsed
/// transcript with no qualifying completion confirms there is nothing to mark.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionCompletionSummary {
    #[default]
    Unknown,
    None,
    Latest(RunCompletionToken),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    /// Canonical attached project that owns this session file.
    pub work_dir: PathBuf,
    /// Effective directory used for agent execution.
    pub runtime_work_dir: PathBuf,
    pub session_file: PathBuf,
    pub updated_at: u64,
    pub health: SessionHealth,
    pub git_branch: Option<String>,
    pub github_issue: Option<GitHubIssueRef>,
    pub is_worktree: bool,
    pub worktree_available: bool,
    pub completion_summary: SessionCompletionSummary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub work_dir: PathBuf,
    pub sessions: Vec<SessionInfo>,
    pub is_expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Error,
    ContextMarker,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolActivityInfo {
    pub id: String,
    pub category: String,
    pub title: String,
    pub display_summary: String,
    pub detail: String,
    /// Raw tool-call arguments JSON, kept after `detail` is replaced by the
    /// tool result so the chat surface can render the call itself.
    #[serde(default)]
    pub arguments: String,
    pub is_expanded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryDiagnostics {
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
    pub model_visible: bool,
    pub source: Option<String>,
    pub raw: Option<String>,
    pub parent_id: Option<String>,
    pub result_id: Option<String>,
    pub exit_code: Option<i32>,
    pub output_bytes: Option<u64>,
    pub files_mutated: Vec<String>,
    pub commands_executed: Vec<String>,
    pub error_summary: Option<String>,
    pub items_count: Option<usize>,
    pub token_estimate: Option<u32>,
    pub is_anomaly: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryEntry {
    pub seq: Option<u64>,
    pub run_id: Option<String>,
    pub turn: Option<u32>,
    /// The user-facing request this entry belongs to, when it can be inferred
    /// from the canonical transcript. Runtime records inherit the active request.
    pub request: Option<u32>,
    pub category: String,
    pub summary: String,
    pub detail: String,
    pub lane: Option<String>,
    pub correlation_id: Option<String>,
    pub diagnostics: TrajectoryDiagnostics,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetricsInfo {
    pub turns: usize,
    pub tool_calls: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl SessionMetricsInfo {
    pub fn billed_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    pub fn cache_hit_percent(&self) -> Option<u64> {
        let billed_input = self.billed_input_tokens();
        (billed_input > 0).then(|| {
            (((self.cache_read_tokens as u128) * 100 + (billed_input as u128) / 2)
                / billed_input as u128) as u64
        })
    }

    pub fn accumulate_usage(&mut self, usage: &TokenUsage) {
        self.input_tokens = self
            .input_tokens
            .saturating_add(u64::from(usage.input_tokens));
        self.output_tokens = self
            .output_tokens
            .saturating_add(u64::from(usage.output_tokens));
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(u64::from(usage.cache_read_tokens));
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(u64::from(usage.cache_write_tokens));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindowInfo {
    pub current_tokens: u64,
    pub context_limit: u64,
    pub context_limit_is_estimate: bool,
    pub effective_model: String,
    pub compaction_generation: u64,
    pub last_compaction_seq: Option<u64>,
    pub provisional: bool,
    pub estimating: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionProjectionKey {
    pub session_id: String,
    pub session_file: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessageInfo {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub tool_activities: Vec<ToolActivityInfo>,
    pub streaming: bool,
    pub reasoning_content: Option<String>,
    pub reasoning_expanded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_prompt: Option<RetryPrompt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentActivityStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentActivityInfo {
    pub batch_run_id: u64,
    pub task_index: usize,
    pub journal_run_id: Option<String>,
    pub lane: Option<String>,
    pub agent: String,
    pub task: String,
    pub model: Option<String>,
    pub status: SubagentActivityStatus,
    pub messages: Vec<ChatMessageInfo>,
    pub isolation: Option<SubagentIsolation>,
    pub error: Option<String>,
}

/// A queued composer message a client wants the session to run next.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingComposerMessage {
    pub text: String,
    pub images: Vec<ImageAttachment>,
}

/// Timing of the latest foreground run, projected from the session journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTiming {
    pub start_seq: u64,
    pub source_seq: u64,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub finished: bool,
    pub suppressed: bool,
}

impl RunTiming {
    pub fn elapsed_seconds(&self, now_ms: u64, generating: bool) -> Option<u64> {
        if self.suppressed || generating == self.finished {
            return None;
        }
        let end = if self.finished {
            self.finished_at_ms?
        } else {
            now_ms
        };
        end.checked_sub(self.started_at_ms?).map(|ms| ms / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::ACP_CONFIG_CATEGORY_MODEL;
    use crate::interaction::{PermissionRequest, QuestionItemAnswer};

    #[test]
    fn retry_prompt_preserves_images_and_legacy_rows() {
        let legacy = serde_json::json!({"id":"error", "role":"Error", "content":"failed", "tool_activities":[], "streaming":false, "reasoning_content":null, "reasoning_expanded":false});
        let mut row: ChatMessageInfo = serde_json::from_value(legacy).unwrap();
        assert!(row.retry_prompt.is_none());
        for text in ["", "describe screenshot", "text only"] {
            let images = if text == "text only" {
                vec![]
            } else {
                vec![ImageAttachment {
                    display_name: "shot.png".into(),
                    data_url: "data:image/png;base64,AA==".into(),
                }]
            };
            row.retry_prompt = Some(RetryPrompt {
                text: text.into(),
                images,
            });
            assert!(row.retry_prompt.as_ref().unwrap().is_sendable());
            let decoded: ChatMessageInfo =
                serde_json::from_value(serde_json::to_value(&row).unwrap()).unwrap();
            assert_eq!(decoded, row);
        }
        assert!(!RetryPrompt {
            text: "  ".into(),
            images: vec![]
        }
        .is_sendable());
    }

    #[test]
    fn session_command_round_trips_through_json() {
        let commands = vec![
            SessionCommand::SubmitPrompt {
                session_id: "sess_1".into(),
                work_dir: PathBuf::from("/repo"),
                text: "hello".into(),
                images: vec![ImageAttachment {
                    display_name: "shot.png".into(),
                    data_url: "data:image/png;base64,AAAA".into(),
                }],
                effort: Some(ReasoningEffort::High),
                acp_config: vec![("model".into(), "auto".into())],
                model: Some("gpt-5".into()),
            },
            SessionCommand::CancelRun {
                session_id: "sess_1".into(),
            },
            SessionCommand::AnswerPermission {
                session_id: "sess_1".into(),
                request_id: "perm_1".into(),
                decision: PermissionDecision::AllowSession,
            },
            SessionCommand::AnswerQuestion {
                session_id: "sess_1".into(),
                answer: QuestionAnswer {
                    request_id: "q_1".into(),
                    answers: vec![QuestionItemAnswer {
                        question_id: "q1".into(),
                        selected: vec!["a".into()],
                        custom_text: None,
                    }],
                    dismissed: false,
                },
            },
            SessionCommand::SetModel {
                session_id: "sess_1".into(),
                model: "gpt-test".into(),
            },
            SessionCommand::SetReasoningEffort {
                session_id: "sess_1".into(),
                effort: ReasoningEffort::Low,
            },
            SessionCommand::SetModelRoles {
                session_id: "sess_1".into(),
                roles: ModelRoles {
                    fast: Some("fast-model".into()),
                    fallback_chain: vec!["b".into()],
                    cooldown_models: vec![],
                },
            },
            SessionCommand::SetOrchestratorMode {
                session_id: "sess_1".into(),
                mode: OrchestratorMode::Fusion,
            },
            SessionCommand::SetAcpConfigOption {
                session_id: "sess_1".into(),
                config_id: "cfg".into(),
                value: "v".into(),
            },
            SessionCommand::HydrateSession {
                request: SessionHydrationRequest {
                    session_id: "sess_1".into(),
                    session_file: PathBuf::from("/repo/.threadlane/sessions/sess_1.jsonl"),
                    reload_messages: true,
                    runtime_options: Some(HydrationRuntimeOptions {
                        work_dir: PathBuf::from("/repo"),
                        model: "gpt-test".into(),
                        model_roles: ModelRoles::default(),
                    }),
                },
            },
            SessionCommand::TerminalOpen {
                terminal_id: "pty_1".into(),
                cwd: PathBuf::from("/repo"),
                cols: 80,
                rows: 24,
            },
            SessionCommand::TerminalInput {
                terminal_id: "pty_1".into(),
                data: "ls\n".into(),
            },
            SessionCommand::TerminalResize {
                terminal_id: "pty_1".into(),
                cols: 80,
                rows: 24,
            },
            SessionCommand::TerminalClose {
                terminal_id: "pty_1".into(),
            },
            SessionCommand::SteerMessage {
                session_id: "sess_1".into(),
                text: "stop that".into(),
                images: vec![],
            },
            SessionCommand::SteerQueuedMessage {
                session_id: "sess_1".into(),
                entry_id: "entry-1".into(),
            },
            SessionCommand::CancelQueuedMessage {
                session_id: "sess_1".into(),
                entry_id: "entry-1".into(),
                work_dir: Some(PathBuf::from("/repo")),
            },
            SessionCommand::GetProjectState {
                work_dir: PathBuf::from("/repo"),
            },
            SessionCommand::GetSessionSnapshot {
                session_id: "sess_1".into(),
            },
            SessionCommand::ReadProjectFileVersioned {
                work_dir: PathBuf::from("/repo"),
                path: "src/main.rs".into(),
            },
            SessionCommand::WriteProjectFileGuarded {
                work_dir: PathBuf::from("/repo"),
                path: "src/main.rs".into(),
                content: "updated".into(),
                expected_version: "sha256:old".into(),
            },
        ];
        for command in commands {
            let json = serde_json::to_string(&command).expect("command serializes");
            let back: SessionCommand =
                serde_json::from_str(&json).expect("command deserializes");
            assert_eq!(serde_json::to_string(&back).unwrap(), json);
        }
    }

    fn sample_session() -> SessionInfo {
        SessionInfo {
            id: "sess_1".into(),
            title: "work".into(),
            work_dir: PathBuf::from("/repo"),
            runtime_work_dir: PathBuf::from("/repo"),
            session_file: PathBuf::from("/repo/.threadlane/sessions/sess_1.jsonl"),
            updated_at: 1,
            health: SessionHealth::Working,
            git_branch: Some("worktree/fix-1".into()),
            github_issue: Some(GitHubIssueRef {
                host: "github.com".into(),
                owner: "o".into(),
                repo: "r".into(),
                number: 79,
                url: "https://github.com/o/r/issues/79".into(),
            }),
            is_worktree: true,
            worktree_available: true,
            completion_summary: SessionCompletionSummary::Latest(RunCompletionToken {
                record_id: "r1".into(),
                run_id: "run1".into(),
                seq: 9,
            }),
        }
    }

    #[test]
    fn session_event_round_trips_through_json() {
        let events = vec![
            SessionEvent::Agent {
                session_id: "sess_1".into(),
                event: AgentEvent::PermissionRequested {
                    request: PermissionRequest {
                        id: "perm_1".into(),
                        capability: "network".into(),
                        title: "Allow host".into(),
                        detail: "example.com".into(),
                        scopes: vec![crate::interaction::PermissionScope::Session],
                    },
                },
            },
            SessionEvent::Finished {
                session_id: "sess_1".into(),
                session_file: PathBuf::from("/repo/.threadlane/sessions/sess_1.jsonl"),
            },
            SessionEvent::Scheduled {
                session_id: "sess_1".into(),
                session_file: PathBuf::from("/repo/.threadlane/sessions/sess_1.jsonl"),
                result: Some(Ok("done".into())),
            },
            SessionEvent::TitleGenerated {
                session_id: "sess_1".into(),
                session_file: PathBuf::from("/repo/.threadlane/sessions/sess_1.jsonl"),
            },
            SessionEvent::WorktreePrepared {
                session_id: "sess_1".into(),
                result: Ok(sample_session()),
            },
            SessionEvent::AcpConfigOptions {
                session_id: "sess_1".into(),
                session_file: PathBuf::from("/repo/.threadlane/sessions/sess_1.jsonl"),
                runtime_instance: 7,
                options: vec![AcpConfigOption {
                    id: "model".into(),
                    name: "Model".into(),
                    description: None,
                    category: Some(ACP_CONFIG_CATEGORY_MODEL.into()),
                    current_value: serde_json::Value::String("auto".into()),
                    options: vec![],
                }],
                error: None,
                failed_config: Some(("model".into(), "auto".into())),
            },
            SessionEvent::TerminalEvent {
                event: TerminalEvent::Output {
                    terminal_id: "pty_1".into(),
                    data: "$ ".into(),
                },
            },
            SessionEvent::TerminalEvent {
                event: TerminalEvent::Failed {
                    terminal_id: "pty_1".into(),
                    message: "could not spawn shell".into(),
                },
            },
            SessionEvent::FollowUpQueued {
                session_id: "sess_1".into(),
                entry_id: "entry-1".into(),
            },
            SessionEvent::ProjectChanged {
                project: ProjectInfo {
                    name: "repo".into(),
                    work_dir: PathBuf::from("/repo"),
                    sessions: vec![sample_session()],
                    is_expanded: true,
                },
            },
            SessionEvent::SessionSnapshot {
                session_id: "sess_1".into(),
                snapshot: Box::new(SessionSnapshot {
                    session: sample_session(),
                    ..Default::default()
                }),
            },
            SessionEvent::DaemonError {
                session_id: None,
                message: "boom".into(),
            },
            SessionEvent::SessionRemoved {
                session_id: "sess_1".into(),
                session_file: PathBuf::from("/repo/.threadlane/sessions/sess_1.jsonl"),
            },
            SessionEvent::CommandResult {
                request_id: 7,
                result: Ok(CommandResponse::CancelledQueuedMessage {
                    session_id: "sess_1".into(),
                    entry_id: "entry-1".into(),
                    text: "staged".into(),
                    images: vec![ImageAttachment {
                        display_name: "shot.png".into(),
                        data_url: "data:image/png;base64,AAAA".into(),
                    }],
                }),
            },
            SessionEvent::CommandResult {
                request_id: 8,
                result: Err("no live runtime".into()),
            },
            SessionEvent::QueuedEntryCancelled {
                session_id: "sess_1".into(),
                entry_id: "entry-1".into(),
                request_id: Some(7),
                text: "staged".into(),
                images: vec![ImageAttachment {
                    display_name: "shot.png".into(),
                    data_url: "data:image/png;base64,AAAA".into(),
                }],
            },
            SessionEvent::QueuedEntryCancelled {
                session_id: "sess_1".into(),
                entry_id: "entry-2".into(),
                request_id: None,
                text: String::new(),
                images: Vec::new(),
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).expect("event serializes");
            let back: SessionEvent = serde_json::from_str(&json).expect("event deserializes");
            assert_eq!(event, back);
        }
    }

    #[test]
    fn command_request_and_reply_round_trip_through_json() {
        let request = CommandRequest {
            request_id: 7,
            command: SessionCommand::CancelQueuedMessage {
                session_id: "sess_1".into(),
                entry_id: "entry-1".into(),
                work_dir: None,
            },
        };
        let json = serde_json::to_string(&request).expect("request serializes");
        let back: CommandRequest = serde_json::from_str(&json).expect("request deserializes");
        assert_eq!(back.request_id, request.request_id);
        assert_eq!(
            serde_json::to_string(&back.command).unwrap(),
            serde_json::to_string(&request.command).unwrap()
        );
        // The wire picks bare-command vs request by shape: neither may
        // deserialize as the other.
        let bare = serde_json::to_string(&request.command).expect("command serializes");
        assert!(serde_json::from_str::<CommandRequest>(&bare).is_err());
        assert!(serde_json::from_str::<SessionCommand>(&json).is_err());

        for reply in [
            CommandReply {
                request_id: 7,
                result: Ok(CommandResponse::Ack),
            },
            CommandReply {
                request_id: 8,
                result: Ok(CommandResponse::VersionedFile {
                    result: Ok(VersionedFile {
                        content: "λ".into(),
                        version: "sha256:abc".into(),
                    }),
                }),
            },
            CommandReply {
                request_id: 9,
                result: Ok(CommandResponse::GuardedFileWrite {
                    result: Err(ProjectFileError::Changed),
                }),
            },
            CommandReply {
                request_id: 7,
                result: Ok(CommandResponse::CancelledQueuedMessage {
                    session_id: "sess_1".into(),
                    entry_id: "entry-1".into(),
                    text: "staged".into(),
                    images: vec![ImageAttachment {
                        display_name: "shot.png".into(),
                        data_url: "data:image/png;base64,AAAA".into(),
                    }],
                }),
            },
            CommandReply {
                request_id: 7,
                result: Err("no live runtime".into()),
            },
        ] {
            let json = serde_json::to_string(&reply).expect("reply serializes");
            assert_eq!(
                serde_json::from_str::<CommandReply>(&json).expect("reply deserializes"),
                reply
            );
        }
    }

    #[test]
    fn guarded_file_types_and_typed_results_round_trip_through_json() {
        let file = VersionedFile {
            content: "saved text".into(),
            version: "sha256:0123".into(),
        };
        let errors = [
            ProjectFileError::Changed,
            ProjectFileError::Deleted,
            ProjectFileError::Io {
                message: "permission denied".into(),
            },
            ProjectFileError::Unsupported,
            ProjectFileError::Disconnected,
        ];
        for value in [
            CommandResponse::VersionedFile {
                result: Ok(file.clone()),
            },
            CommandResponse::VersionedFile {
                result: Err(errors[0].clone()),
            },
            CommandResponse::GuardedFileWrite {
                result: Ok("sha256:new".into()),
            },
            CommandResponse::GuardedFileWrite {
                result: Err(errors[2].clone()),
            },
        ] {
            let json = serde_json::to_string(&value).unwrap();
            assert_eq!(serde_json::from_str::<CommandResponse>(&json).unwrap(), value);
        }
        assert_eq!(ProjectFileError::Changed.to_string(), "file changed since it was read");
    }

    #[test]
    fn permission_decision_variants_decode() {
        for (json, expected) in [
            (r#""allow_once""#, PermissionDecision::AllowOnce),
            (r#""allow_session""#, PermissionDecision::AllowSession),
            (r#""allow_always""#, PermissionDecision::AllowAlways),
            (r#""deny""#, PermissionDecision::Deny),
        ] {
            let back: PermissionDecision = serde_json::from_str(json).expect("decodes");
            assert_eq!(back, expected);
        }
    }

    #[test]
    fn guarded_save_protocol_floor_does_not_raise_older_capabilities() {
        assert_eq!(WIRE_PROTOCOL_VERSION, 8);
        assert_eq!(EDITOR_LSP_PROTOCOL_VERSION, 8);
        assert_eq!(GUARDED_SAVE_PROTOCOL_VERSION, 7);
        assert_eq!(FILE_SEARCH_PROTOCOL_VERSION, 6);
        assert_eq!(PROJECT_IO_PROTOCOL_VERSION, 3);
        assert_eq!(COMMAND_REQUEST_PROTOCOL_VERSION, 2);
    }
}
