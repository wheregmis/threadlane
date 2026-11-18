//! Client surface for the Threadlane daemon.
//!
//! [`DaemonClient`] is the only boundary the UI talks to: commands go in as
//! serializable [`SessionCommand`]s, events come out as [`SessionEvent`]s.
//! Two transports implement it:
//!
//! - [`LocalDaemon`] — owns a [`DaemonCore`] in-process. The desktop app's
//!   zero-regression path; identical semantics to the pre-split in-process
//!   services.
//! - [`RemoteDaemon`] — a WebSocket client speaking bare `SessionCommand`
//!   JSON outbound and `SessionEvent` JSON inbound. On every (re)connect the
//!   server replays its bounded journal tail before live events, which is
//!   what makes attach-mid-run work.
//!
//! Framing is one JSON value per WebSocket text message — bare commands
//! outbound (a `CommandRequest` envelope for reply-carrying commands),
//! `{"seq", "event"}` and `{"response"}` frames inbound. The client-facing
//! `subscribe()` receiver collapses every transport into one ordered
//! `SessionEvent` stream so views never learn which side they are on.

#[cfg(feature = "local")]
mod local;
mod remote;

#[cfg(feature = "local")]
pub use local::LocalDaemon;
mod state;
pub use state::{ClientState, ComposerDraft};
pub use remote::{RemoteDaemon, ConnectionState};

use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use threadlane_protocol::daemon::{
    CommandRequest, CommandResponse, SessionCommand, SessionEvent,
};

/// Caller-chosen `request_id` source shared by every `CommandRequest`
/// issued through this crate — `AppState` control requests and per-view
/// project-io calls alike — so two issuers on one `RemoteDaemon`
/// connection can never collide and strand each other's waiters.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a connection-unique `request_id` for a `CommandRequest`.
pub fn next_request_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// The session contract a daemon serves and clients consume.
#[async_trait]
pub trait DaemonClient: Send + Sync {
    /// Dispatch one command. Callers must handle the returned `Err`;
    /// `SessionEvent::DaemonError` delivery is transport-specific
    /// (`AppState::dispatch_command` also converts `Err` into the event
    /// stream for its callers).
    async fn command(&self, command: SessionCommand) -> Result<(), String>;

    /// Dispatch one command expecting the daemon's [`CommandResponse`]
    /// back: `Ok` carries the payload (`Ack` for commands that return
    /// nothing), `Err` is the send or dispatch failure (the daemon also
    /// broadcasts a dispatch failure as `DaemonError`). `request_id` is
    /// caller-chosen and must be unique per client connection.
    ///
    /// Only valid when [`Self::supports_command_requests`] is true — a
    /// version-1 daemon cannot decode the envelope, so an ungated call
    /// fails the request without the command ever being dispatched.
    async fn command_request(
        &self,
        request: CommandRequest,
    ) -> Result<CommandResponse, String>;

    /// `command_request` with a freshly allocated id from
    /// [`crate::next_request_id`] — the common case for callers that do
    /// not need to know the id up front (e.g. to correlate a journaled
    /// recovery event).
    async fn request(&self, command: SessionCommand) -> Result<CommandResponse, String> {
        self.command_request(CommandRequest {
            request_id: crate::next_request_id(),
            command,
        })
        .await
    }

    /// Whether the attached daemon speaks the `CommandRequest` envelope
    /// (wire protocol version ≥ 2). Always true in-process; a remote
    /// client learns it from the handshake's `x-threadlane-protocol`
    /// response header and reports false while unconnected or attached
    /// to a pre-2 daemon. Callers use it to pick a degraded path (e.g. a
    /// fire-and-forget bare command) rather than lose the command.
    fn supports_command_requests(&self) -> bool;

    /// Whether the attached daemon serves the project-io surface: file
    /// tree/read/write commands, `GitRequest` operations, and
    /// `WatchProject`/`UnwatchProject`/`GetWorktreeBases` (wire protocol
    /// version ≥ [`PROJECT_IO_PROTOCOL_VERSION`]). Always true in-process;
    /// a remote client reports false while unconnected or attached to a
    /// pre-3 daemon — callers must fail the operation rather than touch
    /// their own filesystem, which is not the host's.
    fn supports_project_io(&self) -> bool;

    /// Whether the attached daemon supports ephemeral language services for
    /// unsaved editor buffers (wire protocol version ≥ 8).
    fn supports_editor_lsp(&self) -> bool { false }

    /// Distinct capability floor: project I/O alone does not imply search.
    fn supports_file_search(&self) -> bool { false }
    /// Versioned reads and writes guarded by a previously read version.
    fn supports_guarded_saves(&self) -> bool { false }
    /// Current transport availability (in-process clients are always connected).
    fn is_connected(&self) -> bool { true }
    /// Changes on reconnect, invalidating ephemeral results from an old host.
    fn file_search_connection_epoch(&self) -> u64 { 0 }

    /// Whether the attached daemon serves the GitHub (forge) and
    /// automation surfaces: `GitHubRequest`/`AutomationRequest` commands
    /// and `SessionEvent::AutomationChanged` (wire protocol version ≥
    /// [`GITHUB_AUTOMATION_PROTOCOL_VERSION`]). Always true in-process;
    /// a remote client reports false while unconnected or attached to a
    /// pre-5 daemon — callers hide the panels rather than issue commands
    /// an older daemon cannot decode.
    fn supports_github_automation(&self) -> bool;

    /// Attach to the daemon's event stream. Each call returns an
    /// independent receiver; journal replay (for late attach/reconnect)
    /// is decided by the transport, not the caller.
    fn subscribe(&self) -> mpsc::UnboundedReceiver<SessionEvent>;
}
