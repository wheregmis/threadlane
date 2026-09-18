//! Session permission manager: user approval flows for sensitive actions.
//!
//! `PermissionManager`/`PermissionHandle`/`PermissionDecision` govern network,
//! computer-use, and external-agent prompts with Once/Session/Always scopes,
//! per-project persisted grants, and default-deny unattended posture. They
//! depend only on `threadlane-protocol` contracts, the
//! `threadlane-computer` approval trait, and local persistence — never on
//! the session or engine. Trace vocabulary is shared with the harness via
//! `threadlane-protocol::interaction`. Import `threadlane_permission` directly.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use threadlane_protocol::interaction::{
    PermissionTraceDecision, PermissionTraceScope, PermissionTraceSource,
};
use threadlane_protocol::{AgentEvent, PermissionRequest, PermissionScope};
use tokio::sync::oneshot;

const PERMISSIONS_FILE: &str = ".threadlane/permissions.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    AllowOnce,
    /// In-memory grant for the rest of the session (never persisted).
    AllowSession,
    AllowAlways,
    Deny,
}

#[derive(Clone)]
pub struct PermissionHandle {
    inner: Arc<PermissionManagerInner>,
}

#[derive(Clone, Debug)]
pub enum PermissionTraceEvent {
    Requested {
        request_id: String,
        capability: String,
        scopes: Vec<PermissionTraceScope>,
        detail_sha256: String,
        source: PermissionTraceSource,
    },
    Resolved {
        request_id: String,
        decision: PermissionTraceDecision,
        scope: Option<PermissionTraceScope>,
        source: PermissionTraceSource,
        remembered: bool,
    },
}

pub type PermissionTraceRecorder = Arc<
    dyn Fn(PermissionTraceEvent) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub struct PermissionManager {
    handle: PermissionHandle,
    event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
}

struct PermissionManagerInner {
    session_prefix: String,
    interactive: AtomicBool,
    next_id: AtomicU64,
    pending: Mutex<HashMap<String, oneshot::Sender<PermissionDecision>>>,
    project_root: PathBuf,
    persistent: Mutex<PersistentPermissions>,
    trace_recorder: Mutex<Option<PermissionTraceRecorder>>,
    /// Session-scoped computer-use grant: set by one `AllowSession` decision,
    /// never persisted, dies with this manager (one per session runtime).
    computer_session_allowed: AtomicBool,
}

/// Persistent permissions remembered across agent runs and restarts.
///
/// Persistent grants are currently capability-scoped to network host connections.
/// Filesystem, execution, and environment capabilities are scoped to the active session.
#[derive(Default, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct PersistentPermissions {
    #[serde(default)]
    network_hosts: HashSet<String>,
    /// Project-scoped computer-use grant: screenshots and input no longer
    /// re-prompt once the user allows always. Unlike network hosts this is a
    /// single flag, not a per-target list, so it stays obvious in the file.
    #[serde(default)]
    computer_allowed: bool,
}

impl PermissionManager {
    pub fn new(
        project_root: PathBuf,
        event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    ) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let session_prefix = format!("{}-{:x}", std::process::id(), nanos);
        Self::with_session_prefix(project_root, session_prefix, event_tx)
    }

    fn with_session_prefix(
        project_root: PathBuf,
        session_prefix: impl Into<String>,
        event_tx: tokio::sync::broadcast::Sender<AgentEvent>,
    ) -> Self {
        let persistent = load_permissions(&project_root);
        Self {
            handle: PermissionHandle {
                inner: Arc::new(PermissionManagerInner {
                    session_prefix: session_prefix.into(),
                    interactive: AtomicBool::new(false),
                    next_id: AtomicU64::new(1),
                    pending: Mutex::new(HashMap::new()),
                    project_root,
                    persistent: Mutex::new(persistent),
                    trace_recorder: Mutex::new(None),
                    computer_session_allowed: AtomicBool::new(false),
                }),
            },
            event_tx,
        }
    }

    pub fn handle(&self) -> PermissionHandle {
        self.handle.clone()
    }

    fn generate_request_id(&self) -> String {
        self.handle.generate_request_id()
    }

    pub fn network_host_is_approved(&self, host: &str) -> bool {
        self.handle
            .inner
            .persistent
            .lock()
            .is_ok_and(|permissions| permissions.network_hosts.contains(host))
    }

    async fn record_trace(&self, event: PermissionTraceEvent) -> Result<(), String> {
        self.handle.record_trace(event).await
    }

    pub async fn trace_preapproved_network_host(
        &self,
        url: &str,
        persisted: bool,
    ) -> Result<(), String> {
        let id = self.generate_request_id();
        let source = if persisted {
            PermissionTraceSource::PersistedGrant
        } else {
            PermissionTraceSource::Policy
        };
        let scope = if persisted {
            PermissionTraceScope::Project
        } else {
            PermissionTraceScope::Session
        };
        self.record_trace(PermissionTraceEvent::Requested {
            request_id: id.clone(),
            capability: "network".into(),
            scopes: vec![scope.clone()],
            detail_sha256: format!("{:x}", Sha256::digest(url.as_bytes())),
            source: source.clone(),
        })
        .await?;
        self.record_trace(PermissionTraceEvent::Resolved {
            request_id: id,
            decision: PermissionTraceDecision::Allowed,
            scope: Some(scope),
            source,
            remembered: persisted,
        })
        .await
    }

    pub async fn request_network_host(&self, host: &str, url: &str) -> PermissionDecision {
        let id = self.generate_request_id();
        let interactive = self.handle.inner.interactive.load(Ordering::SeqCst);
        let requested = PermissionTraceEvent::Requested {
            request_id: id.clone(),
            capability: "network".into(),
            scopes: vec![PermissionTraceScope::Once, PermissionTraceScope::Project],
            detail_sha256: format!("{:x}", Sha256::digest(url.as_bytes())),
            source: if interactive {
                PermissionTraceSource::User
            } else {
                PermissionTraceSource::UnattendedDefault
            },
        };
        if self.record_trace(requested).await.is_err() {
            return PermissionDecision::Deny;
        }
        if !interactive {
            let _ = self
                .record_trace(PermissionTraceEvent::Resolved {
                    request_id: id,
                    decision: PermissionTraceDecision::Denied,
                    scope: None,
                    source: PermissionTraceSource::UnattendedDefault,
                    remembered: false,
                })
                .await;
            return PermissionDecision::Deny;
        }
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.handle.inner.pending.lock() {
            pending.insert(id.clone(), tx);
        } else {
            return PermissionDecision::Deny;
        }
        let request = PermissionRequest {
            id: id.clone(),
            capability: "network".into(),
            title: format!("Connect to {host}"),
            detail: url.to_owned(),
            scopes: vec![PermissionScope::Once, PermissionScope::Always],
        };
        if self
            .event_tx
            .send(AgentEvent::PermissionRequested { request })
            .is_err()
        {
            self.handle.remove_pending(&id);
            return PermissionDecision::Deny;
        }
        let guard = PendingRequestGuard {
            handle: self.handle.clone(),
            request_id: id.clone(),
        };
        let decision = rx.await.unwrap_or(PermissionDecision::Deny);
        drop(guard);
        let mut effective = decision;
        let mut remembered = false;
        if decision == PermissionDecision::AllowAlways {
            if self.persist_network_host(host).is_err() {
                effective = PermissionDecision::Deny;
            } else {
                remembered = true;
            }
        }
        let (trace_decision, scope) = match effective {
            PermissionDecision::AllowOnce => (
                PermissionTraceDecision::Allowed,
                Some(PermissionTraceScope::Once),
            ),
            // Network prompts never offer Session; a programmatic resolve
            // with it is a one-time allow that persists nothing.
            PermissionDecision::AllowSession => (
                PermissionTraceDecision::Allowed,
                Some(PermissionTraceScope::Once),
            ),
            PermissionDecision::AllowAlways => (
                PermissionTraceDecision::Allowed,
                Some(PermissionTraceScope::Project),
            ),
            PermissionDecision::Deny => (PermissionTraceDecision::Denied, None),
        };
        if self
            .record_trace(PermissionTraceEvent::Resolved {
                request_id: id,
                decision: trace_decision,
                scope,
                source: PermissionTraceSource::User,
                remembered,
            })
            .await
            .is_err()
        {
            return PermissionDecision::Deny;
        }
        effective
    }

    fn persist_network_host(&self, host: &str) -> Result<(), String> {
        let mut permissions = self
            .handle
            .inner
            .persistent
            .lock()
            .map_err(|_| "permission settings are unavailable".to_string())?;
        permissions.network_hosts.insert(host.to_owned());
        save_permissions(&self.handle.inner.project_root, &permissions)
    }

    /// Ask the user to approve one computer-use action (screenshot or input).
    /// A remembered project grant or a session grant for this run skips the
    /// prompt; otherwise every action re-prompts with Once/Session/Always
    /// scopes. Unattended sessions deny.
    pub(crate) async fn request_computer(&self, title: &str, detail: &str) -> PermissionDecision {
        let id = self.generate_request_id();
        let interactive = self.handle.inner.interactive.load(Ordering::SeqCst);
        let persisted = self.computer_is_approved();
        let session_grant = self.handle.inner.computer_session_allowed.load(Ordering::SeqCst);
        let source = if persisted {
            PermissionTraceSource::PersistedGrant
        } else if interactive {
            PermissionTraceSource::User
        } else {
            PermissionTraceSource::UnattendedDefault
        };
        let requested = PermissionTraceEvent::Requested {
            request_id: id.clone(),
            capability: "computer".into(),
            scopes: vec![
                PermissionTraceScope::Once,
                PermissionTraceScope::Session,
                PermissionTraceScope::Project,
            ],
            detail_sha256: format!("{:x}", Sha256::digest(detail.as_bytes())),
            source: source.clone(),
        };
        if self.record_trace(requested).await.is_err() {
            return PermissionDecision::Deny;
        }
        if persisted {
            let _ = self
                .record_trace(PermissionTraceEvent::Resolved {
                    request_id: id,
                    decision: PermissionTraceDecision::Allowed,
                    scope: Some(PermissionTraceScope::Project),
                    source,
                    remembered: true,
                })
                .await;
            return PermissionDecision::AllowOnce;
        }
        if session_grant {
            let _ = self
                .record_trace(PermissionTraceEvent::Resolved {
                    request_id: id,
                    decision: PermissionTraceDecision::Allowed,
                    scope: Some(PermissionTraceScope::Session),
                    source,
                    remembered: false,
                })
                .await;
            return PermissionDecision::AllowOnce;
        }
        if !interactive {
            let _ = self
                .record_trace(PermissionTraceEvent::Resolved {
                    request_id: id,
                    decision: PermissionTraceDecision::Denied,
                    scope: None,
                    source: PermissionTraceSource::UnattendedDefault,
                    remembered: false,
                })
                .await;
            return PermissionDecision::Deny;
        }
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.handle.inner.pending.lock() {
            pending.insert(id.clone(), tx);
        } else {
            return PermissionDecision::Deny;
        }
        let request = PermissionRequest {
            id: id.clone(),
            capability: "computer".into(),
            title: title.to_owned(),
            detail: detail.to_owned(),
            scopes: vec![
                PermissionScope::Once,
                PermissionScope::Session,
                PermissionScope::Always,
            ],
        };
        if self
            .event_tx
            .send(AgentEvent::PermissionRequested { request })
            .is_err()
        {
            self.handle.remove_pending(&id);
            return PermissionDecision::Deny;
        }
        let guard = PendingRequestGuard {
            handle: self.handle.clone(),
            request_id: id.clone(),
        };
        let decision = rx.await.unwrap_or(PermissionDecision::Deny);
        drop(guard);
        let mut effective = decision;
        let mut remembered = false;
        if decision == PermissionDecision::AllowAlways {
            if self.persist_computer_grant().is_err() {
                effective = PermissionDecision::Deny;
            } else {
                remembered = true;
            }
        }
        if effective == PermissionDecision::AllowSession {
            self.handle
                .inner
                .computer_session_allowed
                .store(true, Ordering::SeqCst);
        }
        let (trace_decision, scope) = match effective {
            PermissionDecision::AllowOnce => (
                PermissionTraceDecision::Allowed,
                Some(PermissionTraceScope::Once),
            ),
            PermissionDecision::AllowSession => (
                PermissionTraceDecision::Allowed,
                Some(PermissionTraceScope::Session),
            ),
            PermissionDecision::AllowAlways => (
                PermissionTraceDecision::Allowed,
                Some(PermissionTraceScope::Project),
            ),
            PermissionDecision::Deny => (PermissionTraceDecision::Denied, None),
        };
        let _ = self
            .record_trace(PermissionTraceEvent::Resolved {
                request_id: id,
                decision: trace_decision,
                scope,
                source: PermissionTraceSource::User,
                remembered,
            })
            .await;
        effective
    }

    pub(crate) fn computer_is_approved(&self) -> bool {
        self.handle
            .inner
            .persistent
            .lock()
            .is_ok_and(|permissions| permissions.computer_allowed)
    }

    fn persist_computer_grant(&self) -> Result<(), String> {
        let mut permissions = self
            .handle
            .inner
            .persistent
            .lock()
            .map_err(|_| "permission settings are unavailable".to_string())?;
        permissions.computer_allowed = true;
        save_permissions(&self.handle.inner.project_root, &permissions)
    }
}

/// Approval channel backing `threadlane_computer`: the executor prompts
/// through the session permission manager, so computer actions keep the
/// Once/Session/Always scopes, per-project persisted grant, and default-deny
/// unattended posture of every other capability.
#[async_trait::async_trait]
impl threadlane_computer::ComputerApproval for PermissionManager {
    async fn request_computer(
        &self,
        title: &str,
        detail: &str,
    ) -> threadlane_computer::ComputerDecision {
        match PermissionManager::request_computer(self, title, detail).await {
            PermissionDecision::AllowOnce | PermissionDecision::AllowSession => {
                threadlane_computer::ComputerDecision::AllowOnce
            }
            PermissionDecision::AllowAlways => threadlane_computer::ComputerDecision::AllowAlways,
            PermissionDecision::Deny => threadlane_computer::ComputerDecision::Deny,
        }
    }
}

struct PendingRequestGuard {
    handle: PermissionHandle,
    request_id: String,
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        self.handle.remove_pending(&self.request_id);
    }
}

impl PermissionHandle {
    pub fn set_trace_recorder(&self, recorder: Option<PermissionTraceRecorder>) {
        if let Ok(mut current) = self.inner.trace_recorder.lock() {
            *current = recorder;
        }
    }

    fn generate_request_id(&self) -> String {
        format!(
            "permission-{}-{}",
            self.inner.session_prefix,
            self.inner.next_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    async fn record_trace(&self, event: PermissionTraceEvent) -> Result<(), String> {
        let recorder = self
            .inner
            .trace_recorder
            .lock()
            .map_err(|_| "permission trace recorder is unavailable".to_string())?
            .clone();
        match recorder {
            Some(recorder) => recorder(event).await,
            None => Ok(()),
        }
    }

    fn is_interactive(&self) -> bool {
        self.inner.interactive.load(Ordering::SeqCst)
    }

    /// Asks the user to approve an action originating outside the tool
    /// dispatcher, such as an external ACP agent's `session/request_permission`.
    ///
    /// Unlike the capability requests above this grants nothing on its own and
    /// persists no grants: it renders a prompt, waits for the answer, and returns
    /// it. Without a UI attached there is no informed consent to give, so the
    /// answer is [`PermissionDecision::Deny`] rather than a silent allow.
    /// Cancellation returns `None` and records a cancelled permission decision.
    pub async fn request_external(
        &self,
        event_tx: &tokio::sync::broadcast::Sender<AgentEvent>,
        capability: &str,
        title: String,
        detail: String,
        allow_always: bool,
        cancelled: impl Future<Output = ()>,
    ) -> Option<PermissionDecision> {
        let id = self.generate_request_id();
        let interactive = self.is_interactive();
        let recorder = match self.inner.trace_recorder.lock() {
            Ok(recorder) => recorder.clone(),
            Err(_) => return Some(PermissionDecision::Deny),
        };
        let mut scopes = vec![PermissionScope::Once];
        if allow_always {
            scopes.push(PermissionScope::Always);
        }
        let trace_scopes = scopes
            .iter()
            .map(|scope| match scope {
                PermissionScope::Always => PermissionTraceScope::Project,
                PermissionScope::Session => PermissionTraceScope::Session,
                PermissionScope::Once => PermissionTraceScope::Once,
            })
            .collect();
        let requested = PermissionTraceEvent::Requested {
            request_id: id.clone(),
            capability: capability.to_string(),
            scopes: trace_scopes,
            detail_sha256: format!("{:x}", Sha256::digest(detail.as_bytes())),
            source: if interactive {
                PermissionTraceSource::User
            } else {
                PermissionTraceSource::UnattendedDefault
            },
        };
        if let Some(recorder) = &recorder {
            if recorder(requested).await.is_err() {
                return Some(PermissionDecision::Deny);
            }
        }
        let decision = tokio::select! {
            biased;
            _ = cancelled => None,
            decision = async {
                if !interactive {
                    return PermissionDecision::Deny;
                }
                let (tx, rx) = oneshot::channel();
                if let Ok(mut pending) = self.inner.pending.lock() {
                    pending.insert(id.clone(), tx);
                } else {
                    return PermissionDecision::Deny;
                }
                let guard = PendingRequestGuard {
                    handle: self.clone(),
                    request_id: id.clone(),
                };
                let request = PermissionRequest {
                    id: id.clone(),
                    capability: capability.to_string(),
                    title,
                    detail,
                    scopes,
                };
                if event_tx.send(AgentEvent::PermissionRequested { request }).is_err() {
                    return PermissionDecision::Deny;
                }
                let decision = rx.await.unwrap_or(PermissionDecision::Deny);
                drop(guard);
                decision
            } => Some(decision),
        };
        if let Some(recorder) = recorder {
            let _ = recorder(PermissionTraceEvent::Resolved {
                request_id: id,
                decision: match decision {
                    None => PermissionTraceDecision::Cancelled,
                    Some(PermissionDecision::Deny) => PermissionTraceDecision::Denied,
                    _ => PermissionTraceDecision::Allowed,
                },
                scope: match decision {
                    Some(PermissionDecision::AllowAlways) => Some(PermissionTraceScope::Project),
                    Some(PermissionDecision::AllowSession) => Some(PermissionTraceScope::Session),
                    Some(PermissionDecision::AllowOnce) => Some(PermissionTraceScope::Once),
                    _ => None,
                },
                source: if decision.is_none() {
                    PermissionTraceSource::System
                } else if interactive {
                    PermissionTraceSource::User
                } else {
                    PermissionTraceSource::UnattendedDefault
                },
                remembered: false,
            })
            .await;
        }
        decision
    }

    pub fn set_interactive(&self, interactive: bool) {
        self.inner.interactive.store(interactive, Ordering::SeqCst);
    }

    pub fn resolve(&self, request_id: &str, decision: PermissionDecision) -> bool {
        self.inner
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(request_id))
            .is_some_and(|sender| sender.send(decision).is_ok())
    }

    fn remove_pending(&self, request_id: &str) {
        if let Ok(mut pending) = self.inner.pending.lock() {
            pending.remove(request_id);
        }
    }
}

/// Test-only constructor for a standalone permission handle.
///
/// The manager that owns a handle is crate-private, so integration tests that
/// need to answer prompts (the ACP engine's, for one) have no other way to get
/// one.
#[cfg(feature = "test-support")]
impl PermissionHandle {
    pub fn for_tests(project_root: PathBuf) -> Self {
        let (event_tx, _rx) = tokio::sync::broadcast::channel(64);
        PermissionManager::new(project_root, event_tx).handle()
    }
}

fn load_permissions(project_root: &Path) -> PersistentPermissions {
    fs::read(project_root.join(PERMISSIONS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_permissions(
    project_root: &Path,
    permissions: &PersistentPermissions,
) -> Result<(), String> {
    let path = project_root.join(PERMISSIONS_FILE);
    let parent = path
        .parent()
        .ok_or_else(|| "permission settings path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;

    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() {
            return Err("refusing to follow symlink for permissions file".to_string());
        }
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_path = parent.join(format!(
        ".permissions.json.tmp.{}-{nanos}",
        std::process::id()
    ));

    let bytes = serde_json::to_vec_pretty(permissions).map_err(|error| error.to_string())?;
    fs::write(&temp_path, &bytes).map_err(|error| error.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600));
    }

    if let Err(error) = fs::rename(&temp_path, &path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn unattended_requests_default_to_deny() {
        let dir = tempdir().unwrap();
        let (event_tx, _) = tokio::sync::broadcast::channel(4);
        let manager = PermissionManager::new(dir.path().to_path_buf(), event_tx);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let trace_observed = observed.clone();
        manager
            .handle()
            .set_trace_recorder(Some(Arc::new(move |event| {
                let observed = trace_observed.clone();
                Box::pin(async move {
                    observed.lock().unwrap().push(event);
                    Ok(())
                })
            })));

        assert_eq!(
            manager
                .request_network_host("example.com", "https://example.com")
                .await,
            PermissionDecision::Deny
        );
        let observed = observed.lock().unwrap();
        assert!(matches!(
            observed.as_slice(),
            [
                PermissionTraceEvent::Requested {
                    source: PermissionTraceSource::UnattendedDefault,
                    ..
                },
                PermissionTraceEvent::Resolved {
                    decision: PermissionTraceDecision::Denied,
                    source: PermissionTraceSource::UnattendedDefault,
                    ..
                }
            ]
        ));
    }

    #[tokio::test]
    async fn allow_once_does_not_persist_host() {
        let dir = tempdir().unwrap();
        let (event_tx, mut events) = tokio::sync::broadcast::channel(4);
        let manager = Arc::new(PermissionManager::new(dir.path().to_path_buf(), event_tx));
        let handle = manager.handle();
        handle.set_interactive(true);
        let request_manager = manager.clone();
        let task = tokio::spawn(async move {
            request_manager
                .request_network_host("example.com", "https://example.com/page")
                .await
        });
        let AgentEvent::PermissionRequested { request } = events.recv().await.unwrap() else {
            panic!("expected permission request");
        };
        assert!(handle.resolve(&request.id, PermissionDecision::AllowOnce));
        assert_eq!(task.await.unwrap(), PermissionDecision::AllowOnce);
        assert!(!manager.network_host_is_approved("example.com"));
    }

    #[tokio::test]
    async fn always_allow_persists_exact_host() {
        let dir = tempdir().unwrap();
        let (event_tx, mut events) = tokio::sync::broadcast::channel(4);
        let manager = Arc::new(PermissionManager::new(dir.path().to_path_buf(), event_tx));
        let handle = manager.handle();
        handle.set_interactive(true);
        let request_manager = manager.clone();
        let task = tokio::spawn(async move {
            request_manager
                .request_network_host("example.com", "https://example.com/page")
                .await
        });
        let AgentEvent::PermissionRequested { request } = events.recv().await.unwrap() else {
            panic!("expected permission request");
        };
        assert!(handle.resolve(&request.id, PermissionDecision::AllowAlways));
        assert_eq!(task.await.unwrap(), PermissionDecision::AllowAlways);
        assert!(manager.network_host_is_approved("example.com"));
        assert!(!manager.network_host_is_approved("sub.example.com"));

        let (event_tx, _) = tokio::sync::broadcast::channel(1);
        let restored = PermissionManager::new(dir.path().to_path_buf(), event_tx);
        assert!(restored.network_host_is_approved("example.com"));
    }

    #[tokio::test]
    async fn computer_unattended_denies_without_prompt() {
        let dir = tempdir().unwrap();
        let (event_tx, mut events) = tokio::sync::broadcast::channel(4);
        let manager = PermissionManager::new(dir.path().to_path_buf(), event_tx);
        assert_eq!(
            manager.request_computer("Click at (1, 1)", "click").await,
            PermissionDecision::Deny
        );
        assert!(events.try_recv().is_err(), "unattended must not prompt");
        assert!(!manager.computer_is_approved());
    }

    #[tokio::test]
    async fn computer_allow_always_persists_project_grant() {
        let dir = tempdir().unwrap();
        let (event_tx, mut events) = tokio::sync::broadcast::channel(4);
        let manager = Arc::new(PermissionManager::new(dir.path().to_path_buf(), event_tx));
        let handle = manager.handle();
        handle.set_interactive(true);
        let request_manager = manager.clone();
        let task = tokio::spawn(async move {
            request_manager
                .request_computer("Click at (1, 1)", "click")
                .await
        });
        let AgentEvent::PermissionRequested { request } = events.recv().await.unwrap() else {
            panic!("expected permission request");
        };
        assert_eq!(request.capability, "computer");
        assert!(handle.resolve(&request.id, PermissionDecision::AllowAlways));
        assert_eq!(task.await.unwrap(), PermissionDecision::AllowAlways);
        assert!(manager.computer_is_approved());

        // A fresh manager on the same project skips the prompt entirely.
        let (event_tx, mut events) = tokio::sync::broadcast::channel(4);
        let restored = PermissionManager::new(dir.path().to_path_buf(), event_tx);
        assert!(restored.computer_is_approved());
        assert_eq!(
            restored.request_computer("Type", "type").await,
            PermissionDecision::AllowOnce
        );
        assert!(
            events.try_recv().is_err(),
            "remembered grant must not prompt"
        );
    }

    #[tokio::test]
    async fn computer_allow_session_covers_later_actions_without_persisting() {
        let dir = tempdir().unwrap();
        let (event_tx, mut events) = tokio::sync::broadcast::channel(8);
        let manager = Arc::new(PermissionManager::new(dir.path().to_path_buf(), event_tx));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let trace_observed = observed.clone();
        manager
            .handle()
            .set_trace_recorder(Some(Arc::new(move |event| {
                let observed = trace_observed.clone();
                Box::pin(async move {
                    observed.lock().unwrap().push(event);
                    Ok(())
                })
            })));
        let handle = manager.handle();
        handle.set_interactive(true);
        let request_manager = manager.clone();
        let task = tokio::spawn(async move {
            request_manager
                .request_computer("Click at (1, 1)", "click")
                .await
        });
        let AgentEvent::PermissionRequested { request } = events.recv().await.unwrap() else {
            panic!("expected permission request");
        };
        assert!(request
            .scopes
            .contains(&threadlane_protocol::PermissionScope::Session));
        assert!(handle.resolve(&request.id, PermissionDecision::AllowSession));
        assert_eq!(task.await.unwrap(), PermissionDecision::AllowSession);
        // Later actions in this session skip the prompt...
        assert_eq!(
            manager.request_computer("Type", "type").await,
            PermissionDecision::AllowOnce
        );
        assert!(
            events.try_recv().is_err(),
            "session grant must not prompt"
        );
        // ...but nothing is persisted: a fresh manager prompts again.
        let (event_tx, mut fresh_events) = tokio::sync::broadcast::channel(4);
        let restored = PermissionManager::new(dir.path().to_path_buf(), event_tx);
        restored.handle().set_interactive(true);
        let restored = Arc::new(restored);
        let task = tokio::spawn({
            let restored = restored.clone();
            async move { restored.request_computer("Click", "click").await }
        });
        let AgentEvent::PermissionRequested { request } = fresh_events.recv().await.unwrap()
        else {
            panic!("fresh manager must prompt");
        };
        assert!(restored.handle().resolve(&request.id, PermissionDecision::Deny));
        assert_eq!(task.await.unwrap(), PermissionDecision::Deny);
        // Trace shows the session grant and its reuse distinctly from Once.
        let observed = observed.lock().unwrap();
        let resolved: Vec<_> = observed
            .iter()
            .filter_map(|event| match event {
                PermissionTraceEvent::Resolved {
                    decision, scope, ..
                } => Some((decision.clone(), scope.clone())),
                _ => None,
            })
            .collect();
        assert!(
            resolved.contains(&(
                PermissionTraceDecision::Allowed,
                Some(PermissionTraceScope::Session)
            )),
            "session grant must trace Session scope: {resolved:?}"
        );
    }

    #[tokio::test]
    async fn permission_ids_are_session_scoped_and_unique_across_managers() {
        let dir = tempdir().unwrap();
        let (event_tx1, _) = tokio::sync::broadcast::channel(4);
        let (event_tx2, _) = tokio::sync::broadcast::channel(4);
        let manager1 = PermissionManager::new(dir.path().to_path_buf(), event_tx1);
        let manager2 = PermissionManager::new(dir.path().to_path_buf(), event_tx2);

        let id1 = manager1.generate_request_id();
        let id2 = manager1.generate_request_id();
        let id3 = manager2.generate_request_id();

        assert_ne!(id1, id2);
        assert_ne!(id1, id3);
        assert!(id1.starts_with("permission-"));
        assert!(id3.starts_with("permission-"));
    }

    #[test]
    fn save_permissions_round_trips_without_temporary_residue() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut permissions = PersistentPermissions::default();
        permissions.network_hosts.insert("example.com".into());
        permissions.computer_allowed = true;
        save_permissions(root, &permissions).unwrap();

        let reloaded = load_permissions(root);
        assert!(reloaded.network_hosts.contains("example.com"));
        assert!(reloaded.computer_allowed);

        // The uniquely-named temporary file is renamed into place: no residue
        // may remain alongside the committed permissions file.
        let residue: Vec<_> = std::fs::read_dir(root.join(".threadlane"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| {
                let name = name.to_string_lossy();
                name.ends_with(".tmp") || name.contains(".tmp.")
            })
            .collect();
        assert!(residue.is_empty(), "unexpected files: {residue:?}");
    }

    #[test]
    fn save_permissions_rejects_symlink_destination() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let permissions_dir = root.join(".threadlane");
        fs::create_dir_all(&permissions_dir).unwrap();
        let target_file = root.join("other_file.json");
        fs::write(&target_file, "{}").unwrap();

        let symlink_path = permissions_dir.join("permissions.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target_file, &symlink_path).unwrap();

        #[cfg(unix)]
        {
            let mut permissions = PersistentPermissions::default();
            permissions.network_hosts.insert("bad.com".into());
            let result = save_permissions(root, &permissions);
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("symlink"));
        }
    }
}
