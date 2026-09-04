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
    pub(crate) prompt_lock: Arc<tokio::sync::Mutex<()>>,
    pub session_file: PathBuf,
    mode: ExecutionMode,
    pub selected_model: String,
    pub system_prompt: String,
    pub harness_error: Option<String>,
    is_generating: AtomicBool,
    status: Mutex<SessionStatus>,
    pub(crate) recovery_loaded: AtomicBool,
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

    pub fn try_set_needle_enabled(&self, enabled: bool) -> bool {
        let Ok(mut agent) = self.agent.try_lock() else {
            return false;
        };
        agent.set_needle_enabled(enabled);
        true
    }

    pub(crate) async fn reload_extensions(&self) -> Result<usize, String> {
        let _guard = self.prompt_lock.lock().await;
        let mut agent = self.agent.lock().await;
        agent.reload_extensions().await
    }

    /// Settings the session's external agent offers, starting it if it is not
    /// running yet.
    ///
    /// Connecting is the point rather than a side effect: an agent reports its
    /// settings on `session/new`, so there is nothing to offer before then.
    /// Empty for a non-ACP model, since asking what an agent offers is a
    /// question the caller may ask about any selection.
    ///
    /// A turn holds the agent for its whole duration, so callers gate on
    /// [`Self::is_generating`] rather than letting this block on the lock.
    pub async fn acp_config_options(&self) -> Result<Vec<crate::AcpConfigOption>, String> {
        let _guard = self.prompt_lock.lock().await;
        let mut agent = self.agent.lock().await;
        agent.acp_config_options().await
    }

    /// Applies one agent-defined setting, addressed by the agent's own id.
    ///
    /// Returns the settings as the agent reports them afterwards: changing one
    /// can change another, because picking a different model changes which
    /// effort levels that model offers.
    pub async fn set_acp_config_option(
        &self,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<crate::AcpConfigOption>, String> {
        let _guard = self.prompt_lock.lock().await;
        let mut agent = self.agent.lock().await;
        agent.set_acp_config_option(config_id, value).await
    }
}
