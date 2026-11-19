//! The hub's client interface over iroh (Plan.md §3.5): QUIC between nodes
//! dialed by public key, with no relays and no public address lookup.
//!
//! Two protocols, told apart by ALPN:
//!
//! - [`CLIENT_ALPN`], for admitted keys only. The hub checks the remote key
//!   right after the handshake and closes the connection before reading
//!   anything if the key is not admitted, or as soon as it is revoked. Each
//!   request has its own stream, framed as in `threadlane-transport-local`:
//!   one JSON [`ClientRequest`] line in, one [`ClientResponse`] line out. A
//!   stream that opens with `{"op":"follow","after":N}` instead stays open and
//!   carries the pushed feed, one `ClientResponse::Feed` line per delivery.
//! - [`JOIN_ALPN`], the only thing a key not yet admitted may do: one stream,
//!   one bounded line with a join secret, one answer, then the hub closes.

use std::fmt;
use std::fs;
use std::future::Future;
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use iroh::RelayMode;
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt, presets};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use serde::{Deserialize, Serialize};
use threadlane_core::client::{
    Accepted, ApiError, ClientRequest, ClientResponse, DecisionReply, DecisionRequest, FeedItem,
    HubApi, HubInfo, MemoryReply, MemoryRequest, OrgReply, OrgRequest, OverviewReply,
    OverviewRequest, PairingReply, PairingRequest, ProjectSummary, UserMessage, dispatch,
};
use threadlane_core::client::{
    OperationsReply, OperationsRequest, TaskReply, TaskRequest, TimerReply, TimerRequest,
};
use threadlane_transport_local::{MAX_FEED_WAIT, cap_wait, encode, unexpected};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinSet;

pub use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, TransportAddr};

/// The client interface, for admitted keys.
pub const CLIENT_ALPN: &[u8] = b"threadlane/client/1";

/// The pairing exchange, for a key presenting a join secret.
pub const JOIN_ALPN: &[u8] = b"threadlane/join/1";

/// The local-network service name hubs announce themselves under.
const SERVICE_NAME: &str = "threadlane";

/// Close code: the key is not (or no longer) admitted.
const NOT_ADMITTED: u32 = 1;
/// Close code: the pairing exchange is over.
const JOIN_DONE: u32 = 2;

/// The largest request an admitted client may send.
const MAX_REQUEST: usize = 4 << 20;
/// The largest answer or pushed frame a client accepts from its hub.
const MAX_RESPONSE: usize = 64 << 20;
/// The largest pairing message; a join secret fits many times over.
const MAX_JOIN_MESSAGE: usize = 512;

/// How long a dial may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a client waits for the answer to a request that does not wait on
/// the feed, and how long the hub waits for a request to arrive.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Added to a feed wait on the client side, as over the Unix socket.
const FEED_MARGIN: Duration = Duration::from_secs(10);
/// How long each step of the pairing exchange may take.
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Waits between attempts to resume the pushed feed.
const RESUME_BACKOFF: Duration = Duration::from_millis(200);
const MAX_RESUME_BACKOFF: Duration = Duration::from_secs(10);

/// Binds an endpoint for the hub: it accepts both protocols and, with
/// `local_discovery`, announces itself on the local network. No relays and
/// no public address lookup. `bind: None` listens on every interface, IPv4 and IPv6, on a free port.
///
/// # Errors
///
/// Fails if `bind` cannot be bound or local discovery cannot start.
pub async fn hub_endpoint(
    secret: SecretKey,
    bind: Option<SocketAddr>,
    local_discovery: bool,
) -> io::Result<Endpoint> {
    endpoint(secret, bind, local_discovery, true).await
}

async fn endpoint(
    secret: SecretKey,
    bind: Option<SocketAddr>,
    local_discovery: bool,
    serve: bool,
) -> io::Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(secret);
    if let Some(bind) = bind {
        builder = builder
            .clear_ip_transports()
            .bind_addr(bind)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    }
    if serve {
        builder = builder.alpns(vec![CLIENT_ALPN.to_vec(), JOIN_ALPN.to_vec()]);
    }
    if local_discovery {
        // Only hubs are ever dialed (Plan.md §3.5), so only they announce.
        builder = builder.address_lookup(
            MdnsAddressLookup::builder()
                .service_name(SERVICE_NAME)
                .advertise(serve),
        );
    }
    builder
        .bind()
        .await
        .map_err(|e| io::Error::other(format!("cannot bind the iroh endpoint: {e}")))
}

/// Reads the secret key in `path`, or creates a new one there, readable only
/// by its owner, if there is no file yet.
///
/// # Errors
///
/// As [`load_key`] and [`create_key`].
pub fn load_or_create_key(path: &Path) -> io::Result<SecretKey> {
    match load_key(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_key(path),
        other => other,
    }
}

/// Reads the secret key in `path`.
///
/// # Errors
///
/// Fails if the file cannot be read, does not hold a key, or can be read by
/// anyone but its owner.
pub fn load_key(path: &Path) -> io::Result<SecretKey> {
    let mode = fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} can be read by others (mode {:o}); it must be readable only by its owner",
                path.display(),
                mode & 0o777
            ),
        ));
    }
    let text = fs::read_to_string(path)?;
    let bytes = unhex(text.trim()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} does not hold a key", path.display()),
        )
    })?;
    Ok(SecretKey::from_bytes(&bytes))
}

/// Creates a new secret key in `path`, readable only by its owner. Never
/// replaces a file.
///
/// # Errors
///
/// Fails if the file exists or cannot be written.
pub fn create_key(path: &Path) -> io::Result<SecretKey> {
    let secret = SecretKey::generate();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(format!("{}\n", hex(&secret.to_bytes())).as_bytes())?;
    file.sync_all()?;
    Ok(secret)
}

/// The hub's address as a join code carries it: its id and the addresses it
/// listens on, leaving out unspecified ones.
#[must_use]
pub fn hub_addr(endpoint: &Endpoint) -> EndpointAddr {
    let mut addrs: Vec<SocketAddr> = endpoint.addr().ip_addrs().copied().collect();
    addrs.extend(
        endpoint
            .bound_sockets()
            .into_iter()
            .filter(|addr| !addr.ip().is_unspecified()),
    );
    addrs.sort_unstable();
    addrs.dedup();
    EndpointAddr::from_parts(endpoint.id(), addrs.into_iter().map(TransportAddr::Ip))
}

/// A one-time join code: the hub's id, how to reach it, and a secret.
///
/// Written as `tl1-<hub id>-<secret>-<addr>,<addr>…`, ids and secrets in hex.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCode {
    pub hub: EndpointAddr,
    pub secret: [u8; 32],
}

impl JoinCode {
    /// A code with a fresh random secret.
    #[must_use]
    pub fn new(hub: EndpointAddr) -> Self {
        // A secret key is 32 bytes from the system's random source.
        let secret = SecretKey::generate().to_bytes();
        Self { hub, secret }
    }
}

impl fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinCode")
            .field("hub", &self.hub)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for JoinCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let addrs: Vec<String> = self.hub.ip_addrs().map(ToString::to_string).collect();
        write!(
            f,
            "tl1-{}-{}-{}",
            self.hub.id,
            hex(&self.secret),
            addrs.join(",")
        )
    }
}

impl FromStr for JoinCode {
    type Err = String;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        let invalid = || "not a Threadlane join code".to_owned();
        let mut parts = code.trim().splitn(4, '-');
        if parts.next() != Some("tl1") {
            return Err(invalid());
        }
        let id = EndpointId::from_str(parts.next().ok_or_else(invalid)?).map_err(|_| invalid())?;
        let secret = unhex(parts.next().ok_or_else(invalid)?).ok_or_else(invalid)?;
        let addrs = parts
            .next()
            .unwrap_or_default()
            .split(',')
            .filter(|addr| !addr.is_empty())
            .map(|addr| addr.parse::<SocketAddr>().map(TransportAddr::Ip))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid())?;
        Ok(Self {
            hub: EndpointAddr::from_parts(id, addrs),
            secret,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

/// Who may use the hub: the admitted list, joining, and revocation.
#[async_trait]
pub trait Gate: Send + Sync + fmt::Debug {
    /// Whether `key` is admitted now.
    async fn is_admitted(&self, key: EndpointId) -> bool;
    /// Admits `key` if `secret` belongs to an unused, unexpired join code.
    async fn join(&self, key: EndpointId, secret: &[u8; 32]) -> Result<(), ApiError>;
    /// Changes whenever a key is revoked; open connections then check again.
    fn revocations(&self) -> watch::Receiver<u64>;
}

/// The first line of a feed stream.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Follow {
    Follow { after: i64 },
}

/// The pairing exchange's one message from the new node.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinRequest {
    secret: String,
}

/// Accepts connections on `endpoint` until `shutdown` completes, then closes
/// it. Each connection is served by its own task, each stream by its own task
/// within it, so a long feed wait never delays anything else.
pub async fn serve(
    endpoint: Endpoint,
    api: Arc<dyn HubApi>,
    gate: Arc<dyn Gate>,
    shutdown: impl Future<Output = ()> + Send,
) {
    tracing::debug!(id = %endpoint.id(), "serving the client interface over iroh");
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => {
                    connections.spawn(serve_incoming(incoming, Arc::clone(&api), Arc::clone(&gate)));
                }
                None => break,
            },
            Some(_finished) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.abort_all();
    endpoint.close().await;
}

async fn serve_incoming(
    incoming: iroh::endpoint::Incoming,
    api: Arc<dyn HubApi>,
    gate: Arc<dyn Gate>,
) {
    let connecting = match incoming.accept() {
        Ok(connecting) => connecting,
        Err(error) => {
            tracing::debug!(%error, "refused an incoming connection");
            return;
        }
    };
    let connection = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            tracing::debug!(%error, "an incoming handshake failed");
            return;
        }
        Err(_elapsed) => return,
    };
    let alpn = connection.alpn().to_vec();
    if alpn == CLIENT_ALPN {
        serve_client(connection, api, gate).await;
    } else if alpn == JOIN_ALPN {
        serve_join(connection, gate).await;
    } else {
        connection.close(VarInt::from_u32(NOT_ADMITTED), b"unknown protocol");
    }
}

async fn serve_client(connection: Connection, api: Arc<dyn HubApi>, gate: Arc<dyn Gate>) {
    let key = connection.remote_id();
    let mut revocations = gate.revocations();
    // Seen before checking, so a revocation during the check is not missed.
    revocations.borrow_and_update();
    if !gate.is_admitted(key).await {
        tracing::info!(key = %key.fmt_short(), "closed a connection from a key that is not admitted");
        connection.close(VarInt::from_u32(NOT_ADMITTED), b"not admitted");
        return;
    }
    let mut streams = JoinSet::new();
    loop {
        tokio::select! {
            stream = connection.accept_bi() => match stream {
                Ok((send, recv)) => {
                    streams.spawn(serve_stream(send, recv, Arc::clone(&api)));
                }
                Err(_closed) => break,
            },
            changed = revocations.changed() => {
                if changed.is_err() || !gate.is_admitted(key).await {
                    tracing::info!(key = %key.fmt_short(), "closed the connection of a revoked key");
                    connection.close(VarInt::from_u32(NOT_ADMITTED), b"revoked");
                    break;
                }
            }
            Some(_finished) = streams.join_next(), if !streams.is_empty() => {}
        }
    }
    streams.abort_all();
}

async fn serve_stream(mut send: SendStream, recv: RecvStream, api: Arc<dyn HubApi>) {
    let request = match tokio::time::timeout(REQUEST_TIMEOUT, read_message(recv, MAX_REQUEST)).await
    {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => {
            tracing::debug!(%error, "cannot read a client request");
            return;
        }
        Err(_elapsed) => return,
    };
    let response = match serde_json::from_slice::<ClientRequest>(&request) {
        Ok(request) => dispatch(api.as_ref(), cap_wait(request)).await,
        Err(error) => match serde_json::from_slice::<Follow>(&request) {
            Ok(Follow::Follow { after }) => return push_feed(send, api, after).await,
            Err(_) => ClientResponse::Error {
                error: ApiError::Invalid(format!("unparseable request: {error}")),
            },
        },
    };
    let _ignored = write_line(&mut send, &response).await;
    let _ignored = send.finish();
}

/// Pushes every feed item after `after` as it is delivered, until the client
/// stops reading. Rests on the hub's own long-poll, which wakes on delivery.
async fn push_feed(mut send: SendStream, api: Arc<dyn HubApi>, mut after: i64) {
    loop {
        let items = tokio::select! {
            items = api.feed(after, MAX_FEED_WAIT) => items,
            _stopped = send.stopped() => return,
        };
        let frame = match items {
            Ok(items) if items.is_empty() => continue,
            Ok(items) => {
                after = items.iter().map(|item| item.seq).max().unwrap_or(after);
                ClientResponse::Feed { items }
            }
            Err(error) => ClientResponse::Error { error },
        };
        let failed = matches!(frame, ClientResponse::Error { .. });
        if write_line(&mut send, &frame).await.is_err() || failed {
            let _ignored = send.finish();
            return;
        }
    }
}

async fn serve_join(connection: Connection, gate: Arc<dyn Gate>) {
    let key = connection.remote_id();
    let exchange = async {
        let (mut send, recv) = connection.accept_bi().await.ok()?;
        let message = read_message(recv, MAX_JOIN_MESSAGE).await.ok()?;
        let result = match serde_json::from_slice::<JoinRequest>(&message)
            .ok()
            .and_then(|request| unhex(&request.secret))
        {
            Some(secret) => gate.join(key, &secret).await,
            None => Err(ApiError::Invalid("malformed join request".into())),
        };
        let mut line = serde_json::to_vec(&result).ok()?;
        line.push(b'\n');
        send.write_all(&line).await.ok()?;
        send.finish().ok()?;
        // Let the answer reach the node before closing.
        let _acknowledged = send.stopped().await;
        Some(())
    };
    let _done = tokio::time::timeout(JOIN_TIMEOUT, exchange).await;
    connection.close(VarInt::from_u32(JOIN_DONE), b"done");
}

/// Reads a whole stream of at most `limit` bytes.
async fn read_message(mut recv: RecvStream, limit: usize) -> io::Result<Vec<u8>> {
    recv.read_to_end(limit)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

async fn write_line(send: &mut SendStream, response: &ClientResponse) -> io::Result<()> {
    let mut line = encode(response);
    line.push(b'\n');
    send.write_all(&line)
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))
}

/// Pairs `secret`'s key with the hub of `code`: dials it, presents the code's
/// secret once, and reports whether the hub admitted the key.
///
/// # Errors
///
/// [`ApiError::Unreachable`] if the hub cannot be reached, or the hub's own
/// error if it refused the code.
pub async fn join(
    secret: SecretKey,
    code: &JoinCode,
    local_discovery: bool,
) -> Result<(), ApiError> {
    let endpoint = endpoint(secret, None, local_discovery, false)
        .await
        .map_err(|e| ApiError::Unreachable(e.to_string()))?;
    let result = async {
        let connection = dial(&endpoint, code.hub.clone(), JOIN_ALPN).await?;
        let exchange = async {
            let (mut send, recv) = connection.open_bi().await.map_err(broken)?;
            let mut line = serde_json::to_vec(&JoinRequest {
                secret: hex(&code.secret),
            })
            .map_err(|e| ApiError::Internal(e.to_string()))?;
            line.push(b'\n');
            send.write_all(&line).await.map_err(broken)?;
            send.finish().map_err(broken)?;
            let answer = read_message(recv, MAX_JOIN_MESSAGE).await.map_err(broken)?;
            serde_json::from_slice::<Result<(), ApiError>>(&answer)
                .map_err(|e| ApiError::Internal(format!("unreadable answer from the hub: {e}")))?
        };
        let result = tokio::time::timeout(JOIN_TIMEOUT, exchange)
            .await
            .unwrap_or_else(|_elapsed| Err(ApiError::Unreachable("the hub did not answer".into())));
        connection.close(VarInt::from_u32(JOIN_DONE), b"done");
        result
    }
    .await;
    endpoint.close().await;
    result
}

async fn dial(endpoint: &Endpoint, hub: EndpointAddr, alpn: &[u8]) -> Result<Connection, ApiError> {
    let id = hub.id;
    match tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(hub, alpn)).await {
        Ok(Ok(connection)) => Ok(connection),
        Ok(Err(error)) => Err(ApiError::Unreachable(format!(
            "cannot connect to hub {}: {error}",
            id.fmt_short()
        ))),
        Err(_elapsed) => Err(ApiError::Unreachable(format!(
            "cannot connect to hub {} within {} s",
            id.fmt_short(),
            CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

fn broken(error: impl fmt::Display) -> ApiError {
    ApiError::Unreachable(format!("connection to the hub broke: {error}"))
}

/// A [`HubApi`] reached over iroh with one key of this client's own.
///
/// Binds its endpoint and dials on first use, then keeps the connection and
/// opens one stream per request, redialing when the connection is gone. A
/// dial failure, a broken connection, or no answer in time is
/// [`ApiError::Unreachable`].
#[derive(Debug)]
pub struct IrohClient {
    secret: SecretKey,
    hub: EndpointAddr,
    local_discovery: bool,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    endpoint: Option<Endpoint>,
    connection: Option<Connection>,
}

impl IrohClient {
    /// A client that dials `hub` as `secret`'s key. Does not connect yet.
    #[must_use]
    pub fn new(secret: SecretKey, hub: EndpointAddr, local_discovery: bool) -> Self {
        Self {
            secret,
            hub,
            local_discovery,
            state: Mutex::default(),
        }
    }

    /// This client's own id, as the hub sees it.
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.secret.public()
    }

    async fn connection(&self) -> Result<Connection, ApiError> {
        let mut state = self.state.lock().await;
        if let Some(connection) = &state.connection
            && connection.close_reason().is_none()
        {
            return Ok(connection.clone());
        }
        let endpoint = match &state.endpoint {
            Some(endpoint) => endpoint.clone(),
            None => {
                let endpoint = endpoint(self.secret.clone(), None, self.local_discovery, false)
                    .await
                    .map_err(|e| ApiError::Unreachable(e.to_string()))?;
                state.endpoint = Some(endpoint.clone());
                endpoint
            }
        };
        let connection = dial(&endpoint, self.hub.clone(), CLIENT_ALPN).await?;
        state.connection = Some(connection.clone());
        Ok(connection)
    }

    /// Opens a stream and writes `first` as its only line.
    async fn open(&self, first: &impl Serialize) -> Result<RecvStream, ApiError> {
        let mut line = serde_json::to_vec(first)
            .map_err(|e| ApiError::Invalid(format!("cannot encode the request: {e}")))?;
        line.push(b'\n');
        let connection = self.connection().await?;
        let (mut send, recv) = connection.open_bi().await.map_err(broken)?;
        send.write_all(&line).await.map_err(broken)?;
        send.finish().map_err(broken)?;
        Ok(recv)
    }

    async fn call(
        &self,
        request: &ClientRequest,
        limit: Duration,
    ) -> Result<ClientResponse, ApiError> {
        let exchange = async {
            let recv = self.open(request).await?;
            let answer = read_message(recv, MAX_RESPONSE).await.map_err(broken)?;
            if answer.is_empty() {
                return Err(broken("the stream closed before the hub answered"));
            }
            serde_json::from_slice(&answer)
                .map_err(|e| ApiError::Internal(format!("unreadable answer from the hub: {e}")))
        };
        tokio::time::timeout(limit, exchange)
            .await
            .unwrap_or_else(|_elapsed| {
                Err(ApiError::Unreachable(format!(
                    "no answer from hub {} within {} s",
                    self.hub.id.fmt_short(),
                    limit.as_secs()
                )))
            })
    }

    /// The pushed feed: every item after `after`, oldest first, as the hub
    /// delivers it, with no polling. A dropped connection is resumed from the
    /// last item handed out, so nothing is missed or repeated. A failed
    /// attempt to (re)connect is handed out as an error, and the stream keeps
    /// trying with growing pauses until the receiver is dropped.
    #[must_use]
    pub fn follow(self: &Arc<Self>, after: i64) -> mpsc::Receiver<Result<FeedItem, ApiError>> {
        let (items, receiver) = mpsc::channel(64);
        let client = Arc::clone(self);
        tokio::spawn(async move {
            let mut after = after;
            let mut backoff = RESUME_BACKOFF;
            loop {
                let outcome = tokio::select! {
                    outcome = client.follow_once(&mut after, &items) => outcome,
                    () = items.closed() => return,
                };
                match outcome {
                    // Streamed, then dropped: resume at once.
                    Ok(true) => backoff = RESUME_BACKOFF,
                    Ok(false) => return,
                    Err(error) => {
                        if items.send(Err(error)).await.is_err() {
                            return;
                        }
                        tokio::select! {
                            () = tokio::time::sleep(backoff) => {}
                            () = items.closed() => return,
                        }
                        backoff = (backoff * 2).min(MAX_RESUME_BACKOFF);
                    }
                }
            }
        });
        receiver
    }

    /// One feed stream. `Ok(true)` if it was established and then ended,
    /// `Ok(false)` if the receiver is gone, an error if it never got going.
    async fn follow_once(
        &self,
        after: &mut i64,
        items: &mpsc::Sender<Result<FeedItem, ApiError>>,
    ) -> Result<bool, ApiError> {
        let recv = self.open(&Follow::Follow { after: *after }).await?;
        let mut reader = BufReader::new(recv);
        let mut established = false;
        loop {
            let mut line = Vec::new();
            let read = (&mut reader)
                .take(MAX_RESPONSE as u64)
                .read_until(b'\n', &mut line)
                .await;
            if read.is_err() || line.last() != Some(&b'\n') {
                // The connection broke, or the line was cut off at the bound.
                return if established {
                    Ok(true)
                } else {
                    Err(broken("the feed stream ended"))
                };
            }
            established = true;
            let frame: ClientResponse = serde_json::from_slice(&line)
                .map_err(|e| ApiError::Internal(format!("unreadable feed from the hub: {e}")))?;
            match frame {
                ClientResponse::Feed { items: delivered } => {
                    for item in delivered {
                        if item.seq <= *after {
                            continue;
                        }
                        let seq = item.seq;
                        if items.send(Ok(item)).await.is_err() {
                            return Ok(false);
                        }
                        *after = seq;
                    }
                }
                other => return Err(unexpected(other, "follow")),
            }
        }
    }

    /// Closes the connection and the endpoint, telling the hub at once.
    pub async fn close(&self) {
        let mut state = self.state.lock().await;
        state.connection = None;
        if let Some(endpoint) = state.endpoint.take() {
            endpoint.close().await;
        }
    }
}

#[async_trait]
impl HubApi for IrohClient {
    async fn update_hub(
        &self,
        request: threadlane_core::client::HubUpdate,
    ) -> Result<Option<String>, ApiError> {
        match self
            .call(
                &ClientRequest::UpdateHub { request },
                Duration::from_secs(360),
            )
            .await?
        {
            ClientResponse::UpdateHub { version } => Ok(version),
            other => Err(unexpected(other, "hub update")),
        }
    }

    async fn info(&self) -> Result<HubInfo, ApiError> {
        match self.call(&ClientRequest::Info, REQUEST_TIMEOUT).await? {
            ClientResponse::Info { info } => Ok(info),
            other => Err(unexpected(other, "info")),
        }
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        self.project_list(false).await
    }

    async fn project_list(&self, include_archived: bool) -> Result<Vec<ProjectSummary>, ApiError> {
        let request = ClientRequest::Projects { include_archived };
        match self.call(&request, REQUEST_TIMEOUT).await? {
            ClientResponse::Projects { projects } => Ok(projects),
            other => Err(unexpected(other, "projects")),
        }
    }

    async fn send_message(&self, message: &UserMessage) -> Result<Accepted, ApiError> {
        let request = if message.images.is_empty() {
            ClientRequest::SendMessage {
                message: message.clone(),
            }
        } else {
            ClientRequest::SendMessageWithImages {
                message: message.clone(),
            }
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

    async fn memory(&self, request: MemoryRequest) -> Result<MemoryReply, ApiError> {
        match self
            .call(&ClientRequest::Memory { request }, REQUEST_TIMEOUT)
            .await?
        {
            ClientResponse::Memory { reply } => Ok(reply),
            other => Err(unexpected(other, "memory")),
        }
    }

    async fn timers(&self, request: TimerRequest) -> Result<TimerReply, ApiError> {
        match self
            .call(&ClientRequest::Timers { request }, REQUEST_TIMEOUT)
            .await?
        {
            ClientResponse::Timers { reply } => Ok(reply),
            other => Err(unexpected(other, "timers")),
        }
    }
    async fn tasks(&self, request: TaskRequest) -> Result<TaskReply, ApiError> {
        match self
            .call(&ClientRequest::Tasks { request }, REQUEST_TIMEOUT)
            .await?
        {
            ClientResponse::Tasks { reply } => Ok(reply),
            other => Err(unexpected(other, "tasks")),
        }
    }

    async fn operations(&self, request: OperationsRequest) -> Result<OperationsReply, ApiError> {
        match self
            .call(&ClientRequest::Operations { request }, REQUEST_TIMEOUT)
            .await?
        {
            ClientResponse::Operations { reply } => Ok(reply),
            other => Err(unexpected(other, "operations")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_join_code_round_trips_through_its_text() {
        let hub = SecretKey::generate().public();
        let code = JoinCode {
            hub: EndpointAddr::from_parts(
                hub,
                [
                    TransportAddr::Ip("127.0.0.1:4433".parse().unwrap()),
                    TransportAddr::Ip("[fe80::1]:4433".parse().unwrap()),
                ],
            ),
            secret: [7; 32],
        };
        let text = code.to_string();
        assert!(text.starts_with(&format!("tl1-{hub}-")), "{text}");
        assert_eq!(text.parse::<JoinCode>(), Ok(code.clone()));
        assert!(
            !format!("{code:?}").contains(&hex(&code.secret)),
            "Debug hides the secret"
        );

        for bad in [
            "",
            "tl2-x-y-z",
            &format!("tl1-{hub}-abc-127.0.0.1:1"),
            &format!("tl1-{hub}-{}-not an address", hex(&[1; 32])),
            "tl1-nothex-0000-127.0.0.1:1",
        ] {
            assert!(bad.parse::<JoinCode>().is_err(), "{bad:?}");
        }
    }
}
