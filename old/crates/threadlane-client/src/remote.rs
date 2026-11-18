//! WebSocket transport: [`RemoteDaemon`] dials a running `threadlane-daemon`
//! process and speaks the `threadlane-protocol::daemon` session contract —
//! bare `SessionCommand` JSON outbound (a `CommandRequest` envelope for
//! commands that expect a reply), `{"seq", "event"}` frames inbound plus
//! `{"response"}` replies resolved against in-flight requests.
//!
//! Reconnect semantics: the driver reconnects with capped exponential
//! backoff whenever the socket drops. The daemon journals every event with
//! a monotonic sequence; the client tracks its last-seen `seq` and passes
//! it back as `?since=` on the next attach, so the journal-tail replay
//! delivers exactly the events the client missed — deltas already applied
//! (a half-streamed `TextDelta`, say) never append twice.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use threadlane_protocol::daemon::{
    CommandReply, CommandRequest, CommandResponse, SessionCommand, SessionEvent,
    COMMAND_REQUEST_PROTOCOL_VERSION, EDITOR_LSP_PROTOCOL_VERSION,
    GUARDED_SAVE_PROTOCOL_VERSION,
    PROJECT_IO_PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER,
};

use crate::DaemonClient;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting,
    Failed(String),
}

/// First reconnect delay; doubles per failed dial up to
/// [`RECONNECT_BACKOFF_MAX`].
const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(250);
/// Longest delay between reconnect attempts.
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Longest a `command_request` waits for its reply before the caller is
/// failed — the reply and the journaled recovery both land far inside
/// it, so expiry means the answer was never coming (a peer that does
/// not speak the request envelope, a dropped frame). Sized for project-io
/// Git mutations (`fetch`/`pull` over a WAN), not just control traffic.
const COMMAND_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);

/// One outbound frame: a bare fire-and-forget command, or a request
/// envelope the server answers with a `{"response": ...}` reply.
enum OutboundMessage {
    Command(SessionCommand),
    Request(CommandRequest),
}

/// A `CommandRequest` awaiting its reply. `replayable` requests — ones
/// whose outcome the daemon also journals (today `CancelQueuedMessage`,
/// via `QueuedEntryCancelled`) — survive one disconnect: the reply dies
/// with the socket, but the journaled copy still arrives on the
/// reconnect's journal-tail replay and resolves the waiter.
struct PendingRequest {
    waiter: oneshot::Sender<Result<CommandResponse, String>>,
    /// The reply alone is unrecoverable; a journaled event can still
    /// answer the waiter on replay.
    replayable: bool,
    /// Already outlived one disconnect — a second one fails the waiter
    /// like any other request.
    survived_disconnect: bool,
}

/// Server-to-client frame: either the journal sequence the event was
/// written under (`0` for frames the daemon synthesizes outside the
/// journal, like lag notices) plus the event itself, or the point-to-point
/// reply to a `CommandRequest` this client sent.
#[derive(Deserialize)]
struct WireFrame {
    #[serde(default)]
    seq: u64,
    event: Option<SessionEvent>,
    response: Option<CommandReply>,
}

/// A [`DaemonClient`] that attaches to a remote `threadlane-daemon` over
/// WebSocket.
pub struct RemoteDaemon {
    executor: Option<tokio::runtime::Handle>,
    connection: tokio::sync::watch::Sender<ConnectionState>,
    driver: Mutex<Option<tokio::task::AbortHandle>>,
    /// Commands awaiting/in-flight to the server; the driver drains it.
    command_tx: mpsc::UnboundedSender<OutboundMessage>,
    /// Replies awaited by live `command_request` calls, keyed by
    /// `request_id`; the reader resolves each when its `response` frame
    /// (or a journaled recovery event) lands, and a reconnect fails
    /// them all except one-shot `replayable` survivors.
    pending_requests: Arc<Mutex<HashMap<u64, PendingRequest>>>,
    /// Every `subscribe()` caller's channel; the reader task fans events out.
    subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<SessionEvent>>>>,
    /// Errors raised before any subscriber could exist (e.g. a refused
    /// scheme); drained into the first `subscribe()` receiver.
    startup_errors: Mutex<Vec<SessionEvent>>,
    /// Whether a socket is live right now — commands sent while down fail
    /// fast instead of firing stale after a reconnect.
    connected: Arc<AtomicBool>,
    /// Most recent journal sequence delivered; sent back as `?since=`
    /// on reconnect so the tail replay covers only the gap.
    last_seq: Arc<AtomicU64>,
    /// The peer's wire protocol version from the last successful
    /// handshake's `x-threadlane-protocol` header; 0 before the first
    /// dial and 1 when a pre-versioned daemon sends no header. Updated
    /// on every connect, so a rollback to an older daemon re-gates the
    /// `CommandRequest` envelope.
    protocol_version: Arc<AtomicU64>,
    connection_epoch: Arc<AtomicU64>,
    reconnect: Arc<Notify>,
    paired_device_id: Arc<Mutex<Option<String>>>,
}

impl RemoteDaemon {
    /// Transport failures after enqueueing cannot prove whether dispatch ran.
    /// Reserved prefix on request errors; daemon rejection replies are unchanged.
    pub const UNKNOWN_REQUEST_OUTCOME: &'static str = "request outcome unknown: ";

    /// Construct the client and spawn its connection driver on the shared
    /// Threadlane reactor. The returned client is usable immediately —
    /// commands issued before the first successful dial are reported as
    /// errors through the event stream.
    ///
    /// A bearer token over plaintext `ws://` is refused for non-loopback
    /// hosts (the token, and everything it protects, would cross the wire
    /// readable); use `wss://` there. The refusal lands as a `DaemonError`
    /// on the first `subscribe()` receiver.
    #[cfg(feature = "local")]
    pub fn connect(url: impl Into<String>, token: Option<String>) -> Arc<Self> {
        // Preserve the desktop reactor and its startup behavior.
        let executor = threadlane_daemon::chat::executor().map(|runtime| runtime.handle().clone());
        Self::start(url.into(), token, None, executor, false)
    }

    pub fn supports_composer_options(&self) -> bool {
        self.protocol_version.load(Ordering::SeqCst)
            >= threadlane_protocol::daemon::COMPOSER_PROTOCOL_VERSION
    }

    pub fn connect_with_runtime(
        url: impl Into<String>,
        token: Option<String>,
        executor: tokio::runtime::Handle,
    ) -> Arc<Self> {
        Self::start(url.into(), token, None, Ok(executor), false)
    }

    /// Explicit opt-in for the existing token-protected LAN/VPN pairing listener.
    /// Only literal private/link-local/loopback/shared IPs or localhost are accepted.
    /// Shared IPv4 addresses (100.64.0.0/10) support Tailscale; callers must ensure
    /// their VPN is connected, since the address alone does not guarantee encryption.
    /// Ordinary remote connections retain the TLS policy.
    pub fn connect_pairing(
        url: impl Into<String>,
        token: String,
        executor: tokio::runtime::Handle,
    ) -> Result<Arc<Self>, String> {
        Self::connect_pairing_named(url, token, None, executor)
    }

    pub fn connect_pairing_named(
        url: impl Into<String>,
        token: String,
        device_name: Option<String>,
        executor: tokio::runtime::Handle,
    ) -> Result<Arc<Self>, String> {
        let url = url.into();
        Self::validate_pairing(&url, &token)?;
        Ok(Self::start(url, Some(token), device_name, Ok(executor), true))
    }

    fn validate_pairing(url: &str, token: &str) -> Result<(), String> {
        if token.trim().is_empty() {
            return Err("pairing requires a token".into());
        }
        format!("Bearer {token}")
            .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
            .map_err(|_| "invalid pairing token")?;
        let request = url
            .to_owned()
            .into_client_request()
            .map_err(|_| "invalid pairing URL")?;
        let uri = request.uri();
        let pairing_address = uri.host().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| match ip {
                        std::net::IpAddr::V4(ip) => {
                            let [first, second, ..] = ip.octets();
                            let shared = first == 100 && (64..=127).contains(&second);
                            ip.is_loopback() || ip.is_private() || ip.is_link_local() || shared
                        }
                        std::net::IpAddr::V6(ip) => {
                            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
                        }
                    })
        });
        if uri.scheme_str() != Some("ws") || !pairing_address {
            return Err("Pairing requires a local network or Tailscale IPv4 ws:// address. Use your desktop's IP address, not a hostname.".into());
        }
        Ok(())
    }

    fn start(
        url: String,
        token: Option<String>,
        device_name: Option<String>,
        executor: Result<tokio::runtime::Handle, String>,
        pairing: bool,
    ) -> Arc<Self> {
        let (connection, _) = tokio::sync::watch::channel(ConnectionState::Connecting);
        let (command_tx, command_rx) = mpsc::unbounded_channel::<OutboundMessage>();
        let client = Arc::new(Self {
            executor: executor.as_ref().ok().cloned(),
            connection,
            driver: Mutex::new(None),
            command_tx,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            startup_errors: Mutex::new(Vec::new()),
            connected: Arc::new(AtomicBool::new(false)),
            last_seq: Arc::new(AtomicU64::new(0)),
            protocol_version: Arc::new(AtomicU64::new(0)),
            connection_epoch: Arc::new(AtomicU64::new(0)),
            reconnect: Arc::new(Notify::new()),
            paired_device_id: Arc::new(Mutex::new(None)),
        });
        if let Some(error) = (!pairing)
            .then(|| Self::transport_policy_error(&url, &token))
            .flatten()
        {
            client
                .connection
                .send_replace(ConnectionState::Failed(error.clone()));
            client.push_startup_error(error);
            return client;
        }
        let executor = match executor {
            Ok(executor) => executor,
            Err(error) => {
                client
                    .connection
                    .send_replace(ConnectionState::Failed(error.clone()));
                client.push_startup_error(error);
                return client;
            }
        };
        let subscribers = client.subscribers.clone();
        let pending_requests = client.pending_requests.clone();
        let connected = client.connected.clone();
        let last_seq = client.last_seq.clone();
        let protocol_version = client.protocol_version.clone();
        let connection_epoch = client.connection_epoch.clone();
        let reconnect = client.reconnect.clone();
        let paired_device_id = client.paired_device_id.clone();
        let driver = executor.spawn(Self::drive(
            url,
            token,
            device_name,
            pairing,
            command_rx,
            subscribers,
            pending_requests,
            connected,
            last_seq,
            protocol_version,
            connection_epoch,
            reconnect,
            paired_device_id,
            client.connection.clone(),
        ));
        *client.driver.lock().expect("connection driver poisoned") = Some(driver.abort_handle());
        client
    }

    /// Enqueue a command in caller order, failing immediately while offline.
    pub fn send(&self, command: SessionCommand) -> Result<(), String> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err("daemon is not connected".to_string());
        }
        let peer_version = self.protocol_version.load(Ordering::SeqCst);
        if matches!(&command, SessionCommand::EditorLsp { .. })
            && peer_version < EDITOR_LSP_PROTOCOL_VERSION
        {
            return Err(format!(
                "Editor LSP requires protocol v{EDITOR_LSP_PROTOCOL_VERSION}; attached daemon uses v{peer_version}"
            ));
        }
        self.command_tx
            .send(OutboundMessage::Command(command))
            .map_err(|_| "daemon connection driver is gone".to_string())
    }
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
    pub fn subscribe_connection(&self) -> tokio::sync::watch::Receiver<ConnectionState> {
        self.connection.subscribe()
    }

    /// Wake the reconnect driver after a foreground transition. Connected
    /// sessions are left undisturbed.
    pub fn request_reconnect(&self) {
        if !self.connected.load(Ordering::SeqCst) {
            self.reconnect.notify_one();
        }
    }

    /// Stable daemon-side paired-device identity returned by the handshake.
    pub fn paired_device_id(&self) -> Option<String> {
        self.paired_device_id
            .lock()
            .expect("paired device identity poisoned")
            .clone()
    }

    /// Transport rules enforced before any dial: a token may only cross a
    /// plaintext socket on loopback.
    fn transport_policy_error(url: &str, token: &Option<String>) -> Option<String> {
        let Some(_) = token else {
            return None;
        };
        let request = match url.to_string().into_client_request() {
            Ok(request) => request,
            Err(error) => return Some(format!("invalid daemon url {url}: {error}")),
        };
        let uri = request.uri();
        let plaintext = matches!(uri.scheme_str(), Some("ws") | Some("http"));
        let loopback = uri.host().is_some_and(|host| {
            host == "localhost"
                || host.ends_with(".localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        (plaintext && !loopback).then(|| {
            format!(
                "refusing {url}: a bearer token may not cross a plaintext \
                 ws:// connection to a non-loopback host; use wss://"
            )
        })
    }

    /// Stash an error subscribers have not had a chance to attach for.
    fn push_startup_error(&self, message: String) {
        self.startup_errors
            .lock()
            .expect("startup errors poisoned")
            .push(SessionEvent::DaemonError {
                session_id: None,
                message,
            });
    }

    /// The dial URL for the next attempt, carrying the resume cursor.
    fn dial_url(url: &str, last_seq: &AtomicU64) -> String {
        let since = last_seq.load(Ordering::SeqCst);
        if since == 0 {
            return url.to_string();
        }
        let mut uri = url
            .into_client_request()
            .expect("validated daemon URL")
            .uri()
            .clone()
            .into_parts();
        let path = uri
            .path_and_query
            .as_ref()
            .map(|path| path.as_str())
            .unwrap_or("/");
        let separator = if path.contains('?') { '&' } else { '?' };
        uri.path_and_query = Some(
            format!("{path}{separator}since={since}")
                .parse()
                .expect("valid resume cursor"),
        );
        tokio_tungstenite::tungstenite::http::Uri::from_parts(uri)
            .expect("valid daemon URI")
            .to_string()
    }

    /// One connection/reconnect loop running for the client's lifetime.
    /// Exits once `command_tx` is dropped — a driver must not outlive its
    /// client dialing forever.
    async fn drive(
        url: String,
        token: Option<String>,
        device_name: Option<String>,
        pairing: bool,
        mut command_rx: mpsc::UnboundedReceiver<OutboundMessage>,
        subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<SessionEvent>>>>,
        pending_requests: Arc<Mutex<HashMap<u64, PendingRequest>>>,
        connected: Arc<AtomicBool>,
        last_seq: Arc<AtomicU64>,
        protocol_version: Arc<AtomicU64>,
        connection_epoch: Arc<AtomicU64>,
        reconnect: Arc<Notify>,
        paired_device_id: Arc<Mutex<Option<String>>>,
        connection: tokio::sync::watch::Sender<ConnectionState>,
    ) {
        let mut was_connected = false;
        let mut backoff = RECONNECT_BACKOFF_INITIAL;
        loop {
            if command_rx.is_closed() {
                return;
            }
            if was_connected {
                was_connected = false;
                connected.store(false, Ordering::SeqCst);
                connection.send_replace(ConnectionState::Reconnecting);
                // A queued SubmitPrompt firing minutes late is worse than a
                // dropped command: fail them all and let the user resend.
                let mut dropped = 0usize;
                while command_rx.try_recv().is_ok() {
                    dropped += 1;
                }
                if dropped > 0 {
                    Self::fanout(
                        &subscribers,
                        SessionEvent::DaemonError {
                            session_id: None,
                            message: format!(
                                "daemon connection dropped {dropped} queued command(s)"
                            ),
                        },
                    );
                }
                // Requests queued behind the drain or already on the
                // dead socket can never be answered — resolve their
                // waiters instead of parking callers on a reply that
                // won't come. The exception is requests whose outcome
                // the daemon journals: the journaled event can still
                // arrive on the reconnect's tail replay, so they get
                // one reconnect to recover it.
                let mut pending = pending_requests.lock().expect("command waiters poisoned");
                let drained = std::mem::take(&mut *pending);
                for (request_id, mut waiter) in drained {
                    if waiter.replayable && !waiter.survived_disconnect {
                        waiter.survived_disconnect = true;
                        pending.insert(request_id, waiter);
                    } else {
                        let _ = waiter.waiter.send(Err(format!(
                            "{}daemon connection lost",
                            Self::UNKNOWN_REQUEST_OUTCOME
                        )));
                    }
                }
                drop(pending);
                Self::fanout(
                    &subscribers,
                    SessionEvent::DaemonError {
                        session_id: None,
                        message: "daemon connection lost; reconnecting".to_string(),
                    },
                );
            }
            let dial_url = Self::dial_url(&url, &last_seq);
            let mut request = match dial_url.clone().into_client_request() {
                Ok(request) => request,
                Err(error) => {
                    Self::fanout(
                        &subscribers,
                        SessionEvent::DaemonError {
                            session_id: None,
                            message: format!("invalid daemon url {dial_url}: {error}"),
                        },
                    );
                    return;
                }
            };
            if let Some(token) = &token {
                match format!("Bearer {token}")
                    .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
                {
                    Ok(header) => {
                        request.headers_mut().insert("Authorization", header);
                    }
                    Err(error) => {
                        Self::fanout(
                            &subscribers,
                            SessionEvent::DaemonError {
                                session_id: None,
                                message: format!("could not build auth header: {error}"),
                            },
                        );
                        return;
                    }
                }
            }
            if let Some(device_name) = &device_name {
                if let Ok(header) = tokio_tungstenite::tungstenite::http::HeaderValue::from_str(
                    device_name,
                ) {
                    request.headers_mut().insert("x-threadlane-device-name", header);
                }
            }
            let connected_socket = tokio::time::timeout(
                CONNECT_HANDSHAKE_TIMEOUT,
                tokio_tungstenite::connect_async(request),
            )
            .await;
            let (mut socket, response) = match connected_socket {
                Ok(Ok(pair)) => pair,
                Ok(Err(error))
                    if pairing
                        && matches!(
                            &error,
                            tokio_tungstenite::tungstenite::Error::Http(response)
                                if response.status() == tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED
                        ) =>
                {
                    let message = "This device is no longer authorized. Remove it and pair again."
                        .to_string();
                    connection.send_replace(ConnectionState::Failed(message.clone()));
                    Self::fanout(
                        &subscribers,
                        SessionEvent::DaemonError {
                            session_id: None,
                            message,
                        },
                    );
                    return;
                }
                Ok(Err(error)) => {
                    connection.send_replace(ConnectionState::Reconnecting);
                    tracing::warn!("daemon connect to {dial_url} failed: {error}");
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = reconnect.notified() => {}
                    }
                    backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                    continue;
                }
                Err(_) => {
                    connection.send_replace(ConnectionState::Reconnecting);
                    tracing::warn!("daemon connection handshake to {dial_url} timed out");
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = reconnect.notified() => {}
                    }
                    backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                    continue;
                }
            };
            *paired_device_id
                .lock()
                .expect("paired device identity poisoned") = response
                .headers()
                .get("x-threadlane-device-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            // Capability handshake: the daemon announces its wire protocol
            // version in a response header; absent means a pre-versioned
            // daemon that cannot decode the CommandRequest envelope.
            let peer_version = response
                .headers()
                .get(PROTOCOL_VERSION_HEADER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(1);
            protocol_version.store(peer_version, Ordering::SeqCst);
            connection_epoch.fetch_add(1, Ordering::SeqCst);
            was_connected = true;
            backoff = RECONNECT_BACKOFF_INITIAL;
            connected.store(true, Ordering::SeqCst);
            connection.send_replace(ConnectionState::Connected);
            // Cursor replay may be empty on an idle daemon. Refresh inventory
            // for this epoch so clients need not trust pre-disconnect metadata.
            let inventory = serde_json::to_string(&SessionCommand::GetProjects)
                .expect("GetProjects is serializable");
            if socket.send(Message::Text(inventory.into())).await.is_err() {
                continue;
            }
            let mut connection_seq = 0u64;
            let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let _ = heartbeat.tick().await;
            let mut ping_sent_at = None;
            // Server replays the journal tail newer than `?since=` first,
            // then live events — the same attach semantics LocalDaemon's
            // subscribe() exposes.
            loop {
                tokio::select! {
                    _ = heartbeat.tick() => {
                        if ping_sent_at.is_some_and(|sent_at: Instant| {
                            sent_at.elapsed() >= HEARTBEAT_TIMEOUT
                        }) {
                            tracing::warn!("daemon heartbeat timed out for {dial_url}");
                            break;
                        }
                        if ping_sent_at.is_none() {
                            if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                                break;
                            }
                            ping_sent_at = Some(Instant::now());
                        }
                    }
                    message = command_rx.recv() => {
                        let Some(message) = message else { return };
                        let text = match &message {
                            OutboundMessage::Command(command) => {
                                serde_json::to_string(command)
                            }
                            OutboundMessage::Request(request) => {
                                serde_json::to_string(request)
                            }
                        };
                        let text = match text {
                            Ok(text) => text,
                            Err(error) => {
                                Self::fanout(&subscribers, SessionEvent::DaemonError {
                                    session_id: None,
                                    message: format!("could not encode command: {error}"),
                                });
                                continue;
                            }
                        };
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    message = socket.next() => {
                        match message {
                            Some(Ok(Message::Text(text))) => {
                                match serde_json::from_str::<WireFrame>(&text) {
                                    Ok(frame) => {
                                        if let Some(reply) = frame.response {
                                            let waiter = pending_requests
                                                .lock()
                                                .expect("command waiters poisoned")
                                                .remove(&reply.request_id);
                                            if let Some(waiter) = waiter {
                                                let _ = waiter.waiter.send(reply.result);
                                            }
                                        } else if let Some(event) = frame.event {
                                            if frame.seq > 0 {
                                                // The server filters replay by `since`. A new daemon
                                                // restarts its counter, so dedupe only this socket.
                                                if frame.seq <= connection_seq { continue; }
                                                connection_seq = frame.seq;
                                                last_seq.store(frame.seq, Ordering::SeqCst);
                                            }
                                            // The journaled cancellation
                                            // answers a parked requester
                                            // the way its lost reply
                                            // would have — typically a
                                            // reconnect's tail replay.
                                            if let SessionEvent::QueuedEntryCancelled {
                                                request_id: Some(request_id),
                                                session_id,
                                                entry_id,
                                                text,
                                                images,
                                            } = &event
                                            {
                                                let waiter = pending_requests
                                                    .lock()
                                                    .expect("command waiters poisoned")
                                                    .remove(request_id);
                                                if let Some(waiter) = waiter {
                                                    let _ = waiter.waiter.send(Ok(
                                                        CommandResponse::CancelledQueuedMessage {
                                                            session_id: session_id.clone(),
                                                            entry_id: entry_id.clone(),
                                                            text: text.clone(),
                                                            images: images.clone(),
                                                        },
                                                    ));
                                                }
                                            }
                                            Self::fanout(&subscribers, event);
                                        } else {
                                            Self::fanout(&subscribers, SessionEvent::DaemonError {
                                                session_id: None,
                                                message: "undecodable daemon event: empty frame"
                                                    .to_string(),
                                            });
                                        }
                                    }
                                    Err(error) => {
                                        Self::fanout(&subscribers, SessionEvent::DaemonError {
                                            session_id: None,
                                            message: format!(
                                                "undecodable daemon event: {error}"
                                            ),
                                        });
                                    }
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                ping_sent_at = None;
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if socket.send(Message::Pong(payload)).await.is_err() {
                                    break;
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Err(error)) => {
                                tracing::warn!("daemon socket error: {error}");
                                break;
                            }
                            // Ping/Pong and binary frames carry no session
                            // semantics on this transport.
                            Some(Ok(_)) => {}
                        }
                    }
                }
            }
        }
    }

    fn fanout(
        subscribers: &Arc<Mutex<Vec<mpsc::UnboundedSender<SessionEvent>>>>,
        event: SessionEvent,
    ) {
        let mut subscribers = subscribers.lock().expect("daemon subscribers poisoned");
        // Prune dead subscribers as we go — a dropped view must not wedge
        // the fanout list.
        subscribers.retain(|tx| tx.send(event.clone()).is_ok());
    }
}

impl RemoteDaemon {
    /// Send a `CommandRequest` and wait out `COMMAND_REQUEST_TIMEOUT` for
    /// the matching reply. Runs on whatever Tokio context the caller
    /// supplies — `command_request` guarantees one exists.
    async fn answer_request(
        command_tx: &mpsc::UnboundedSender<OutboundMessage>,
        pending_requests: &Arc<Mutex<HashMap<u64, PendingRequest>>>,
        connected: &Arc<AtomicBool>,
        protocol_version: &Arc<AtomicU64>,
        request: CommandRequest,
    ) -> Result<CommandResponse, String> {
        if !connected.load(Ordering::SeqCst) {
            return Err("daemon is not connected".to_string());
        }
        // A pre-envelope daemon rejects the frame as an undecodable bare
        // command — the dispatch never runs and no reply ever comes, so
        // fail the request here instead of parking it on the timeout.
        let peer_version = protocol_version.load(Ordering::SeqCst);
        if peer_version < COMMAND_REQUEST_PROTOCOL_VERSION {
            return Err(format!(
                "daemon does not support command requests (protocol version {peer_version})"
            ));
        }
        if matches!(&request.command, SessionCommand::EditorLsp { .. })
            && peer_version < EDITOR_LSP_PROTOCOL_VERSION
        {
            return Err(format!(
                "Editor LSP requires protocol v{EDITOR_LSP_PROTOCOL_VERSION}; attached daemon uses v{peer_version}"
            ));
        }
        if matches!(request.command, SessionCommand::SearchProjectFiles { .. } | SessionCommand::ValidateSearchTarget { .. })
            && peer_version < threadlane_protocol::daemon::FILE_SEARCH_PROTOCOL_VERSION {
            return Err(format!("Find in files requires protocol v6; attached daemon uses v{peer_version}"));
        }
        if matches!(
            request.command,
            SessionCommand::ReadProjectFileVersioned { .. }
                | SessionCommand::WriteProjectFileGuarded { .. }
        ) && peer_version < GUARDED_SAVE_PROTOCOL_VERSION
        {
            return Err(format!(
                "Guarded file saves require protocol v{GUARDED_SAVE_PROTOCOL_VERSION}; attached daemon uses v{peer_version}"
            ));
        }
        let request_id = request.request_id;
        let replayable = matches!(request.command, SessionCommand::CancelQueuedMessage { .. });
        let (tx, rx) = oneshot::channel();
        // A reused id would strand the earlier waiter on a reply meant for
        // the newer request — fail it immediately instead.
        if let Some(displaced) = pending_requests
            .lock()
            .expect("command waiters poisoned")
            .insert(
                request_id,
                PendingRequest {
                    waiter: tx,
                    replayable,
                    survived_disconnect: false,
                },
            )
        {
            let _ = displaced
                .waiter
                .send(Err(format!("request id {request_id} reused")));
        }
        if command_tx.send(OutboundMessage::Request(request)).is_err() {
            pending_requests
                .lock()
                .expect("command waiters poisoned")
                .remove(&request_id);
            return Err("daemon connection driver is gone".to_string());
        }
        match tokio::time::timeout(COMMAND_REQUEST_TIMEOUT, rx).await {
            Ok(result) => result.unwrap_or_else(|_| {
                Err(format!(
                    "{}daemon connection driver is gone",
                    Self::UNKNOWN_REQUEST_OUTCOME
                ))
            }),
            Err(_) => {
                pending_requests
                    .lock()
                    .expect("command waiters poisoned")
                    .remove(&request_id);
                Err(format!(
                    "{}daemon did not answer request {request_id}",
                    Self::UNKNOWN_REQUEST_OUTCOME
                ))
            }
        }
    }
}

#[async_trait]
impl DaemonClient for RemoteDaemon {
    async fn command(&self, command: SessionCommand) -> Result<(), String> {
        self.send(command)
    }

    async fn command_request(&self, request: CommandRequest) -> Result<CommandResponse, String> {
        // `tokio::time::timeout` needs a timer driver — GPUI's background
        // executor has none and `answer_request` would panic inside it.
        // Hop the request onto the shared Threadlane reactor when the
        // caller's context lacks Tokio; the driver's Tokio context then
        // serves the timeout.
        if tokio::runtime::Handle::try_current().is_err() {
            let executor = self
                .executor
                .clone()
                .ok_or("daemon event executor unavailable")?;
            let command_tx = self.command_tx.clone();
            let pending_requests = self.pending_requests.clone();
            let connected = self.connected.clone();
            let protocol_version = self.protocol_version.clone();
            return executor
                .spawn(async move {
                    Self::answer_request(
                        &command_tx,
                        &pending_requests,
                        &connected,
                        &protocol_version,
                        request,
                    )
                    .await
                })
                .await
                .unwrap_or_else(|error| Err(format!("request driver failed: {error}")));
        }
        Self::answer_request(
            &self.command_tx,
            &self.pending_requests,
            &self.connected,
            &self.protocol_version,
            request,
        )
        .await
    }

    fn supports_command_requests(&self) -> bool {
        self.protocol_version.load(Ordering::SeqCst) >= COMMAND_REQUEST_PROTOCOL_VERSION
    }

    fn supports_editor_lsp(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
            && self.protocol_version.load(Ordering::SeqCst) >= EDITOR_LSP_PROTOCOL_VERSION
    }

    fn is_connected(&self) -> bool { self.connected.load(Ordering::SeqCst) }

    fn file_search_connection_epoch(&self) -> u64 { self.connection_epoch.load(Ordering::SeqCst) }

    fn supports_file_search(&self) -> bool { self.connected.load(Ordering::SeqCst) && self.protocol_version.load(Ordering::SeqCst) >= threadlane_protocol::daemon::FILE_SEARCH_PROTOCOL_VERSION }

    fn supports_guarded_saves(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
            && self.protocol_version.load(Ordering::SeqCst) >= GUARDED_SAVE_PROTOCOL_VERSION
    }

    fn supports_project_io(&self) -> bool {
        self.protocol_version.load(Ordering::SeqCst) >= PROJECT_IO_PROTOCOL_VERSION
    }

    fn supports_github_automation(&self) -> bool {
        self.protocol_version.load(Ordering::SeqCst)
            >= threadlane_protocol::daemon::GITHUB_AUTOMATION_PROTOCOL_VERSION
    }

    fn subscribe(&self) -> mpsc::UnboundedReceiver<SessionEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        // Errors raised before this receiver existed (transport policy,
        // executor startup) are delivered first, then live events.
        for event in self
            .startup_errors
            .lock()
            .expect("startup errors poisoned")
            .drain(..)
        {
            if tx.send(event).is_err() {
                return rx;
            }
        }
        self.subscribers
            .lock()
            .expect("daemon subscribers poisoned")
            .push(tx);
        rx
    }
}

impl Drop for RemoteDaemon {
    fn drop(&mut self) {
        if let Some(driver) = self
            .driver
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            driver.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pairing_reconnect_wake_reuses_the_saved_credential_and_identity() {
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                    .await
                    .expect("client did not reconnect")
                    .unwrap();
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    socket,
                    move |request: &Request, mut response: Response| {
                        assert_eq!(
                            request
                                .headers()
                                .get("authorization")
                                .and_then(|value| value.to_str().ok()),
                            Some("Bearer same-device-token")
                        );
                        assert_eq!(
                            request
                                .headers()
                                .get("x-threadlane-device-name")
                                .and_then(|value| value.to_str().ok()),
                            Some("Test mobile")
                        );
                        response.headers_mut().insert(
                            "x-threadlane-device-id",
                            "device-stable-id".parse().unwrap(),
                        );
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                let inventory = tokio::time::timeout(Duration::from_secs(3), socket.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .into_text()
                    .unwrap();
                assert!(matches!(
                    serde_json::from_str::<SessionCommand>(&inventory).unwrap(),
                    SessionCommand::GetProjects
                ));
                socket.close(None).await.unwrap();
            }
        });

        let client = RemoteDaemon::connect_pairing_named(
            format!("ws://{address}"),
            "same-device-token".into(),
            Some("Test mobile".into()),
            tokio::runtime::Handle::current(),
        )
        .unwrap();
        let mut connection = client.subscribe_connection();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if *connection.borrow_and_update() == ConnectionState::Connected {
                    break;
                }
                connection.changed().await.unwrap();
            }
        })
        .await
        .expect("first authenticated connection did not complete");
        assert_eq!(client.paired_device_id().as_deref(), Some("device-stable-id"));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if *connection.borrow_and_update() == ConnectionState::Reconnecting {
                    break;
                }
                connection.changed().await.unwrap();
            }
        })
        .await
        .expect("first disconnect did not enter reconnecting state");
        client.request_reconnect();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if *connection.borrow_and_update() == ConnectionState::Connected {
                    break;
                }
                connection.changed().await.unwrap();
            }
        })
        .await
        .expect("foreground wake did not reconnect");
        assert_eq!(client.paired_device_id().as_deref(), Some("device-stable-id"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_wake_during_handshake_preserves_the_established_socket() {
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            accepted_tx.send(()).unwrap();
            release_rx.await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                socket,
                |_request: &Request, mut response: Response| {
                    response
                        .headers_mut()
                        .insert(PROTOCOL_VERSION_HEADER, "6".parse().unwrap());
                    Ok(response)
                },
            )
            .await
            .unwrap();

            let initial = socket.next().await.unwrap().unwrap().into_text().unwrap();
            assert!(matches!(
                serde_json::from_str::<SessionCommand>(&initial).unwrap(),
                SessionCommand::GetProjects
            ));
            let request = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let request: CommandRequest = serde_json::from_str(&request).unwrap();
            assert!(matches!(request.command, SessionCommand::GetProjects));
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "response": CommandReply {
                            request_id: request.request_id,
                            result: Ok(CommandResponse::Ack),
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            match tokio::time::timeout(Duration::from_millis(250), socket.next()).await {
                Err(_) => {}
                Ok(Some(Ok(message))) => {
                    panic!("established socket received an unexpected frame: {message:?}")
                }
                Ok(Some(Err(error))) => panic!("established socket closed with error: {error}"),
                Ok(None) => panic!("established socket closed after handshake wake"),
            }
        });

        let client = RemoteDaemon::connect_with_runtime(
            format!("ws://{address}"),
            None,
            tokio::runtime::Handle::current(),
        );
        accepted_rx.await.unwrap();
        client.request_reconnect();
        release_tx.send(()).unwrap();
        let mut connection = client.subscribe_connection();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if *connection.borrow_and_update() == ConnectionState::Connected {
                    break;
                }
                connection.changed().await.unwrap();
            }
        })
        .await
        .expect("gated handshake did not complete");
        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                client.request(SessionCommand::GetProjects),
            )
            .await
            .expect("pending request did not complete"),
            Ok(CommandResponse::Ack)
        ));
        assert!(client.is_connected());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stalled_websocket_handshake_is_bounded_and_retried() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stalled, _) = listener.accept().await.unwrap();
            let (_retry, _) = tokio::time::timeout(
                CONNECT_HANDSHAKE_TIMEOUT + Duration::from_secs(5),
                listener.accept(),
            )
            .await
            .expect("client did not retry after the bounded handshake")
            .unwrap();
            drop(stalled);
        });
        let client = RemoteDaemon::connect_with_runtime(
            format!("ws://{address}"),
            None,
            tokio::runtime::Handle::current(),
        );
        let mut connection = client.subscribe_connection();
        tokio::time::timeout(
            CONNECT_HANDSHAKE_TIMEOUT + Duration::from_secs(3),
            async {
                loop {
                    if *connection.borrow_and_update() == ConnectionState::Reconnecting {
                        break;
                    }
                    connection.changed().await.unwrap();
                }
            },
        )
        .await
        .expect("stalled handshake did not enter reconnecting state");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn unauthorized_pairing_stops_with_repair_guidance() {
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let rejected = tokio_tungstenite::accept_hdr_async(
                socket,
                |_request: &Request, _response: Response| {
                    Err(
                        tokio_tungstenite::tungstenite::http::Response::builder()
                            .status(401)
                            .body(Some("unauthorized".to_string()))
                            .unwrap(),
                    )
                },
            )
            .await;
            assert!(rejected.is_err());
        });
        let client = RemoteDaemon::connect_pairing(
            format!("ws://{address}"),
            "revoked-token".into(),
            tokio::runtime::Handle::current(),
        )
        .unwrap();
        let mut connection = client.subscribe_connection();
        let state = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let state = connection.borrow_and_update().clone();
                match state {
                    ConnectionState::Failed(message) => break message,
                    _ => connection.changed().await.unwrap(),
                }
            }
        })
        .await
        .expect("unauthorized pairing was not surfaced");
        assert!(state.contains("Remove it and pair again"), "{state}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn idle_connections_request_inventory_on_every_epoch() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for epoch in 1..=2 {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                // An idle daemon sends no replay events: refresh must be client-driven.
                let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
                    .await.expect("missing inventory refresh").unwrap().unwrap();
                let command: SessionCommand = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                assert!(matches!(command, SessionCommand::GetProjects), "epoch {epoch}");
                socket.close(None).await.unwrap();
            }
        });
        let _client = RemoteDaemon::connect_with_runtime(
            format!("ws://{address}"), None, tokio::runtime::Handle::current(),
        );
        tokio::time::timeout(Duration::from_secs(10), server).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn file_search_old_daemon_fails_before_enqueue() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let connected = Arc::new(AtomicBool::new(true));
        let version = Arc::new(AtomicU64::new(5));
        let result = RemoteDaemon::answer_request(&tx, &pending, &connected, &version, CommandRequest {
            request_id: 99,
            command: SessionCommand::SearchProjectFiles { work_dir: "/remote-only".into(), query: "private query".into() },
        }).await;
        assert!(result.unwrap_err().contains("requires protocol v6"));
        assert!(rx.try_recv().is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn editor_lsp_request_is_version_gated_before_enqueue() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let connected = Arc::new(AtomicBool::new(true));
        let version = Arc::new(AtomicU64::new(7));
        let result = RemoteDaemon::answer_request(
            &tx,
            &pending,
            &connected,
            &version,
            CommandRequest {
                request_id: 100,
                command: SessionCommand::EditorLsp {
                    request: threadlane_protocol::editor_lsp::EditorLspRequest {
                        session_id: "session".into(),
                        work_dir: "/remote-only".into(),
                        path: "src/main.rs".into(),
                        document_id: 1,
                        version: 1,
                        expected_runtime_id: None,
                        text: "fn main() {}".into(),
                        position: threadlane_protocol::editor_lsp::EditorLspPosition::default(),
                        operation: threadlane_protocol::editor_lsp::EditorLspOperation::Hover,
                    },
                },
            },
        )
        .await;
        assert!(result.unwrap_err().contains("requires protocol v8"));
        assert!(rx.try_recv().is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn editor_lsp_bare_command_is_not_sent_to_old_daemon() {
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                socket,
                |_request: &Request, mut response: Response| {
                    response
                        .headers_mut()
                        .insert(PROTOCOL_VERSION_HEADER, "7".parse().unwrap());
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let inventory = tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap();
            assert!(matches!(
                serde_json::from_str::<SessionCommand>(&inventory).unwrap(),
                SessionCommand::GetProjects
            ));
            match tokio::time::timeout(Duration::from_millis(250), socket.next()).await {
                Err(_) => {}
                Ok(Some(Ok(message))) => {
                    panic!("old daemon received an unsupported command: {message:?}")
                }
                Ok(Some(Err(error))) => panic!("old daemon closed with error: {error}"),
                Ok(None) => panic!("old daemon closed the socket"),
            }
        });
        let client = RemoteDaemon::connect_with_runtime(
            format!("ws://{address}"),
            None,
            tokio::runtime::Handle::current(),
        );
        let mut connection = client.subscribe_connection();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if *connection.borrow_and_update() == ConnectionState::Connected {
                    break;
                }
                connection.changed().await.unwrap();
            }
        })
        .await
        .expect("handshake did not complete");
        assert!(!client.supports_editor_lsp());
        let error = client
            .send(SessionCommand::EditorLsp {
                request: threadlane_protocol::editor_lsp::EditorLspRequest {
                    session_id: "session".into(),
                    work_dir: "/remote-only".into(),
                    path: "src/main.rs".into(),
                    document_id: 1,
                    version: 1,
                    expected_runtime_id: None,
                    text: "fn main() {}".into(),
                    position: threadlane_protocol::editor_lsp::EditorLspPosition::default(),
                    operation: threadlane_protocol::editor_lsp::EditorLspOperation::Hover,
                },
            })
            .unwrap_err();
        assert!(error.contains("requires protocol v8"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn guarded_file_requests_are_version_gated_before_enqueue_and_after_reconnect() {
        let request = || CommandRequest {
            request_id: 101,
            command: SessionCommand::WriteProjectFileGuarded {
                work_dir: "/remote-only".into(),
                path: "src/main.rs".into(),
                content: "updated".into(),
                expected_version: "sha256:old".into(),
            },
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let connected = Arc::new(AtomicBool::new(true));
        let version = Arc::new(AtomicU64::new(6));
        let error = RemoteDaemon::answer_request(
            &tx,
            &pending,
            &connected,
            &version,
            request(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("require protocol v7"));
        assert!(rx.try_recv().is_err());
        assert!(pending.lock().unwrap().is_empty());

        // A reconnect can roll the peer back to an older daemon. It must
        // not reuse the previously capable epoch's save permission.
        version.store(7, Ordering::SeqCst);
        connected.store(false, Ordering::SeqCst);
        assert!(RemoteDaemon::answer_request(
            &tx,
            &pending,
            &connected,
            &version,
            request(),
        )
        .await
        .unwrap_err()
        .contains("not connected"));
        assert!(rx.try_recv().is_err());

        connected.store(true, Ordering::SeqCst);
        version.store(6, Ordering::SeqCst);
        assert!(RemoteDaemon::answer_request(
            &tx,
            &pending,
            &connected,
            &version,
            request(),
        )
        .await
        .unwrap_err()
        .contains("require protocol v7"));
        assert!(rx.try_recv().is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn guarded_file_requests_are_enqueued_for_protocol_seven() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let connected = Arc::new(AtomicBool::new(true));
        let version = Arc::new(AtomicU64::new(7));
        let task = tokio::spawn({
            let pending = pending.clone();
            let connected = connected.clone();
            let version = version.clone();
            async move {
                RemoteDaemon::answer_request(
                    &tx,
                    &pending,
                    &connected,
                    &version,
                    CommandRequest {
                        request_id: 102,
                        command: SessionCommand::ReadProjectFileVersioned {
                            work_dir: "/remote-only".into(),
                            path: "src/main.rs".into(),
                        },
                    },
                )
                .await
            }
        });
        let Some(OutboundMessage::Request(request)) = rx.recv().await else {
            panic!("protocol 7 request was not enqueued");
        };
        assert_eq!(request.request_id, 102);
        assert!(matches!(
            request.command,
            SessionCommand::ReadProjectFileVersioned { .. }
        ));
        pending
            .lock()
            .unwrap()
            .remove(&102)
            .expect("request waiter installed")
            .waiter
            .send(Ok(CommandResponse::VersionedFile {
                result: Ok(threadlane_protocol::daemon::VersionedFile {
                    content: "contents".into(),
                    version: "sha256:new".into(),
                }),
            }))
            .unwrap();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            CommandResponse::VersionedFile { result: Ok(_) }
        ));
    }

    #[test]
    fn pairing_is_explicit_and_confined_to_token_protected_local_or_shared_addresses() {
        for address in [
            "ws://127.0.0.1:8080",
            "ws://192.168.1.10:8080",
            "ws://10.0.0.1:8080",
            "ws://100.64.0.0:8080",
            "ws://100.101.102.103:8080",
            "ws://100.127.255.255:8080",
            "ws://[::1]:8080",
        ] {
            assert!(
                RemoteDaemon::validate_pairing(address, "token").is_ok(),
                "{address}"
            );
        }
        for address in [
            "ws://8.8.8.8:8080",
            "ws://100.63.255.255:8080",
            "ws://100.128.0.0:8080",
            "ws://example.com:8080",
            "ws://desktop.example.ts.net:8080",
            "wss://192.168.1.10:8080",
            "wss://100.101.102.103:8080",
        ] {
            assert!(
                RemoteDaemon::validate_pairing(address, "token").is_err(),
                "{address}"
            );
        }
        assert!(RemoteDaemon::validate_pairing("ws://127.0.0.1:8080", "").is_err());
        for token in ["", " ", "invalid\r\ntoken"] {
            assert!(RemoteDaemon::validate_pairing("ws://100.101.102.103:8080", token).is_err());
        }
        assert!(RemoteDaemon::transport_policy_error(
            "ws://100.101.102.103:8080",
            &Some("token".into())
        )
        .is_some());
        assert!(RemoteDaemon::transport_policy_error(
            "ws://192.168.1.10:8080",
            &Some("token".into())
        )
        .is_some());
        assert!(RemoteDaemon::transport_policy_error(
            "wss://example.com:8080",
            &Some("token".into())
        )
        .is_none());
    }
    #[tokio::test]
    async fn reconnect_replay_is_deduplicated_and_requests_are_acknowledged() {
        use threadlane_protocol::AgentEvent;
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for attempt in 0..3 {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    socket,
                    move |request: &Request, mut response: Response| {
                        if attempt > 0 {
                            assert!(request
                                .uri()
                                .query()
                                .unwrap_or_default()
                                .contains(if attempt == 1 { "since=1" } else { "since=2" }));
                        }
                        response
                            .headers_mut()
                            .insert(PROTOCOL_VERSION_HEADER, "3".parse().unwrap());
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                let frame = socket.next().await.unwrap().unwrap().into_text().unwrap();
                assert!(matches!(serde_json::from_str::<SessionCommand>(&frame).unwrap(), SessionCommand::GetProjects));
                // Same daemon resumes at 2; a restarted daemon starts at 1.
                let seq = if attempt == 1 { 2 } else { 1 };
                for _duplicate in 0..2 {
                    let event = SessionEvent::Agent {
                        session_id: "session".into(),
                        event: AgentEvent::MessageUpdate {
                            text_delta: Some(seq.to_string()),
                            reasoning_delta: None,
                            tool_call_name: None,
                        },
                    };
                    socket
                        .send(Message::Text(
                            serde_json::json!({"seq":seq,"event":event})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .unwrap();
                }
                if attempt == 2 {
                    let frame = socket.next().await.unwrap().unwrap().into_text().unwrap();
                    let request: CommandRequest = serde_json::from_str(&frame).unwrap();
                    assert!(matches!(request.command, SessionCommand::GetProjects));
                    let reply = CommandReply {
                        request_id: request.request_id,
                        result: Ok(CommandResponse::Ack),
                    };
                    socket
                        .send(Message::Text(
                            serde_json::json!({"response":reply}).to_string().into(),
                        ))
                        .await
                        .unwrap();
                }
                if attempt == 2 {
                    // The server accepts a prompt, but its reply is lost.
                    let frame = socket.next().await.unwrap().unwrap().into_text().unwrap();
                    let request: CommandRequest = serde_json::from_str(&frame).unwrap();
                    assert!(matches!(
                        request.command,
                        SessionCommand::SubmitPrompt { .. }
                    ));
                }
                socket.close(None).await.unwrap();
            }
        });
        let client = RemoteDaemon::connect_with_runtime(
            format!("ws://{address}"),
            None,
            tokio::runtime::Handle::current(),
        );
        let mut events = client.subscribe();
        let received = tokio::time::timeout(Duration::from_secs(10), async {
            let mut text = String::new();
            while text.len() < 3 {
                if let Some(SessionEvent::Agent {
                    event:
                        AgentEvent::MessageUpdate {
                            text_delta: Some(delta),
                            ..
                        },
                    ..
                }) = events.recv().await
                {
                    text.push_str(&delta);
                }
            }
            text
        })
        .await
        .unwrap();
        assert_eq!(received, "121");
        assert!(matches!(
            client.request(SessionCommand::GetProjects).await,
            Ok(CommandResponse::Ack)
        ));
        let error = client
            .request(SessionCommand::SubmitPrompt {
                session_id: "session".into(),
                work_dir: "/tmp".into(),
                text: "accepted before disconnect".into(),
                images: Vec::new(),
                effort: None,
                acp_config: Vec::new(),
                model: None,
            })
            .await
            .unwrap_err();
        assert!(
            error.starts_with(RemoteDaemon::UNKNOWN_REQUEST_OUTCOME),
            "{error}"
        );
        server.await.unwrap();
    }
}
