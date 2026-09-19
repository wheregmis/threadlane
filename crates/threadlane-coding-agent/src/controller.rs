//! Unified Session Controller for interactive chat.
//!
//! Provides the shared execution core across surface adapters (GPUI, Headless),
//! adhering to the principle: One shared durable execution core; multiple thin surface adapters.
//!
//! This module is also the canonical home of the session-runtime surface
//! (`SessionRuntime` alias, status text, blocking-pool constructor, and the
//! `test_support` provider-injection helper) used by the GPUI crates.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::cancellation::CodingAgentCancellation;
use crate::options::CodingAgentOptions;
use crate::runtime::CodingAgent;
use crate::scheduler::CodingAgentWorkHandle;
use threadlane_acp::AcpConfigOption;
use threadlane_permission::{PermissionDecision, PermissionHandle};
use threadlane_protocol::{AgentEvent, ImageAttachment, ReasoningEffort};
use threadlane_question::QuestionHandle;
use threadlane_runtime::harness::{EventError, HarnessEvent, Subscription};
use threadlane_runtime::ModelRoles;

/// Dynamic status of the session controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Ready,
    Working,
    Interrupted,
    Error(String),
}

/// Historical alias: the GPUI session runtime *is* the session controller.
pub type SessionRuntime = SessionController;
/// Historical alias for the controller status.

/// Owned lifecycle for an opt-in session scheduler supervisor.
pub struct SchedulerSupervisorHandle {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl SchedulerSupervisorHandle {
    /// Signal supervisor shutdown and wait until its task exits.
    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
struct SchedulerSupervisorLease(Arc<AtomicBool>);

impl Drop for SchedulerSupervisorLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Drop for SchedulerSupervisorHandle {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
pub type SessionRuntimeStatus = SessionStatus;

pub fn runtime_status_text(status: SessionRuntimeStatus) -> Option<String> {
    match status {
        SessionRuntimeStatus::Ready => None,
        SessionRuntimeStatus::Working => Some("Working…".into()),
        SessionRuntimeStatus::Interrupted => {
            Some("Turn interrupted · Safe replay checkpoints available".into())
        }
        SessionRuntimeStatus::Error(error) => Some(error),
    }
}

/// Construct a session controller on the shared Tokio blocking pool. WASI
/// extension loading needs the larger stack and reactor provided there.
pub fn spawn_session_runtime_construction(
    options: CodingAgentOptions,
) -> tokio::task::JoinHandle<std::sync::Arc<SessionController>> {
    threadlane_provider::exec::get_runtime().spawn_blocking(move || SessionController::new(options))
}

/// Narrow adapters for cross-crate integration tests.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    use std::sync::Arc;

    use threadlane_protocol::ProviderPort;

    use crate::{CodingAgent, CodingAgentOptions};

    pub fn coding_agent_with_provider(
        options: CodingAgentOptions,
        provider: Arc<dyn ProviderPort>,
    ) -> CodingAgent {
        CodingAgent::new_with_provider(options, provider)
    }
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
    question_handle: QuestionHandle,
    pub(crate) prompt_lock: Arc<tokio::sync::Mutex<()>>,
    pub session_file: PathBuf,
    pub selected_model: String,
    pub(crate) reasoning_effort: ReasoningEffort,
    pub system_prompt: String,
    pub harness_error: Option<String>,
    is_generating: AtomicBool,
    scheduler_supervisor_active: Arc<AtomicBool>,
    status: Mutex<SessionStatus>,
}
impl SessionController {

    /// Subscribe to replayable durable harness events for this session.
    pub fn subscribe_durable_events(&self) -> Result<Subscription, EventError> {
        let agent = self.agent.try_lock().map_err(|_| {
            EventError::Unavailable("session agent is currently busy".to_owned())
        })?;
        agent.subscribe_durable_events()
    }

    /// Poll durable harness events without waiting for the agent execution lock.
    pub fn poll_durable_events(
        &self,
        subscription: &mut Subscription,
    ) -> Result<Vec<HarnessEvent>, EventError> {
        let agent = self.agent.try_lock().map_err(|_| {
            EventError::Unavailable("session agent is currently busy".to_owned())
        })?;
        agent.poll_durable_events(subscription)
    }
    /// Construct a new interactive session controller.
    pub fn new(options: CodingAgentOptions) -> Arc<Self> {
        let session_file = options
            .session_file
            .clone()
            .expect("SessionController requires a durable session file");
        let agent = CodingAgent::new(options);
        let cancellation = agent.cancellation_handle();
        let work_handle = agent.work_handle();
        let permission_handle = agent.permission_handle();
        let question_handle = agent.question_handle();
        permission_handle.set_interactive(true);
        question_handle.set_interactive(true);
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
        let reasoning_effort = agent.agent.reasoning_effort();

        Arc::new(Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            cancellation,
            work_handle,
            permission_handle,
            question_handle,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            session_file,
            selected_model,
            reasoning_effort,
            system_prompt,
            harness_error,
            is_generating: AtomicBool::new(false),
            scheduler_supervisor_active: Arc::new(AtomicBool::new(false)),
            status: Mutex::new(status),
        })
    }

    pub fn session_file(&self) -> &Path {
        &self.session_file
    }

    pub fn model(&self) -> &str {
        &self.selected_model
    }

    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
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

    /// Launch the single scheduler driver for this session.
    ///
    /// The caller must not concurrently drive interactive work through the
    /// same `CodingAgent` while this task owns its mutex. The returned task
    /// ends after `stop` is signaled, making supervisor lifecycle explicit to
    /// the surface adapter rather than spawning hidden background work.
    pub fn spawn_scheduler_supervisor(
        self: &Arc<Self>,
        stop: tokio::sync::oneshot::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        let (result_tx, _result_rx) = tokio::sync::mpsc::unbounded_channel();
        self.spawn_scheduler_supervisor_with_results(stop, result_tx)
    }

    /// Launch the scheduler driver and stream completed scheduled results.
    pub fn spawn_scheduler_supervisor_with_results(
        self: &Arc<Self>,
        mut stop: tokio::sync::oneshot::Receiver<()>,
        result_tx: tokio::sync::mpsc::UnboundedSender<Option<Result<String, String>>>,
    ) -> tokio::task::JoinHandle<()> {
        let agent = self.agent.clone();
        let work_handle = self.work_handle.clone();
        let active = self.scheduler_supervisor_active.clone();
        threadlane_provider::exec::get_runtime().spawn(async move {
            let _lease = SchedulerSupervisorLease(active);
            loop {
                tokio::select! {
                    _ = &mut stop => break,
                    _ = work_handle.wait_for_work() => {}
                }
                let result = {
                    let mut agent = agent.lock().await;
                    agent.execute_scheduled_work().await
                };
                let _ = result_tx.send(result);
            }
        })
    }

    /// Create an explicitly owned supervisor lifecycle handle.
    pub fn start_scheduler_supervisor(
        self: &Arc<Self>,
    ) -> Result<SchedulerSupervisorHandle, &'static str> {
        if self
            .scheduler_supervisor_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("scheduler supervisor already running");
        }
        let (stop, receiver) = tokio::sync::oneshot::channel();
        let task = self.spawn_scheduler_supervisor(receiver);
        Ok(SchedulerSupervisorHandle {
            stop: Some(stop),
            task: Some(task),
        })
    }

    /// Start a supervisor and return its completion stream to the adapter.
    pub fn start_scheduler_supervisor_with_results(
        self: &Arc<Self>,
    ) -> Result<
        (
            SchedulerSupervisorHandle,
            tokio::sync::mpsc::UnboundedReceiver<Option<Result<String, String>>>,
        ),
        &'static str,
    > {
        if self
            .scheduler_supervisor_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("scheduler supervisor already running");
        }
        let (stop, receiver) = tokio::sync::oneshot::channel();
        let (result_tx, result_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = self.spawn_scheduler_supervisor_with_results(receiver, result_tx);
        Ok((
            SchedulerSupervisorHandle {
                stop: Some(stop),
                task: Some(task),
            },
            result_rx,
        ))
    }

    /// Report whether this session currently has an owned scheduler driver.
    pub fn scheduler_supervisor_running(&self) -> bool {
        self.scheduler_supervisor_active.load(Ordering::SeqCst)
    }

    pub fn permission_handle(&self) -> PermissionHandle {
        self.permission_handle.clone()
    }

    pub fn resolve_permission(&self, request_id: &str, decision: PermissionDecision) -> bool {
        self.permission_handle.resolve(request_id, decision)
    }

    pub fn question_handle(&self) -> QuestionHandle {
        self.question_handle.clone()
    }

    pub fn resolve_question(
        &self,
        request_id: &str,
        answer: threadlane_protocol::QuestionAnswer,
    ) -> bool {
        self.question_handle.resolve(request_id, answer)
    }

    pub fn cancel(&self) -> Result<(), String> {
        self.cancellation.cancel()
    }

    /// Drive one interactive turn on the shared Tokio runtime: generation
    /// guard, task registration, ACP pre-selection, git-branch fact, event
    /// pump, and cleanup. The only UI-owned parts are the four sinks, which
    /// the caller maps onto its own stream events; everything else here is
    /// engine policy shared by every surface adapter.
    pub fn spawn_interactive_turn<F1, F2, F3, F4>(
        self: &Arc<Self>,
        text: String,
        images: Vec<ImageAttachment>,
        effort: ReasoningEffort,
        work_dir: PathBuf,
        pending_acp: Vec<(String, String)>,
        on_agent_event: F1,
        on_output_text: F2,
        on_acp_options: F3,
        on_finished: F4,
    ) -> Result<(), String>
    where
        F1: Fn(AgentEvent) + Send + Sync + 'static,
        F2: Fn(String) + Send + Sync + 'static,
        F3: Fn(Vec<AcpConfigOption>, Option<String>, Option<(String, String)>)
            + Send
            + Sync
            + 'static,
        F4: Fn() + Send + Sync + 'static,
    {
        self.begin_generation()?;
        struct RunCleanup<F4: Fn()> {
            runtime: Arc<SessionController>,
            registration_id: u64,
            on_finished: F4,
            error: Option<String>,
        }
        impl<F4: Fn()> Drop for RunCleanup<F4> {
            fn drop(&mut self) {
                self.runtime
                    .cancellation
                    .finish_active_run(self.registration_id);
                self.runtime.finish_generation(self.error.clone());
                (self.on_finished)();
            }
        }
        let task_runtime = self.clone();
        let (registration_tx, registration_rx) = tokio::sync::oneshot::channel();
        let task = threadlane_provider::exec::get_runtime().spawn(async move {
            let Ok(registration_id) = registration_rx.await else {
                task_runtime.finish_generation(Some("Generation registration failed".into()));
                return;
            };

            let mut cleanup = RunCleanup {
                runtime: task_runtime.clone(),
                registration_id,
                on_finished,
                error: None,
            };

            // Apply New-task ACP selections before the first turn. A failure must
            // abort this turn: otherwise the prompt would run under the agent's
            // default configuration rather than the picker selection.
            for (config_id, value) in pending_acp {
                match task_runtime.set_acp_config_option(&config_id, &value).await {
                    Ok(options) => {
                        on_acp_options(options, None, None);
                    }
                    Err(error) => {
                        cleanup.error = Some(error.clone());
                        on_acp_options(Vec::new(), Some(error), Some((config_id, value)));
                        return;
                    }
                }
            }

            let turn_span = tracing::info_span!("chat.turn");
            tracing::info!(parent: &turn_span, "starting chat turn");
            let git_branch =
                tokio::task::spawn_blocking(move || threadlane_git::current_branch(&work_dir))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten();
            let mut agent = task_runtime.agent.lock().await;
            if let Some(branch) = git_branch {
                if let Err(error) = agent.set_fact("git_branch", &branch) {
                    cleanup.error = Some(error.clone());
                    on_agent_event(AgentEvent::AgentError { error });
                    return;
                }
            }
            agent.set_reasoning_effort(effort).await;
            let mut events = agent.subscribe();
            let run_error = {
                let run = agent.handle_input_with_images(&text, images);
                tokio::pin!(run);
                let mut run_error = None;
                let mut saw_agent_error = false;

                loop {
                    tokio::select! {
                        result = &mut run => {
                            match result {
                                Some(Ok(output)) if !output.is_empty() => {
                                    on_output_text(output);
                                }
                                Some(Err(error)) => {
                                    tracing::error!(error = %error, "chat turn failed");
                                    run_error = Some(error);
                                }
                                _ => {}
                            }
                            while let Ok(event) = events.try_recv() {
                                saw_agent_error |= matches!(event, AgentEvent::AgentError { .. });
                                on_agent_event(event);
                            }
                            if let Some(error) = run_error.as_ref().filter(|_| !saw_agent_error) {
                                on_agent_event(AgentEvent::AgentError { error: error.clone() });
                            }
                            break;
                        }
                        event = events.recv() => {
                            match event {
                                Ok(event) => {
                                    saw_agent_error |= matches!(event, AgentEvent::AgentError { .. });
                                    on_agent_event(event);
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    }
                }
                run_error
            };

            tracing::info!(error = ?run_error, "chat turn finished");
            // Read after the turn because an external agent defines its own
            // settings and only reports them once a session is open. This is the
            // free path: the turn already connected, so nothing is started here.
            let acp_options = agent.acp_user_config_options();
            if !acp_options.is_empty() {
                on_acp_options(acp_options, None, None);
            }
            drop(agent);
            cleanup.error = run_error;
        });

        let registration_id = match self.cancellation.track_active_run(task.abort_handle()) {
            Ok(id) => id,
            Err(error) => {
                task.abort();
                self.finish_generation(Some(error.clone()));
                return Err(error);
            }
        };
        if registration_tx.send(registration_id).is_err() {
            self.cancellation.finish_active_run(registration_id);
            let error = "Generation task stopped before registration".to_string();
            self.finish_generation(Some(error.clone()));
            return Err(error);
        }
        Ok(())
    }

    pub async fn set_model_roles(&self, roles: ModelRoles) {
        let mut agent = self.agent.lock().await;
        agent.set_model_roles(roles);
    }

    pub async fn reload_extensions(&self) -> Result<usize, String> {
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
    pub async fn acp_config_options(&self) -> Result<Vec<threadlane_acp::AcpConfigOption>, String> {
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
    ) -> Result<Vec<threadlane_acp::AcpConfigOption>, String> {
        let _guard = self.prompt_lock.lock().await;
        let mut agent = self.agent.lock().await;
        agent.set_acp_config_option(config_id, value).await
    }
}
