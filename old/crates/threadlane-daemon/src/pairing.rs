//! Durable LAN device pairing for the embedded [`DaemonCore`].

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::fs::File;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::core::DaemonCore;

const CONFIG_VERSION: u32 = 1;
const CONFIG_FILENAME: &str = "pairing.json";
pub(crate) const DEVICE_NAME_HEADER: &str = "x-threadlane-device-name";
pub(crate) const DEVICE_ID_HEADER: &str = "x-threadlane-device-id";
static REGISTRY_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// URL scheme the mobile app registers; iOS Camera offers to open a QR
/// carrying this scheme in the pairing app directly.
pub const PAIRING_SCHEME: &str = "threadlane";

/// Connection details a thin client needs to attach, encoded into the
/// [`uri`](PairingInfo::uri) deep link behind the QR code.
#[derive(Clone)]
pub struct PairingInfo {
    /// LAN IPv4 the listener is reachable on.
    pub host: String,
    pub port: u16,
    /// Credential for this one device invitation.
    pub token: String,
    /// Stable ID allocated for the device when its invitation is accepted.
    pub device_id: String,
    /// Suggested device label. The authenticated client may provide its own.
    pub name: String,
}

impl fmt::Debug for PairingInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingInfo")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("token", &"[redacted]")
            .field("device_id", &self.device_id)
            .field("name", &self.name)
            .finish()
    }
}

impl PairingInfo {
    /// The WebSocket endpoint a native thin client dials.
    pub fn ws_url(&self) -> String {
        format!("ws://{}:{}", self.host, self.port)
    }

    /// The pairing deep link. Older links containing only host, port, and
    /// token remain valid.
    pub fn uri(&self) -> String {
        format!(
            "{}://pair?host={}&port={}&token={}&device_id={}&name={}",
            PAIRING_SCHEME,
            self.host,
            self.port,
            self.token,
            percent_encode(&self.device_id),
            percent_encode(&self.name)
        )
    }
}

/// Public device information. Device credentials are deliberately private.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingConfig {
    version: u32,
    enabled: bool,
    port: u16,
    devices: Vec<StoredDevice>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDevice {
    id: String,
    name: String,
    token: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingInvitation {
    id: String,
    token: String,
}

struct PairingState {
    config: PairingConfig,
    pending: Option<PendingInvitation>,
    healthy: bool,
    revoked: HashMap<String, watch::Sender<bool>>,
}

/// Shared by the admin handle and the pairing-specific WebSocket handshake.
/// The lock also serializes registration and revocation against auth checks.
pub(crate) struct PairingAuth {
    path: PathBuf,
    state: Mutex<PairingState>,
}

impl PairingAuth {
    fn new(
        path: PathBuf,
        config: PairingConfig,
        pending: Option<PendingInvitation>,
    ) -> Self {
        Self {
            path,
            state: Mutex::new(PairingState {
                config,
                pending,
                healthy: true,
                revoked: HashMap::new(),
            }),
        }
    }

    pub(crate) fn authorize(
        &self,
        token: &str,
        requested_name: Option<&str>,
    ) -> Result<(String, watch::Receiver<bool>), AuthorizationFailure> {
        let mut state = self.state.lock().expect("pairing state poisoned");
        if !state.healthy || !state.config.enabled {
            return Err(AuthorizationFailure::Unavailable);
        }
        let existing = state
            .config
            .devices
            .iter()
            .find(|device| device.token == token)
            .map(|device| device.id.clone());
        let id = if let Some(id) = existing {
            id
        } else {
            let Some(invitation) = state
                .pending
                .as_ref()
                .filter(|invitation| invitation.token == token)
                .cloned()
            else {
                return Err(AuthorizationFailure::Unauthorized);
            };
            let name = unique_device_name(&state.config.devices, requested_name);
            let mut candidate = state.config.clone();
            candidate.devices.push(StoredDevice {
                id: invitation.id.clone(),
                name,
                token: invitation.token,
            });
            if write_config(&self.path, &candidate).is_err() {
                state.healthy = false;
                state.pending = None;
                revoke_all(&mut state);
                return Err(AuthorizationFailure::Unavailable);
            }
            state.config = candidate;
            state.pending = None;
            invitation.id
        };
        let receiver = state
            .revoked
            .entry(id.clone())
            .or_insert_with(|| watch::channel(false).0)
            .subscribe();
        Ok((id, receiver))
    }

    fn pending_invitation(&self, host: &str) -> Option<PairingInfo> {
        let state = self.state.lock().expect("pairing state poisoned");
        state.pending.as_ref().map(|pending| PairingInfo {
            host: host.to_string(),
            port: state.config.port,
            token: pending.token.clone(),
            device_id: pending.id.clone(),
            name: "New device".to_string(),
        })
    }

    fn devices(&self) -> Vec<PairedDevice> {
        self.state
            .lock()
            .expect("pairing state poisoned")
            .config
            .devices
            .iter()
            .map(|device| PairedDevice {
                id: device.id.clone(),
                name: device.name.clone(),
            })
            .collect()
    }

    fn begin_pairing(&self, host: &str) -> Result<PairingInfo, String> {
        let mut state = self.state.lock().expect("pairing state poisoned");
        ensure_healthy(&state)?;
        if !state.config.enabled {
            return Err("device sharing is not enabled".to_string());
        }
        let mut token = pairing_token();
        while state
            .config
            .devices
            .iter()
            .any(|device| device.token == token)
            || state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.token == token)
        {
            token = pairing_token();
        }
        let invitation = PendingInvitation {
            id: pairing_id(),
            token,
        };
        state.pending = Some(invitation.clone());
        Ok(PairingInfo {
            host: host.to_string(),
            port: state.config.port,
            token: invitation.token,
            device_id: invitation.id,
            name: "New device".to_string(),
        })
    }

    fn remove_device(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().expect("pairing state poisoned");
        ensure_healthy(&state)?;
        let mut candidate = state.config.clone();
        let old_len = candidate.devices.len();
        candidate.devices.retain(|device| device.id != id);
        if candidate.devices.len() == old_len {
            return Err("paired device was not found".to_string());
        }
        commit_config(&self.path, &mut state, candidate)?;
        if state.pending.as_ref().is_some_and(|pending| pending.id == id) {
            state.pending = None;
        }
        if let Some(revoked) = state.revoked.remove(id) {
            revoked.send_replace(true);
        }
        Ok(())
    }

    fn remove_all(&self) -> Result<(), String> {
        let mut state = self.state.lock().expect("pairing state poisoned");
        ensure_healthy(&state)?;
        let mut candidate = state.config.clone();
        candidate.enabled = false;
        candidate.devices.clear();
        commit_config(&self.path, &mut state, candidate)?;
        state.pending = None;
        revoke_all(&mut state);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthorizationFailure {
    Unauthorized,
    Unavailable,
}

/// Handle for a live pairing listener. Dropping or stopping the handle
/// stops the listener but preserves saved devices and the enabled setting.
pub struct PairingServer {
    info: PairingInfo,
    host: String,
    auth: Arc<PairingAuth>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl PairingServer {
    /// Start persistent LAN sharing, reusing its saved port. The listener
    /// remains bound until stopped, including when it currently has no
    /// paired devices.
    pub async fn start(core: Arc<DaemonCore>) -> Result<Self, String> {
        Self::start_with_config_path(core, default_config_path(), None).await
    }

    /// Test/support variant that isolates the durable registry at `path`.
    pub async fn start_with_config_path(
        core: Arc<DaemonCore>,
        path: impl Into<PathBuf>,
        host_override: Option<String>,
    ) -> Result<Self, String> {
        let _registry_guard = registry_lock().lock().await;
        let path = path.into();
        let existing = read_config(&path)?;
        let requested_port = existing
            .as_ref()
            .filter(|config| config.enabled)
            .map_or(0, |config| config.port);
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, requested_port))
            .await
            .map_err(|error| format!("could not bind pairing listener: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("could not read pairing listener address: {error}"))?
            .port();
        let mut config = existing.unwrap_or_else(|| PairingConfig {
            version: CONFIG_VERSION,
            enabled: false,
            port,
            devices: Vec::new(),
        });
        let changed = !config.enabled || config.port != port;
        config.enabled = true;
        config.port = port;
        let pending = if config.devices.is_empty() {
            Some(PendingInvitation {
                id: pairing_id(),
                token: pairing_token(),
            })
        } else {
            None
        };
        if changed {
            write_config(&path, &config)?;
        }
        let host = host_override
            .unwrap_or_else(|| primary_ipv4().map_or_else(|| "127.0.0.1".to_string(), |ip| ip.to_string()));
        let auth = Arc::new(PairingAuth::new(path, config, pending));
        Self::serve(listener, core, auth, host, port)
    }

    /// Restore only an enabled, previously saved listener. This never
    /// creates an enrollment invitation.
    pub async fn restore(core: Arc<DaemonCore>) -> Result<Option<Self>, String> {
        Self::restore_with_config_path(core, default_config_path(), None).await
    }

    /// Test/support variant that isolates the durable registry at `path`.
    pub async fn restore_with_config_path(
        core: Arc<DaemonCore>,
        path: impl Into<PathBuf>,
        host_override: Option<String>,
    ) -> Result<Option<Self>, String> {
        let _registry_guard = registry_lock().lock().await;
        let path = path.into();
        let Some(config) = read_config(&path)? else {
            return Ok(None);
        };
        if !config.enabled {
            return Ok(None);
        }
        let port = config.port;
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
            .await
            .map_err(|error| format!("could not bind saved pairing port {port}: {error}"))?;
        let host = host_override
            .unwrap_or_else(|| primary_ipv4().map_or_else(|| "127.0.0.1".to_string(), |ip| ip.to_string()));
        let auth = Arc::new(PairingAuth::new(path, config, None));
        Self::serve(listener, core, auth, host, port).map(Some)
    }

    /// Disable persisted sharing even when the listener could not be
    /// restored (for example, because its saved port is occupied).
    pub async fn remove_all_saved() -> Result<(), String> {
        Self::remove_all_saved_with_config_path(default_config_path()).await
    }

    /// Test/support variant that disables persisted sharing at `path`
    /// without requiring a live listener.
    pub async fn remove_all_saved_with_config_path(
        path: impl Into<PathBuf>,
    ) -> Result<(), String> {
        let _registry_guard = registry_lock().lock().await;
        let path = path.into();
        let Some(mut config) = read_config(&path)? else {
            return Ok(());
        };
        config.enabled = false;
        config.devices.clear();
        write_config(&path, &config)
    }

    fn serve(
        listener: TcpListener,
        core: Arc<DaemonCore>,
        auth: Arc<PairingAuth>,
        host: String,
        port: u16,
    ) -> Result<Self, String> {
        let info = auth.pending_invitation(&host).unwrap_or_else(|| PairingInfo {
            host: host.clone(),
            port,
            token: String::new(),
            device_id: String::new(),
            name: String::new(),
        });
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(crate::server::serve_pairing_until(
            listener,
            core,
            auth.clone(),
            async move {
                let _ = shutdown_rx.await;
            },
        ));
        tracing::info!(%host, %port, "pairing listener started");
        Ok(Self {
            info,
            host,
            auth,
            shutdown: Some(shutdown),
            task,
        })
    }

    /// Legacy first-invitation view. New UI should use
    /// [`pending_invitation`](Self::pending_invitation).
    pub fn info(&self) -> &PairingInfo {
        &self.info
    }

    pub fn pending_invitation(&self) -> Option<PairingInfo> {
        self.auth.pending_invitation(&self.host)
    }

    pub fn begin_pairing(&mut self) -> Result<PairingInfo, String> {
        let invitation = self.auth.begin_pairing(&self.host)?;
        self.info = invitation.clone();
        Ok(invitation)
    }

    pub fn devices(&self) -> Vec<PairedDevice> {
        self.auth.devices()
    }

    pub fn remove_device(&mut self, id: &str) -> Result<(), String> {
        self.auth.remove_device(id)
    }

    /// Stop sharing and revoke all remembered devices. Unlike dropping the
    /// handle or application shutdown, this explicitly disables persistence.
    pub async fn remove_all(self) -> Result<(), String> {
        let _registry_guard = registry_lock().lock().await;
        self.auth.remove_all()?;
        self.stop().await;
        Ok(())
    }

    /// Stop accepting connections while preserving persistent trust.
    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = (&mut self.task).await;
    }
}

fn registry_lock() -> &'static tokio::sync::Mutex<()> {
    REGISTRY_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

impl Drop for PairingServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn default_config_path() -> PathBuf {
    threadlane_project::global_threadlane_dir().join(CONFIG_FILENAME)
}

fn read_config(path: &Path) -> Result<Option<PairingConfig>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not inspect pairing registry {}: {error}",
                path.display()
            ))
        }
    };
    if !metadata.file_type().is_file() {
        return Err(format!(
            "pairing registry is not a regular file: {}",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "pairing registry permissions are not private: {}",
                path.display()
            ));
        }
    }
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "could not read pairing registry {}: {error}",
            path.display()
        )
    })?;
    let config: PairingConfig = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "could not parse pairing registry {}: {error}",
            path.display()
        )
    })?;
    validate_config(&config, path)?;
    Ok(Some(config))
}

fn validate_config(config: &PairingConfig, path: &Path) -> Result<(), String> {
    let malformed = config.version != CONFIG_VERSION
        || config.port == 0
        || config.devices.iter().any(|device| {
            device.id.is_empty() || device.name.is_empty() || device.token.is_empty()
        });
    if malformed {
        return Err(format!(
            "pairing registry has invalid or unsupported data: {}",
            path.display()
        ));
    }
    for (index, device) in config.devices.iter().enumerate() {
        if config.devices[index + 1..].iter().any(|other| {
            device.id == other.id || device.token == other.token
        }) {
            return Err(format!(
                "pairing registry contains duplicate credentials: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn write_config(path: &Path, config: &PairingConfig) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| {
        format!("pairing registry has no parent directory: {}", path.display())
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "could not create pairing registry directory {}: {error}",
            parent.display()
        )
    })?;
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".{}.tmp", pairing_id()));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|error| {
            format!(
                "could not create pairing registry temporary file: {error}"
            )
        })?;
        let encoded = serde_json::to_vec_pretty(config)
            .map_err(|error| format!("could not encode pairing registry: {error}"))?;
        file.write_all(&encoded)
            .map_err(|error| format!("could not write pairing registry: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("could not sync pairing registry: {error}"))?;
        drop(file);
        fs::rename(&temporary, path)
            .map_err(|error| format!("could not replace pairing registry: {error}"))?;
        #[cfg(unix)]
        {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| {
                    format!("could not sync pairing registry directory: {error}")
                })?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn commit_config(
    path: &Path,
    state: &mut PairingState,
    candidate: PairingConfig,
) -> Result<(), String> {
    if let Err(error) = write_config(path, &candidate) {
        state.healthy = false;
        revoke_all(state);
        return Err(error);
    }
    state.config = candidate;
    Ok(())
}

fn ensure_healthy(state: &PairingState) -> Result<(), String> {
    if state.healthy {
        Ok(())
    } else {
        Err("pairing registry is unavailable; restart sharing after fixing the registry".into())
    }
}

fn revoke_all(state: &mut PairingState) {
    for (_, revoked) in state.revoked.drain() {
        revoked.send_replace(true);
    }
}

fn unique_device_name(devices: &[StoredDevice], requested: Option<&str>) -> String {
    let name = requested
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.chars().filter(|character| !character.is_control()).take(64).collect::<String>())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Mobile device".to_string());
    if !devices
        .iter()
        .any(|device| device.name.eq_ignore_ascii_case(&name))
    {
        return name;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{name} {suffix}");
        if !devices
            .iter()
            .any(|device| device.name.eq_ignore_ascii_case(&candidate))
        {
            return candidate;
        }
        suffix += 1;
    }
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// A URL-safe bearer token with enough entropy that a network attacker
/// cannot guess it.
fn pairing_token() -> String {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("secure randomness should be available for pairing tokens");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn pairing_id() -> String {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("secure randomness should be available for pairing IDs");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The IPv4 outbound traffic would leave on. A UDP `connect` sends nothing.
fn primary_ipv4() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_link_local() => Some(ip),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_protocol::daemon::{CommandRequest, SessionEvent};
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    use tokio_tungstenite::tungstenite::Message;

    fn request(port: u16, token: &str, device_name: &str) -> tokio_tungstenite::tungstenite::http::Request<()> {
        let mut request = format!("ws://127.0.0.1:{port}")
            .into_client_request()
            .expect("valid WebSocket URL");
        request.headers_mut().insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).expect("valid bearer token"),
        );
        request.headers_mut().insert(
            DEVICE_NAME_HEADER,
            HeaderValue::from_str(device_name).expect("valid device name"),
        );
        request
    }

    async fn connect(
        port: u16,
        token: &str,
        device_name: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Error,
    > {
        tokio_tungstenite::connect_async(request(port, token, device_name))
            .await
            .map(|(socket, _)| socket)
    }

    #[tokio::test]
    async fn restart_reuses_port_and_credentials_without_open_enrollment() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CONFIG_FILENAME);
        let core = DaemonCore::new().unwrap();
        let server = PairingServer::start_with_config_path(
            core.clone(),
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap();
        let invitation = server.pending_invitation().unwrap();
        let original_port = invitation.port;
        let first_connection =
            connect(original_port, &invitation.token, "Test phone").await.unwrap();
        assert_eq!(server.devices()[0].id, invitation.device_id);
        assert_eq!(server.devices()[0].name, "Test phone");
        let reconnect =
            connect(original_port, &invitation.token, "Renamed phone").await.unwrap();
        assert_eq!(server.devices().len(), 1);
        drop(first_connection);
        drop(reconnect);
        server.stop().await;

        let restored = PairingServer::restore_with_config_path(
            core,
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(restored.info().port, original_port);
        assert!(restored.pending_invitation().is_none());
        assert_eq!(restored.devices()[0].id, invitation.device_id);
        let reconnect = connect(original_port, &invitation.token, "Test phone")
            .await
            .expect("saved credential should authenticate after restore");
        drop(reconnect);
        assert!(connect(original_port, "not-a-valid-device-token", "Attacker")
            .await
            .is_err());
        restored.stop().await;
    }

    #[tokio::test]
    async fn large_replay_does_not_block_ping_or_revocation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CONFIG_FILENAME);
        let core = DaemonCore::new().unwrap();
        let mut server = PairingServer::start_with_config_path(
            core.clone(),
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap();
        let invitation = server.pending_invitation().unwrap();
        let sender = core.event_sender();
        for index in 0..300 {
            sender
                .send(SessionEvent::DaemonError {
                    session_id: None,
                    message: format!("replay {index}"),
                })
                .unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if core.journal_tail().len() >= 300 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("journal did not receive test events");
        let tail = core.journal_tail();
        assert!(tail.len() > 256);

        let mut socket = connect(invitation.port, &invitation.token, "Test phone")
            .await
            .unwrap();
        for (sequence, _) in &tail {
            let text = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                socket.next(),
            )
            .await
            .expect("replay stalled")
            .expect("socket closed during replay")
            .unwrap()
            .into_text()
            .unwrap();
            let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(frame["seq"].as_u64(), Some(*sequence));
        }
        socket.send(Message::Ping(Vec::new().into())).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match socket.next().await {
                    Some(Ok(Message::Pong(_))) => break,
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => panic!("paired socket failed: {error}"),
                    None => panic!("paired socket closed before Pong"),
                }
            }
        })
        .await
        .expect("ping/pong should continue after large replay");
        server.remove_device(&invitation.device_id).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match socket.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await
        .expect("revocation should close the replaying client");
        server.stop().await;
    }

    #[tokio::test]
    async fn revoking_nonreading_backpressured_client_releases_saved_port() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CONFIG_FILENAME);
        let core = DaemonCore::new().unwrap();
        let server = PairingServer::start_with_config_path(
            core.clone(),
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap();
        let invitation = server.pending_invitation().unwrap();
        let port = invitation.port;
        let mut socket = connect(port, &invitation.token, "Test phone").await.unwrap();

        let request = CommandRequest {
            request_id: 1,
            command: threadlane_protocol::daemon::SessionCommand::GetProjects,
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&request).unwrap().into(),
            ))
            .await
            .unwrap();

        // Keep the authenticated peer open without reading while enough
        // frames to exceed both the bounded queue and normal TCP buffers are
        // broadcast. The pending request makes the request worker exercise
        // the same reply path that shutdown must unblock.
        let payload = "x".repeat(64 * 1024);
        let sender = core.event_sender();
        for index in 0..300 {
            sender
                .send(SessionEvent::DaemonError {
                    session_id: None,
                    message: format!("{index}:{payload}"),
                })
                .unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if core.journal_tail().len() >= 300 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("backpressure events did not reach the journal");

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            server
                .remove_all()
                .await
                .expect("offline revoke should persist");
        })
        .await
        .expect("revoking a non-reading client did not finish");
        drop(socket);

        let rebound = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
            .await
            .expect("revoked listener should release its saved port");
        drop(rebound);
        let restarted = PairingServer::start_with_config_path(
            DaemonCore::new().unwrap(),
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .expect("fresh start should recover after offline revoke");
        restarted.stop().await;
    }

    #[tokio::test]
    async fn pending_invitation_is_not_persisted_or_restored() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CONFIG_FILENAME);
        let core = DaemonCore::new().unwrap();
        let server = PairingServer::start_with_config_path(
            core.clone(),
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap();
        let invitation = server.pending_invitation().unwrap();
        let persisted = fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains(&invitation.token));
        assert!(!persisted.contains("pending"));
        server.stop().await;

        let restored = PairingServer::restore_with_config_path(
            core,
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(restored.pending_invitation().is_none());
        restored.stop().await;
    }

    #[tokio::test]
    async fn occupied_saved_port_can_be_recovered_by_offline_remove_all() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CONFIG_FILENAME);
        let server = PairingServer::start_with_config_path(
            DaemonCore::new().unwrap(),
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap();
        let invitation = server.pending_invitation().unwrap();
        let port = invitation.port;
        drop(connect(port, &invitation.token, "Test phone").await.unwrap());
        server.stop().await;

        let occupied = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
            .await
            .unwrap();
        assert!(PairingServer::restore_with_config_path(
            DaemonCore::new().unwrap(),
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .is_err());
        let config = read_config(&path).unwrap().unwrap();
        assert!(config.enabled);
        assert_eq!(config.port, port);
        assert_eq!(config.devices.len(), 1);

        PairingServer::remove_all_saved_with_config_path(&path)
            .await
            .unwrap();
        let revoked = read_config(&path).unwrap().unwrap();
        assert!(!revoked.enabled);
        assert!(revoked.devices.is_empty());
        assert_eq!(revoked.port, port);

        let restarted = PairingServer::start_with_config_path(
            DaemonCore::new().unwrap(),
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .expect("disabled registry should bind a fresh available port");
        assert_ne!(restarted.info().port, port);
        restarted.stop().await;
        drop(occupied);
    }

    #[tokio::test]
    async fn remove_all_disables_sharing_and_restore() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CONFIG_FILENAME);
        let core = DaemonCore::new().unwrap();
        let server = PairingServer::start_with_config_path(
            core.clone(),
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap();
        let invitation = server.pending_invitation().unwrap();
        drop(connect(invitation.port, &invitation.token, "Test phone").await.unwrap());
        server.remove_all().await.unwrap();

        assert!(PairingServer::restore_with_config_path(
            core,
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap()
        .is_none());
        let config = read_config(&path).unwrap().unwrap();
        assert!(!config.enabled);
        assert!(config.devices.is_empty());
    }

    #[tokio::test]
    async fn revoking_one_device_disconnects_it_without_affecting_another() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CONFIG_FILENAME);
        let core = DaemonCore::new().unwrap();
        let mut server = PairingServer::start_with_config_path(
            core,
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap();
        let first_invitation = server.pending_invitation().unwrap();
        let mut first = connect(
            first_invitation.port,
            &first_invitation.token,
            "First phone",
        )
        .await
        .unwrap();
        let first_id = first_invitation.device_id;

        let second_invitation = server.begin_pairing().unwrap();
        let mut second = connect(
            second_invitation.port,
            &second_invitation.token,
            "Second phone",
        )
        .await
        .unwrap();
        server.remove_device(&first_id).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match first.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await
        .expect("revoked connection should close promptly");
        assert!(connect(first_invitation.port, &first_invitation.token, "First phone")
            .await
            .is_err());
        second.send(Message::Ping(Vec::new().into())).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match second.next().await {
                    Some(Ok(Message::Pong(_))) => break,
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => panic!("surviving device socket failed: {error}"),
                    None => panic!("surviving device socket closed"),
                }
            }
        })
        .await
        .expect("surviving device should continue receiving heartbeats");
        assert_eq!(
            server.devices(),
            vec![PairedDevice {
                id: second_invitation.device_id,
                name: "Second phone".to_string(),
            }]
        );
        let _ = second.send(Message::Close(None)).await;
        server.stop().await;
    }

    #[tokio::test]
    async fn corrupt_or_unwritable_registry_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let corrupt = directory.path().join("corrupt.json");
        fs::write(&corrupt, b"{ malformed").unwrap();
        assert!(PairingServer::start_with_config_path(
            DaemonCore::new().unwrap(),
            &corrupt,
            Some("127.0.0.1".to_string()),
        )
        .await
        .is_err());

        let blocking_parent = directory.path().join("not-a-directory");
        fs::write(&blocking_parent, b"x").unwrap();
        assert!(PairingServer::start_with_config_path(
            DaemonCore::new().unwrap(),
            blocking_parent.join(CONFIG_FILENAME),
            Some("127.0.0.1".to_string()),
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn failed_revocation_is_not_reported_as_success() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CONFIG_FILENAME);
        let core = DaemonCore::new().unwrap();
        let mut server = PairingServer::start_with_config_path(
            core,
            &path,
            Some("127.0.0.1".to_string()),
        )
        .await
        .unwrap();
        let invitation = server.pending_invitation().unwrap();
        let mut socket = connect(invitation.port, &invitation.token, "Phone")
            .await
            .unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(server.remove_device(&invitation.device_id).is_err());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match socket.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await
        .expect("failed registry must fail closed and disconnect clients");
        drop(server);
    }
}
