use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use threadlane_runtime::{AgentEvent, PermissionRequest, PermissionScope};
use tokio::sync::oneshot;

const PERMISSIONS_FILE: &str = ".threadlane/permissions.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    AllowOnce,
    AllowAlways,
    Deny,
}

#[derive(Clone)]
pub struct PermissionHandle {
    inner: Arc<PermissionManagerInner>,
}

#[derive(Clone, Debug)]
pub(crate) enum PermissionTraceEvent {
    Requested {
        request_id: String,
        capability: String,
        scopes: Vec<threadlane_runtime::harness::PermissionTraceScope>,
        detail_sha256: String,
        source: threadlane_runtime::harness::PermissionTraceSource,
    },
    Resolved {
        request_id: String,
        decision: threadlane_runtime::harness::PermissionTraceDecision,
        scope: Option<threadlane_runtime::harness::PermissionTraceScope>,
        source: threadlane_runtime::harness::PermissionTraceSource,
        remembered: bool,
    },
}

type PermissionTraceRecorder = Arc<
    dyn Fn(PermissionTraceEvent) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub(crate) struct PermissionManager {
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
}

/// Persistent permissions remembered across agent runs and restarts.
///
/// Persistent grants are currently capability-scoped to network host connections.
/// Filesystem, execution, and environment capabilities are scoped to the active session.
#[derive(Default, Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct PersistentPermissions {
    #[serde(default)]
    network_hosts: HashSet<String>,
}

impl PermissionManager {
    pub(crate) fn new(
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
                }),
            },
            event_tx,
        }
    }

    pub(crate) fn handle(&self) -> PermissionHandle {
        self.handle.clone()
    }

    fn generate_request_id(&self) -> String {
        self.handle.generate_request_id()
    }

    pub(crate) fn network_host_is_approved(&self, host: &str) -> bool {
        self.handle
            .inner
            .persistent
            .lock()
            .is_ok_and(|permissions| permissions.network_hosts.contains(host))
    }

    async fn record_trace(&self, event: PermissionTraceEvent) -> Result<(), String> {
        self.handle.record_trace(event).await
    }

    pub(crate) async fn trace_preapproved_network_host(
        &self,
        url: &str,
        persisted: bool,
    ) -> Result<(), String> {
        let id = self.generate_request_id();
        let source = if persisted {
            threadlane_runtime::harness::PermissionTraceSource::PersistedGrant
        } else {
            threadlane_runtime::harness::PermissionTraceSource::Policy
        };
        let scope = if persisted {
            threadlane_runtime::harness::PermissionTraceScope::Project
        } else {
            threadlane_runtime::harness::PermissionTraceScope::Session
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
            decision: threadlane_runtime::harness::PermissionTraceDecision::Allowed,
            scope: Some(scope),
            source,
            remembered: persisted,
        })
        .await
    }

    pub(crate) async fn request_network_host(&self, host: &str, url: &str) -> PermissionDecision {
        let id = self.generate_request_id();
        let interactive = self.handle.inner.interactive.load(Ordering::SeqCst);
        let requested = PermissionTraceEvent::Requested {
            request_id: id.clone(),
            capability: "network".into(),
            scopes: vec![
                threadlane_runtime::harness::PermissionTraceScope::Once,
                threadlane_runtime::harness::PermissionTraceScope::Project,
            ],
            detail_sha256: format!("{:x}", Sha256::digest(url.as_bytes())),
            source: if interactive {
                threadlane_runtime::harness::PermissionTraceSource::User
            } else {
                threadlane_runtime::harness::PermissionTraceSource::UnattendedDefault
            },
        };
        if self.record_trace(requested).await.is_err() {
            return PermissionDecision::Deny;
        }
        if !interactive {
            let _ = self
                .record_trace(PermissionTraceEvent::Resolved {
                    request_id: id,
                    decision: threadlane_runtime::harness::PermissionTraceDecision::Denied,
                    scope: None,
                    source: threadlane_runtime::harness::PermissionTraceSource::UnattendedDefault,
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
                threadlane_runtime::harness::PermissionTraceDecision::Allowed,
                Some(threadlane_runtime::harness::PermissionTraceScope::Once),
            ),
            PermissionDecision::AllowAlways => (
                threadlane_runtime::harness::PermissionTraceDecision::Allowed,
                Some(threadlane_runtime::harness::PermissionTraceScope::Project),
            ),
            PermissionDecision::Deny => (
                threadlane_runtime::harness::PermissionTraceDecision::Denied,
                None,
            ),
        };
        if self
            .record_trace(PermissionTraceEvent::Resolved {
                request_id: id,
                decision: trace_decision,
                scope,
                source: threadlane_runtime::harness::PermissionTraceSource::User,
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
    pub(crate) fn set_trace_recorder(&self, recorder: Option<PermissionTraceRecorder>) {
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
    /// persists nothing: it renders a prompt, waits for the answer, and returns
    /// it. Without a UI attached there is no informed consent to give, so the
    /// answer is [`PermissionDecision::Deny`] rather than a silent allow.
    pub(crate) async fn request_external(
        &self,
        event_tx: &tokio::sync::broadcast::Sender<AgentEvent>,
        capability: &str,
        title: String,
        detail: String,
        allow_always: bool,
    ) -> PermissionDecision {
        let id = self.generate_request_id();
        let interactive = self.is_interactive();
        let mut scopes = vec![PermissionScope::Once];
        if allow_always {
            scopes.push(PermissionScope::Always);
        }
        let trace_scopes = scopes
            .iter()
            .map(|scope| match scope {
                PermissionScope::Always => {
                    threadlane_runtime::harness::PermissionTraceScope::Project
                }
                _ => threadlane_runtime::harness::PermissionTraceScope::Once,
            })
            .collect();
        let requested = PermissionTraceEvent::Requested {
            request_id: id.clone(),
            capability: capability.to_string(),
            scopes: trace_scopes,
            detail_sha256: format!("{:x}", Sha256::digest(detail.as_bytes())),
            source: if interactive {
                threadlane_runtime::harness::PermissionTraceSource::User
            } else {
                threadlane_runtime::harness::PermissionTraceSource::UnattendedDefault
            },
        };
        if self.record_trace(requested).await.is_err() {
            return PermissionDecision::Deny;
        }
        if !interactive {
            let _ = self
                .record_trace(PermissionTraceEvent::Resolved {
                    request_id: id,
                    decision: threadlane_runtime::harness::PermissionTraceDecision::Denied,
                    scope: None,
                    source: threadlane_runtime::harness::PermissionTraceSource::UnattendedDefault,
                    remembered: false,
                })
                .await;
            return PermissionDecision::Deny;
        }
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.inner.pending.lock() {
            pending.insert(id.clone(), tx);
        } else {
            return PermissionDecision::Deny;
        }
        let request = PermissionRequest {
            id: id.clone(),
            capability: capability.to_string(),
            title,
            detail,
            scopes,
        };
        if event_tx
            .send(AgentEvent::PermissionRequested { request })
            .is_err()
        {
            self.remove_pending(&id);
            return PermissionDecision::Deny;
        }
        let guard = PendingRequestGuard {
            handle: self.clone(),
            request_id: id.clone(),
        };
        let decision = rx.await.unwrap_or(PermissionDecision::Deny);
        drop(guard);
        let _ = self
            .record_trace(PermissionTraceEvent::Resolved {
                request_id: id,
                decision: match decision {
                    PermissionDecision::Deny => {
                        threadlane_runtime::harness::PermissionTraceDecision::Denied
                    }
                    _ => threadlane_runtime::harness::PermissionTraceDecision::Allowed,
                },
                scope: match decision {
                    PermissionDecision::AllowAlways => {
                        Some(threadlane_runtime::harness::PermissionTraceScope::Project)
                    }
                    PermissionDecision::AllowOnce => {
                        Some(threadlane_runtime::harness::PermissionTraceScope::Once)
                    }
                    PermissionDecision::Deny => None,
                },
                source: threadlane_runtime::harness::PermissionTraceSource::User,
                remembered: false,
            })
            .await;
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
                    source: threadlane_runtime::harness::PermissionTraceSource::UnattendedDefault,
                    ..
                },
                PermissionTraceEvent::Resolved {
                    decision: threadlane_runtime::harness::PermissionTraceDecision::Denied,
                    source: threadlane_runtime::harness::PermissionTraceSource::UnattendedDefault,
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
