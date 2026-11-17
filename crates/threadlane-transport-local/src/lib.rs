//! The hub's client interface over a Unix socket, until iroh arrives in milestone 4.
//!
//! Wire format: UTF-8, one JSON value per line. A connection carries any number
//! of requests; each [`ClientRequest`] line is answered by exactly one
//! [`ClientResponse`] line, in order. See `docs/milestone-1.md` §6.

use std::fs;
use std::future::Future;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use threadlane_core::client::{
    Accepted, ApiError, ClientRequest, ClientResponse, DecisionReply, DecisionRequest, FeedItem,
    HubApi, HubInfo, OrgReply, OrgRequest, OverviewReply, OverviewRequest, PairingReply,
    PairingRequest, ProjectSummary, UserMessage, dispatch,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;

/// Default socket file name inside a hub's data directory.
pub const SOCKET_FILE: &str = "hub.sock";

/// The longest feed wait the server honours. Longer requests are cut to this.
pub const MAX_FEED_WAIT: Duration = Duration::from_secs(60);

/// How long a client waits for the answer to a request that does not wait on
/// the feed.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Added to a feed wait on the client side, so the server's own answer after
/// the wait always arrives before the client gives up.
const FEED_MARGIN: Duration = Duration::from_secs(10);

/// Pause after a failed `accept`, so a persistent error (for example running
/// out of file descriptors) does not spin the accept loop.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// A [`HubApi`] reached over the hub's Unix socket.
///
/// Opens a new connection per call. A connect failure, a broken connection, or
/// no answer in time is [`ApiError::Unreachable`].
#[derive(Debug, Clone)]
pub struct LocalClient {
    socket: PathBuf,
}

impl LocalClient {
    /// A client for the hub listening on `socket`. Does not connect yet.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// The socket this client connects to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    async fn call(
        &self,
        request: &ClientRequest,
        limit: Duration,
    ) -> Result<ClientResponse, ApiError> {
        match tokio::time::timeout(limit, self.exchange(request)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(ApiError::Unreachable(format!(
                "no answer from {} within {} s",
                self.socket.display(),
                limit.as_secs()
            ))),
        }
    }

    async fn exchange(&self, request: &ClientRequest) -> Result<ClientResponse, ApiError> {
        let mut line = serde_json::to_vec(request)
            .map_err(|e| ApiError::Invalid(format!("cannot encode the request: {e}")))?;
        line.push(b'\n');

        let mut stream = UnixStream::connect(&self.socket).await.map_err(|e| {
            ApiError::Unreachable(format!("cannot connect to {}: {e}", self.socket.display()))
        })?;
        let (read, mut write) = stream.split();
        write
            .write_all(&line)
            .await
            .map_err(|e| self.broken(&e.to_string()))?;

        let mut answer = Vec::new();
        BufReader::new(read)
            .read_until(b'\n', &mut answer)
            .await
            .map_err(|e| self.broken(&e.to_string()))?;
        if answer.last() != Some(&b'\n') {
            return Err(self.broken("the connection closed before the hub answered"));
        }
        serde_json::from_slice(&answer)
            .map_err(|e| ApiError::Internal(format!("unreadable answer from the hub: {e}")))
    }

    fn broken(&self, reason: &str) -> ApiError {
        ApiError::Unreachable(format!(
            "connection to {} broke: {reason}",
            self.socket.display()
        ))
    }
}

/// The error for an answer of the wrong kind; a hub error passes through.
/// Shared with the other transports' clients.
pub fn unexpected(response: ClientResponse, request: &str) -> ApiError {
    match response {
        ClientResponse::Error { error } => error,
        _ => ApiError::Internal(format!(
            "the hub answered a {request} request with a different kind of answer"
        )),
    }
}

#[async_trait]
impl HubApi for LocalClient {
    async fn info(&self) -> Result<HubInfo, ApiError> {
        match self.call(&ClientRequest::Info, REQUEST_TIMEOUT).await? {
            ClientResponse::Info { info } => Ok(info),
            other => Err(unexpected(other, "info")),
        }
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        match self.call(&ClientRequest::Projects, REQUEST_TIMEOUT).await? {
            ClientResponse::Projects { projects } => Ok(projects),
            other => Err(unexpected(other, "projects")),
        }
    }

    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError> {
        let request = ClientRequest::SendMessage {
            message: message.clone(),
        };
        match self.call(&request, REQUEST_TIMEOUT).await? {
            ClientResponse::Accepted { accepted } => Ok(accepted),
            other => Err(unexpected(other, "send_message")),
        }
    }

    async fn feed(&self, after: i64, wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        let request = ClientRequest::Feed {
            after,
            wait_ms: u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
        };
        let limit = wait.min(MAX_FEED_WAIT) + FEED_MARGIN;
        match self.call(&request, limit).await? {
            ClientResponse::Feed { items } => Ok(items),
            other => Err(unexpected(other, "feed")),
        }
    }

    async fn org(&self, request: OrgRequest) -> Result<OrgReply, ApiError> {
        match self
            .call(&ClientRequest::Org { request }, REQUEST_TIMEOUT)
            .await?
        {
            ClientResponse::Org { reply } => Ok(reply),
            other => Err(unexpected(other, "org")),
        }
    }

    async fn decisions(&self, request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        match self
            .call(&ClientRequest::Decisions { request }, REQUEST_TIMEOUT)
            .await?
        {
            ClientResponse::Decisions { reply } => Ok(reply),
            other => Err(unexpected(other, "decisions")),
        }
    }

    async fn pairing(&self, request: PairingRequest) -> Result<PairingReply, ApiError> {
        match self
            .call(&ClientRequest::Pairing { request }, REQUEST_TIMEOUT)
            .await?
        {
            ClientResponse::Pairing { reply } => Ok(reply),
            other => Err(unexpected(other, "pairing")),
        }
    }

    async fn overview(&self, request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        match self
            .call(&ClientRequest::Overview { request }, REQUEST_TIMEOUT)
            .await?
        {
            ClientResponse::Overview { reply } => Ok(reply),
            other => Err(unexpected(other, "overview")),
        }
    }
}

/// Binds the socket (replacing a stale file), sets mode 0600, and serves
/// until `shutdown` completes. Removes the socket file on the way out.
///
/// Each connection is served by its own task, so a long feed wait on one
/// connection never delays another. Connections still open when `shutdown`
/// completes are dropped.
///
/// # Errors
///
/// Fails if the socket cannot be bound: the path is too long for the platform,
/// another server is listening on it, a file that is not a socket is in the
/// way, or the directory is not writable.
pub async fn serve(
    socket: &Path,
    api: Arc<dyn HubApi>,
    shutdown: impl Future<Output = ()> + Send,
) -> io::Result<()> {
    let listener = bind(socket).await?;
    let _remove_on_exit = SocketFile(socket.to_path_buf());
    tracing::debug!(socket = %socket.display(), "serving the client interface");

    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => {
                    connections.spawn(serve_connection(stream, Arc::clone(&api)));
                }
                Err(error) => {
                    tracing::warn!(%error, "cannot accept a client connection");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Some(_finished) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    drop(listener);
    connections.abort_all();
    Ok(())
}

/// Removes the socket file when dropped, however `serve` ends.
#[derive(Debug)]
struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.0)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(socket = %self.0.display(), %error, "cannot remove the socket file");
        }
    }
}

async fn bind(socket: &Path) -> io::Result<UnixListener> {
    remove_stale(socket).await?;
    let listener = UnixListener::bind(socket).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidInput {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "socket path {} is too long for this platform ({} bytes; Unix socket paths \
                     are limited to about 100 bytes): {error}",
                    socket.display(),
                    socket.as_os_str().len()
                ),
            )
        } else {
            io::Error::new(
                error.kind(),
                format!("cannot bind {}: {error}", socket.display()),
            )
        }
    })?;
    if let Err(error) = fs::set_permissions(socket, fs::Permissions::from_mode(0o600)) {
        // Do not leave a socket with looser permissions behind.
        let _ignored = fs::remove_file(socket);
        return Err(io::Error::new(
            error.kind(),
            format!("cannot set mode 0600 on {}: {error}", socket.display()),
        ));
    }
    Ok(listener)
}

/// Removes a socket file left by a server that is gone. Refuses to touch a
/// socket someone still listens on, or a file that is not a socket.
async fn remove_stale(socket: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(socket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} exists and is not a socket; not replacing it",
                socket.display()
            ),
        ));
    }
    let probe = tokio::time::timeout(Duration::from_secs(1), UnixStream::connect(socket)).await;
    if let Ok(Ok(_live)) = probe {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("another server is listening on {}", socket.display()),
        ));
    }
    tracing::debug!(socket = %socket.display(), "replacing a stale socket file");
    match fs::remove_file(socket) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

async fn serve_connection(mut stream: UnixStream, api: Arc<dyn HubApi>) {
    let (read, mut write) = stream.split();
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(%error, "client connection failed");
                return;
            }
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let response = match serde_json::from_slice::<ClientRequest>(&line) {
            Ok(request) => dispatch(api.as_ref(), cap_wait(request)).await,
            Err(error) => ClientResponse::Error {
                error: ApiError::Invalid(format!("unparseable request: {error}")),
            },
        };
        let mut answer = encode(&response);
        answer.push(b'\n');
        if let Err(error) = write.write_all(&answer).await {
            tracing::debug!(%error, "cannot answer a client");
            return;
        }
    }
}

/// A response as one JSON value, without the newline. An answer that cannot
/// be encoded becomes an internal error. Shared with the other transports.
#[must_use]
pub fn encode(response: &ClientResponse) -> Vec<u8> {
    serde_json::to_vec(response).unwrap_or_else(|error| {
        let fallback = ClientResponse::Error {
            error: ApiError::Internal(format!("cannot encode the answer: {error}")),
        };
        // An error made of plain strings always encodes.
        serde_json::to_vec(&fallback).unwrap_or_default()
    })
}

/// The request with its feed wait cut to [`MAX_FEED_WAIT`]. Shared with the
/// other transports.
#[must_use]
pub fn cap_wait(request: ClientRequest) -> ClientRequest {
    let max_ms = u64::try_from(MAX_FEED_WAIT.as_millis()).unwrap_or(u64::MAX);
    match request {
        ClientRequest::Feed { after, wait_ms } => ClientRequest::Feed {
            after,
            wait_ms: wait_ms.min(max_ms),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_waits_are_capped_at_sixty_seconds() {
        assert_eq!(
            cap_wait(ClientRequest::Feed {
                after: 3,
                wait_ms: 3_600_000
            }),
            ClientRequest::Feed {
                after: 3,
                wait_ms: 60_000
            }
        );
        assert_eq!(
            cap_wait(ClientRequest::Feed {
                after: 3,
                wait_ms: 500
            }),
            ClientRequest::Feed {
                after: 3,
                wait_ms: 500
            }
        );
        assert_eq!(cap_wait(ClientRequest::Info), ClientRequest::Info);
    }
}
