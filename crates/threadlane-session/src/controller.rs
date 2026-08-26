//! Unified Session Controller for interactive chat and background tasks.
//!
//! Provides the shared execution core across surface adapters (GPUI, Supervisor, Headless),
//! adhering to the principle: One shared durable execution core; multiple thin surface adapters.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::coding_agent::{
    CodingAgent, CodingAgentCancellation, CodingAgentOptions, CodingAgentWorkHandle,
};
use crate::permission::{PermissionDecision, PermissionHandle};
use crate::ModelRoles;

/// Execution mode configured for a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Interactive desktop/user session (enables interactive permission prompts).
    Interactive,
    /// Headless or background autonomous execution (e.g. supervisor /task).
    Background,
}

/// Dynamic status of the session controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Ready,
    Working,
    Interrupted,
    Error(String),
}

/// Unified execution controller for an agent session.
///
/// Encapsulates the `CodingAgent` core, execution synchronization, cancellation,
/// permissions, work queues, and event streams across all surface adapters.
pub struct SessionController {
    pub agent: Arc<tokio::sync::Mutex<CodingAgent>>,
    pub cancellation: CodingAgentCancellation,
    pub work_handle: CodingAgentWorkHandle,
    permission_handle: PermissionHandle,
    pub prompt_lock: Arc<tokio::sync::Mutex<()>>,
    pub session_file: PathBuf,
    pub mode: ExecutionMode,
    pub selected_model: String,
    pub system_prompt: String,
    pub harness_error: Option<String>,
    is_generating: AtomicBool,
    status: Mutex<SessionStatus>,
    pub recovery_loaded: AtomicBool,
    needle_enabled: AtomicBool,
}

impl SessionController {
    /// Construct a new session controller with the specified options and execution mode.
    pub fn new(options: CodingAgentOptions, mode: ExecutionMode) -> Arc<Self> {
        let session_file = options
            .session_file
            .clone()
            .expect("SessionController requires a durable session file");
        let agent = CodingAgent::new(options);
        let cancellation = agent.cancellation_handle();
        let work_handle = agent.work_handle();
        let permission_handle = agent.permission_handle();
        if mode == ExecutionMode::Interactive {
            permission_handle.set_interactive(true);
        }
        let system_prompt = agent.system_prompt_snapshot().unwrap_or_default();
        let harness_error = agent.harness_error().map(str::to_owned);
        let status = if let Some(error) = &harness_error {
            SessionStatus::Error(error.clone())
        } else if agent.has_interrupted_work() {
            SessionStatus::Interrupted
        } else {
            SessionStatus::Ready
        };

        let selected_model = agent.model().to_string();
        let needle_enabled = agent.agent.config().needle_enabled;

        Arc::new(Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            cancellation,
            work_handle,
            permission_handle,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            session_file,
            mode,
            selected_model,
            system_prompt,
            harness_error,
            is_generating: AtomicBool::new(false),
            status: Mutex::new(status),
            recovery_loaded: AtomicBool::new(false),
            needle_enabled: AtomicBool::new(needle_enabled),
        })
    }

    pub fn session_file(&self) -> &Path {
        &self.session_file
    }

    pub fn mode(&self) -> ExecutionMode {
        self.mode
    }

    pub fn model(&self) -> &str {
        &self.selected_model
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn harness_error(&self) -> Option<&str> {
        self.harness_error.as_deref()
    }

    pub fn is_generating(&self) -> bool {
        self.is_generating.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> SessionStatus {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| SessionStatus::Error("Session controller unavailable".into()))
    }

    pub fn begin_generation(&self) -> Result<(), String> {
        self.is_generating
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "A generation is already running for this session".to_string())?;
        if let Ok(mut status) = self.status.lock() {
            *status = SessionStatus::Working;
        }
        Ok(())
    }

    pub fn finish_generation(&self, error: Option<String>) {
        self.is_generating.store(false, Ordering::SeqCst);
        if let Ok(mut status) = self.status.lock() {
            *status = error
                .map(SessionStatus::Error)
                .unwrap_or(SessionStatus::Ready);
        }
    }

    pub fn cancellation_handle(&self) -> CodingAgentCancellation {
        self.cancellation.clone()
    }

    pub fn work_handle(&self) -> CodingAgentWorkHandle {
        self.work_handle.clone()
    }

    pub fn permission_handle(&self) -> PermissionHandle {
        self.permission_handle.clone()
    }

    pub fn resolve_permission(&self, request_id: &str, decision: PermissionDecision) -> bool {
        self.permission_handle.resolve(request_id, decision)
    }

    pub fn cancel(&self) -> Result<(), String> {
        self.cancellation.cancel()
    }

    pub async fn set_model_roles(&self, roles: ModelRoles) {
        let mut agent = self.agent.lock().await;
        agent.set_model_roles(roles);
    }

    pub fn set_needle_enabled(&self, enabled: bool) {
        self.needle_enabled.store(enabled, Ordering::SeqCst);
    }

    pub async fn apply_needle_enabled(&self) {
        let mut agent = self.agent.lock().await;
        agent.set_needle_enabled(self.needle_enabled.load(Ordering::SeqCst));
    }

    pub async fn reload_extensions(&self) -> Result<usize, String> {
        let _guard = self.prompt_lock.lock().await;
        let mut agent = self.agent.lock().await;
        agent.reload_extensions().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_agent::CodingAgentOptions;

    #[tokio::test]
    async fn queued_needle_toggle_applies_after_busy_agent_releases_lock() {
        let temp = tempfile::tempdir().unwrap();
        let controller = SessionController::new(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "test-model".into(),
                work_dir: temp.path().to_path_buf(),
                session_file: Some(temp.path().join("session.jsonl")),
                system_prompt: Default::default(),
                agent_config: None,
                coding_config: None,
            },
            ExecutionMode::Interactive,
        );
        let guard = controller.agent.lock().await;
        controller.set_needle_enabled(true);
        let update = tokio::spawn({
            let controller = controller.clone();
            async move { controller.apply_needle_enabled().await }
        });

        tokio::task::yield_now().await;
        drop(guard);
        update.await.unwrap();

        assert!(controller.agent.lock().await.agent.config().needle_enabled);
    }

    #[tokio::test]
    async fn latest_needle_toggle_wins_when_waiters_apply_in_reverse_order() {
        let temp = tempfile::tempdir().unwrap();
        let controller = SessionController::new(
            CodingAgentOptions {
                api_key: "test-key".into(),
                account_id: None,
                model: "test-model".into(),
                work_dir: temp.path().to_path_buf(),
                session_file: Some(temp.path().join("session.jsonl")),
                system_prompt: Default::default(),
                agent_config: None,
                coding_config: None,
            },
            ExecutionMode::Interactive,
        );
        let guard = controller.agent.lock().await;
        let (true_ready_tx, true_ready_rx) = tokio::sync::oneshot::channel();
        let true_gate = Arc::new(tokio::sync::Notify::new());
        controller.set_needle_enabled(true);
        let stale_true = tokio::spawn({
            let controller = controller.clone();
            let true_gate = true_gate.clone();
            async move {
                true_ready_tx.send(()).unwrap();
                true_gate.notified().await;
                controller.apply_needle_enabled().await;
            }
        });
        true_ready_rx.await.unwrap();

        controller.set_needle_enabled(false);
        let current_false = tokio::spawn({
            let controller = controller.clone();
            async move { controller.apply_needle_enabled().await }
        });
        drop(guard);
        current_false.await.unwrap();
        true_gate.notify_one();
        stale_true.await.unwrap();

        assert!(!controller.agent.lock().await.agent.config().needle_enabled);
    }
}
