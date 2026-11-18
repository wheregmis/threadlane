//! [`Runner`]: the lane cycle of docs/milestone-1.md §3 and the claim loop.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value as Json, json};
use threadlane_core::backend::{
    AgentBackend, BackendError, BackendEvent, BackendRun, NativeSession, Probe, RunRequest,
    encode_backends,
};
use threadlane_core::forge::{Forge, OPEN_PULL_REQUEST, PUBLISH_BRANCH};
use threadlane_core::ids::{DecisionId, RunId};
use threadlane_core::link::{
    Begin, HubLink, HubMessage, LinkError, Perform, RunAssignment, RunFailure,
};
use threadlane_core::model::{BackendKind, EventKind, LaneKind, Lease, MachineReport};
use threadlane_core::runtime::steps::perform;
use threadlane_core::runtime::steps::runner::{
    AFTER_BACKEND_RUN, AFTER_BACKEND_START, AFTER_CHECKPOINT, AFTER_CLAIM, AFTER_COMMIT,
    AFTER_DEBOUNCE, AFTER_PREPARE_WORKSPACE, AFTER_RECHECK, AFTER_RELEASE, AFTER_START_RUN,
};
use threadlane_core::runtime::{Killed, Step, StepHook};
use threadlane_core::time::{Clock, Timestamp};
use threadlane_core::workspace::{MergeCheck, Workspaces, task_branch};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinSet};
use tracing::{Instrument, debug, info, info_span, warn};

use crate::config::RunnerConfig;
use crate::error::RunnerError;
use crate::lane::{LaneExit, LaneState, Stop};
use crate::mirror::{Flush, Mirror};
use crate::tools::LaneTools;

/// Claims lanes from a hub and executes their runs on a backend
/// (docs/milestone-1.md §8).
#[derive(Debug)]
pub struct Runner {
    inner: Arc<Inner>,
}

impl Runner {
    /// A runner that works lanes granted by `link` on `backends`, with task
    /// worktrees from `workspaces`, performing the calls you approve through
    /// `forge`.
    #[must_use]
    pub fn new(
        link: Arc<dyn HubLink>,
        backends: Vec<Arc<dyn AgentBackend>>,
        workspaces: Arc<dyn Workspaces>,
        forge: Arc<dyn Forge>,
        clock: Arc<dyn Clock>,
        hook: Arc<dyn StepHook>,
        config: RunnerConfig,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                link,
                backends,
                workspaces,
                forge,
                clock,
                hook,
                config,
                lanes: Mutex::default(),
                probes: Mutex::default(),
                gh_login: AtomicBool::new(false),
            }),
        }
    }

    /// Probes the backends and the forge login, and sends `hello`.
    ///
    /// A backend whose probe fails is reported as unusable rather than
    /// failing the call.
    ///
    /// # Errors
    ///
    /// [`RunnerError::Link`] if the hub refuses or cannot be reached, and
    /// [`RunnerError::Killed`] on an injected crash.
    pub async fn hello(&self) -> Result<(), RunnerError> {
        let mut probes = Vec::with_capacity(self.inner.backends.len());
        for backend in &self.inner.backends {
            probes.push((backend.kind(), probe(backend.as_ref()).await));
        }
        *self.inner.probes.lock().expect("probes lock poisoned") = probes;
        let gh_login = self.inner.forge.logged_in().await;
        self.inner.gh_login.store(gh_login, Ordering::Relaxed);
        let report = self.inner.report(self.inner.load());
        self.inner.link.hello(&report).await?;
        Ok(())
    }

    /// Asks for one lane and, if granted, works it to completion on this task.
    /// Returns whether a lane was granted. Sends no heartbeat. For tests and
    /// simulation.
    ///
    /// Failures inside the lane are reported to the hub, not returned.
    ///
    /// # Errors
    ///
    /// [`RunnerError::Link`] if asking for work fails, and
    /// [`RunnerError::Killed`] when an injected crash stops the runner, at
    /// which point nothing more was sent to the hub.
    pub async fn work_once(&self) -> Result<bool, RunnerError> {
        let Some(lease) = self.inner.link.request_work().await? else {
            return Ok(false);
        };
        let registration = Registration::new(&self.inner, lease);
        let _messages = MessagePump::start(Arc::clone(&self.inner));
        self.inner.work_lane(&registration.lane).await?;
        Ok(true)
    }

    /// The long-running loop: hello, then claim on start, on every nudge and
    /// every `poll`, run up to `max_parallel` lanes at once, heartbeat every
    /// `heartbeat`. Returns when `shutdown` completes, after stopping the
    /// backends of running lanes and releasing their leases.
    ///
    /// # Errors
    ///
    /// [`RunnerError::Io`] if the lanes directory cannot be created,
    /// [`RunnerError::Link`] if `hello` fails, and [`RunnerError::Killed`]
    /// when an injected crash stops the runner or any of its lanes.
    pub async fn run(&self, shutdown: impl Future<Output = ()> + Send) -> Result<(), RunnerError> {
        tokio::fs::create_dir_all(&self.inner.config.lanes_dir).await?;
        self.hello().await?;

        let clock = &self.inner.clock;
        let config = &self.inner.config;
        let mut shutdown = pin!(shutdown);
        let mut messages = MessagePump::start(Arc::clone(&self.inner));
        let mut heartbeat = clock.sleep(config.heartbeat);
        let mut poll = clock.sleep(config.poll);
        let mut lanes = JoinSet::new();
        let mut saturated = self.claim(&mut lanes).await?;

        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                Some(joined) = lanes.join_next() => {
                    lane_finished(joined)?;
                    if saturated {
                        saturated = self.claim(&mut lanes).await?;
                    }
                }
                Some(message) = messages.next() => {
                    if message == HubMessage::Nudge {
                        saturated = self.claim(&mut lanes).await?;
                    } else {
                        debug!(?message, "ignoring a hub message this runner does not handle yet");
                    }
                }
                () = &mut heartbeat => {
                    self.inner.heartbeat().await?;
                    heartbeat = clock.sleep(config.heartbeat);
                }
                () = &mut poll => {
                    saturated = self.claim(&mut lanes).await?;
                    poll = clock.sleep(config.poll);
                }
            }
        }
        self.shut_down(lanes).await
    }

    /// Asks for lanes until the hub has none or `max_parallel` are running.
    /// Returns whether it stopped because every slot is taken.
    async fn claim(&self, lanes: &mut JoinSet<Result<(), Killed>>) -> Result<bool, RunnerError> {
        let max_parallel = usize::try_from(self.inner.config.max_parallel).unwrap_or(usize::MAX);
        while lanes.len() < max_parallel {
            match self.inner.link.request_work().await {
                Ok(Some(lease)) => {
                    let registration = Registration::new(&self.inner, lease);
                    let inner = Arc::clone(&self.inner);
                    lanes.spawn(async move { inner.work_lane(&registration.lane).await });
                }
                Ok(None) => return Ok(false),
                Err(LinkError::Killed(killed)) => return Err(killed.into()),
                Err(error) => {
                    warn!(%error, "could not ask the hub for work");
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Stops every running lane, which stops its backend and releases its
    /// lease, and waits for all of them.
    async fn shut_down(&self, mut lanes: JoinSet<Result<(), Killed>>) -> Result<(), RunnerError> {
        let running = self.inner.running();
        info!(lanes = running.len(), "runner shutting down");
        for lane in running {
            lane.signal(Stop::Shutdown);
        }
        let mut outcome = Ok(());
        while let Some(joined) = lanes.join_next().await {
            if let Err(error) = lane_finished(joined) {
                outcome = outcome.and(Err(error));
            }
        }
        outcome
    }
}

/// A lane task's result: a kill ends the runner, a panic is re-raised.
fn lane_finished(joined: Result<Result<(), Killed>, JoinError>) -> Result<(), RunnerError> {
    match joined {
        Ok(result) => Ok(result?),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => {
            warn!(%error, "lane task was cancelled");
            Ok(())
        }
    }
}

/// What the cycle does after a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterRun {
    /// Ask the hub for the next run in the lane (§4.3 step 6).
    Recheck,
    /// Release the lane; the hub's backoff decides when it runs again.
    Release,
}

/// How a run's event stream ended.
#[derive(Debug)]
enum Ended {
    Terminal(BackendEvent),
    /// The stream closed without a terminal event.
    Closed,
    WallTime,
    /// The agent went on working after a call of the run opened a decision.
    Deferred,
}

/// What woke the event loop.
enum Next {
    Stop(Stop),
    WallTime,
    Event(Option<BackendEvent>),
}

#[derive(Debug, Default)]
struct RunningLanes {
    next_key: u64,
    lanes: BTreeMap<u64, Arc<LaneState>>,
}

#[derive(Debug)]
struct Inner {
    link: Arc<dyn HubLink>,
    backends: Vec<Arc<dyn AgentBackend>>,
    workspaces: Arc<dyn Workspaces>,
    forge: Arc<dyn Forge>,
    clock: Arc<dyn Clock>,
    hook: Arc<dyn StepHook>,
    config: RunnerConfig,
    lanes: Mutex<RunningLanes>,
    /// What `hello` found, reused by heartbeats.
    probes: Mutex<Vec<(BackendKind, Probe)>>,
    /// Whether `hello` found the forge logged in.
    gh_login: AtomicBool,
}

impl Inner {
    /// Works one granted lane. Only a kill is returned: every other ending
    /// has been dealt with, by releasing the lane or by abandoning it.
    async fn work_lane(&self, lane: &Arc<LaneState>) -> Result<(), Killed> {
        let lane_id = lane.lease().lane_id;
        match self.cycle(lane).await {
            Ok(()) => Ok(()),
            Err(LaneExit::Killed(killed)) => Err(killed),
            Err(LaneExit::LeaseLost) => {
                warn!(lane = %lane_id, "lease lost; abandoning the lane");
                Ok(())
            }
            Err(LaneExit::Shutdown | LaneExit::UserStopped) => self.release(lane).await,
            Err(LaneExit::Link(error) | LaneExit::Release(error)) => {
                warn!(lane = %lane_id, %error, "hub request failed; releasing the lane");
                self.release(lane).await
            }
        }
    }

    /// docs/milestone-1.md §3, runner side.
    async fn cycle(&self, lane: &Arc<LaneState>) -> Result<(), LaneExit> {
        self.reach(AFTER_CLAIM)?;
        self.debounce(lane).await?;
        self.reach(AFTER_DEBOUNCE)?;
        loop {
            lane.check()?;
            let Begin::Run(assignment) = self.link.begin_run(&lane.lease()).await? else {
                self.reach(AFTER_RECHECK)?;
                break;
            };
            self.reach(AFTER_START_RUN)?;
            let span = info_span!("run", run = %assignment.run_id, trace_id = %assignment.trace_id);
            if self.execute(lane, &assignment).instrument(span).await? == AfterRun::Release {
                break;
            }
        }
        Ok(self.release(lane).await?)
    }

    /// Waits the debounce window if a user message or a task's result is
    /// pending (§4.8), so that messages in a row, and the results of tasks
    /// that end close together, reach the lead in one run.
    async fn debounce(&self, lane: &LaneState) -> Result<(), LaneExit> {
        let pending = self.link.pending(&lane.lease()).await?;
        if !pending
            .iter()
            .any(|event| matches!(event.kind, EventKind::UserMessage | EventKind::TaskResult))
        {
            return Ok(());
        }
        tokio::select! {
            biased;
            stop = lane.stopped() => Err(stop.into()),
            () = self.clock.sleep(self.config.debounce) => Ok(()),
        }
    }

    /// Executes one assigned run. A hub request that fails mid-run still
    /// fails the run before the lane is released.
    async fn execute(
        &self,
        lane: &Arc<LaneState>,
        assignment: &RunAssignment,
    ) -> Result<AfterRun, LaneExit> {
        match self.attempt(lane, assignment).await {
            Err(LaneExit::UserStopped) => {
                if assignment.lane_kind == LaneKind::Task {
                    if let Err(error) = self.checkpoint(assignment).await {
                        warn!(run = %assignment.run_id, %error, "could not checkpoint the stopped run");
                    }
                    self.reach(AFTER_CHECKPOINT)?;
                }
                self.link
                    .fail(&lane.lease(), assignment.run_id, &RunFailure::Stopped)
                    .await?;
                self.reach(threadlane_core::runtime::steps::runner::AFTER_STOP)?;
                Ok(AfterRun::Release)
            }
            Err(LaneExit::Link(error)) => {
                let message = format!("hub request failed during the run: {error}");
                self.fail(lane, assignment.run_id, RunFailure::Error { message })
                    .await?;
                Err(LaneExit::Release(error))
            }
            other => other,
        }
    }

    /// Starts the backend and sees the run through. On any ending other than
    /// a kill, the backend is stopped.
    async fn attempt(
        &self,
        lane: &Arc<LaneState>,
        assignment: &RunAssignment,
    ) -> Result<AfterRun, LaneExit> {
        let run_id = assignment.run_id;
        lane.set_deferred(false);
        lane.set_progress_written(false);
        let Some(backend) = self.backend(assignment.backend) else {
            let message = format!("backend {} is not available here", assignment.backend);
            return self.fail(lane, run_id, RunFailure::Error { message }).await;
        };
        let working_dir = match self.working_dir(lane, assignment).await {
            Ok(dir) => dir,
            Err(message) => return self.fail(lane, run_id, RunFailure::Error { message }).await,
        };
        if assignment.lane_kind == LaneKind::Task {
            self.reach(AFTER_PREPARE_WORKSPACE)?;
        }
        lane.check()?;
        self.link.check_running(&lane.lease()).await?;
        let mut performed = self.perform(lane, assignment).await?;
        performed.push_str(&self.bring_in_base(assignment).await);
        lane.check()?;
        self.link.check_running(&lane.lease()).await?;

        let deadline = self.clock.now() + assignment.wall_time_limit;
        let request = self.request(lane, assignment, working_dir, performed);
        let mut run = match backend.run(request.clone()).await {
            Ok(run) => run,
            Err(BackendError::UsageLimit { resets_at }) => {
                self.usage_limit(lane, assignment.backend, resets_at)
                    .await?;
                return self.fail(lane, run_id, RunFailure::UsageLimit).await;
            }
            Err(error) => {
                let message = format!("the backend could not start the run: {error}");
                return self.fail(lane, run_id, RunFailure::Error { message }).await;
            }
        };
        self.reach(AFTER_BACKEND_START)?;

        let result = self
            .follow(
                lane,
                assignment,
                run.as_mut(),
                deadline,
                (backend.as_ref(), &request),
            )
            .await;
        if let Err(exit) = &result
            && !matches!(exit, LaneExit::Killed(_))
        {
            stop_backend(run.as_mut()).await;
        }
        result
    }

    /// Mirrors the run's events until it ends, then completes or fails it.
    /// `started` is the backend and the request that started the run, for
    /// the extra turn.
    async fn follow(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
        run: &mut dyn BackendRun,
        deadline: Timestamp,
        started: (&dyn AgentBackend, &RunRequest),
    ) -> Result<AfterRun, LaneExit> {
        let mut mirror = Mirror::new(assignment.run_id);
        let failure = match self
            .mirror_events(lane, assignment, run, &mut mirror, deadline)
            .await?
        {
            Ended::Terminal(BackendEvent::Finished { final_text, .. }) => {
                // A run waiting on you ends now; it is resumed with the answer.
                if assignment.lane_kind == LaneKind::Task && !lane.deferred() {
                    let progress = !lane.progress_written();
                    let quiet = self.left_unwoken(lane, assignment.run_id).await?;
                    // At most one extra turn: both asks share it.
                    let prompt = match (progress, quiet) {
                        (true, true) => Some(format!("{PROGRESS_PROMPT}\n\n{QUIET_END_PROMPT}")),
                        (true, false) => Some(PROGRESS_PROMPT.to_owned()),
                        (false, true) => Some(QUIET_END_PROMPT.to_owned()),
                        (false, false) => None,
                    };
                    if let Some(prompt) = prompt {
                        self.extra_turn(lane, assignment, started, prompt, &mut mirror, deadline)
                            .await?;
                    }
                }
                return self.commit(lane, assignment, final_text.as_deref()).await;
            }
            // The runner ends the run if the agent does not (Plan.md §9.4).
            Ended::Deferred => {
                stop_backend(run).await;
                mirror
                    .flush(self.link.as_ref(), &lane.lease(), Flush::Required)
                    .await?;
                return self.commit(lane, assignment, Some(STOPPED_WAITING)).await;
            }
            Ended::Terminal(BackendEvent::Failed { error }) => RunFailure::Error { message: error },
            Ended::Terminal(BackendEvent::UsageLimit { resets_at }) => {
                self.usage_limit(lane, assignment.backend, resets_at)
                    .await?;
                RunFailure::UsageLimit
            }
            Ended::Terminal(BackendEvent::SessionMissing) => RunFailure::SessionMissing,
            Ended::Terminal(event) => RunFailure::Error {
                message: format!("unexpected terminal backend event: {event:?}"),
            },
            Ended::Closed => RunFailure::Error {
                message: "the backend ended without a result".to_owned(),
            },
            Ended::WallTime => {
                stop_backend(run).await;
                mirror
                    .flush(self.link.as_ref(), &lane.lease(), Flush::Required)
                    .await?;
                let limit = assignment.wall_time_limit;
                RunFailure::LimitExceeded {
                    message: format!("the run hit its wall-time limit of {limit:?}"),
                }
            }
        };
        // Keep what the run did, even though it failed; the next run goes on
        // from it.
        if let Err(error) = self.checkpoint(assignment).await {
            warn!(run = %assignment.run_id, %error, "could not commit the failed run's work");
        }
        self.fail(lane, assignment.run_id, failure).await
    }

    /// The one extra turn of a task run that ends without its progress file
    /// written (docs/milestone-5.md §1 decision 7, [`PROGRESS_PROMPT`]) or
    /// with nothing to wake the task ([`QUIET_END_PROMPT`]): resumes the
    /// session once with `prompt`, mirroring the turn into the same journal
    /// under the run's own deadline. However the turn ends, the run then
    /// ends as it would have; only a stop, a kill or a lost lease ends it
    /// otherwise.
    async fn extra_turn(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
        (backend, started): (&dyn AgentBackend, &RunRequest),
        prompt: String,
        mirror: &mut Mirror,
        deadline: Timestamp,
    ) -> Result<(), LaneExit> {
        let (NativeSession::Start { id: assigned } | NativeSession::Resume { id: assigned }) =
            &assignment.native_session;
        let id = mirror.native_session().unwrap_or(assigned).to_owned();
        let request = RunRequest {
            prompt,
            session: NativeSession::Resume { id },
            ..started.clone()
        };
        let mut run = match backend.run(request).await {
            Ok(run) => run,
            Err(error) => {
                warn!(run = %assignment.run_id, %error, "could not start the extra turn");
                return Ok(());
            }
        };
        let ended = self
            .mirror_events(lane, assignment, run.as_mut(), mirror, deadline)
            .await;
        match ended {
            Ok(Ended::Terminal(BackendEvent::UsageLimit { resets_at })) => {
                self.usage_limit(lane, assignment.backend, resets_at).await
            }
            Ok(Ended::Terminal(_) | Ended::Closed) => Ok(()),
            Ok(Ended::Deferred | Ended::WallTime) => {
                stop_backend(run.as_mut()).await;
                Ok(mirror
                    .flush(self.link.as_ref(), &lane.lease(), Flush::Required)
                    .await?)
            }
            Err(exit) => {
                if !matches!(exit, LaneExit::Killed(_)) {
                    stop_backend(run.as_mut()).await;
                }
                Err(exit)
            }
        }
    }

    /// Races the event stream against the wall-time limit and the lane's
    /// stop signal, appending one journal entry per event. Every entry has
    /// been appended when this returns `Ended::Terminal` or `Ended::Closed`.
    async fn mirror_events(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
        run: &mut dyn BackendRun,
        mirror: &mut Mirror,
        deadline: Timestamp,
    ) -> Result<Ended, LaneExit> {
        let mut limit = self.clock.sleep_until(deadline);
        loop {
            let next = tokio::select! {
                biased;
                stop = lane.stopped() => Next::Stop(stop),
                () = &mut limit => Next::WallTime,
                event = run.next_event() => Next::Event(event),
            };
            let event = match next {
                Next::Stop(stop) => return Err(stop.into()),
                Next::WallTime => return Ok(Ended::WallTime),
                Next::Event(event) => event,
            };
            // A tool call made while producing this event may have been
            // killed or lost the lease; the backend only saw a tool error.
            lane.check()?;
            match self.link.check_running(&lane.lease()).await {
                Ok(()) | Err(LinkError::Unreachable) => {}
                Err(error) => return Err(error.into()),
            }
            let Some(event) = event else {
                mirror
                    .flush(self.link.as_ref(), &lane.lease(), Flush::Required)
                    .await?;
                return Ok(Ended::Closed);
            };
            mirror.record(&event, self.clock.now());
            let flush = if event.is_terminal() {
                Flush::Required
            } else {
                Flush::WhenReachable
            };
            mirror
                .flush(self.link.as_ref(), &lane.lease(), flush)
                .await?;
            if let BackendEvent::SessionStarted { native_session_id } = &event {
                self.native_session(lane, assignment, native_session_id)
                    .await?;
            }
            if event.is_terminal() {
                return Ok(Ended::Terminal(event));
            }
            if !assignment.dreaming
                && assignment.space_lead_ask.is_none()
                && !lane.deferred()
                && event == BackendEvent::TurnBoundary
            {
                self.fold_in(lane, assignment, run).await?;
            }
            if lane.deferred() && matches!(event, BackendEvent::ToolUse { .. }) {
                return Ok(Ended::Deferred);
            }
        }
    }

    async fn fold_in(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
        run: &mut dyn BackendRun,
    ) -> Result<(), LaneExit> {
        let lease = lane.lease();
        let pending = match self.link.pending(&lease).await {
            Ok(pending) => pending,
            Err(LinkError::Unreachable) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut events = Vec::new();
        let mut messages = Vec::new();
        for event in &pending {
            if event.kind == EventKind::UserMessage
                && event.trust == threadlane_core::model::Trust::User
                && event
                    .session_ref
                    .is_none_or(|id| id == assignment.session_id)
                && let Some(text) = event.payload["text"].as_str()
            {
                events.push(event.id);
                messages.push(text);
            }
        }
        if messages.is_empty() {
            return Ok(());
        }
        let input = messages.join("\n\n");
        let accepted = tokio::select! {
            biased;
            stop = lane.stopped() => return Err(stop.into()),
            result = run.fold_in(&input) => result,
        };
        match accepted {
            Ok(true) => {
                self.link
                    .folded_in(&lease, assignment.run_id, &events)
                    .await?;
                self.reach(threadlane_core::runtime::steps::runner::AFTER_FOLD_IN)?;
            }
            Ok(false) => {}
            Err(error) => warn!(%error, "backend could not accept user input"),
        }
        Ok(())
    }

    /// Whether the hub says the task would be left with nothing to wake it.
    /// A hub that cannot answer costs only the extra turn.
    async fn left_unwoken(&self, lane: &LaneState, run_id: RunId) -> Result<bool, LaneExit> {
        match self.link.left_unwoken(&lane.lease()).await {
            Ok(unwoken) => Ok(unwoken),
            Err(error @ (LinkError::Killed(_) | LinkError::StaleLease { .. })) => Err(error.into()),
            Err(error) => {
                warn!(run = %run_id, %error, "could not ask whether the task has anything to wake it");
                Ok(false)
            }
        }
    }

    /// Records the backend's session id if it is not the one assigned.
    async fn native_session(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
        native_session_id: &str,
    ) -> Result<(), LaneExit> {
        let (NativeSession::Start { id: assigned } | NativeSession::Resume { id: assigned }) =
            &assignment.native_session;
        if assigned == native_session_id {
            return Ok(());
        }
        let session_id = assignment.session_id;
        match self
            .link
            .native_session(&lane.lease(), session_id, native_session_id)
            .await
        {
            Ok(()) => Ok(()),
            // Losing it only costs a fresh session on the next run.
            Err(LinkError::Unreachable) => {
                warn!(session = %session_id, "hub unreachable; native session id not recorded");
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn commit(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
        final_text: Option<&str>,
    ) -> Result<AfterRun, LaneExit> {
        let run_id = assignment.run_id;
        self.reach(AFTER_BACKEND_RUN)?;
        if assignment.lane_kind == LaneKind::Task {
            if let Err(error) = self.checkpoint(assignment).await {
                let message = format!("could not commit the run's work: {error}");
                return self.fail(lane, run_id, RunFailure::Error { message }).await;
            }
            self.reach(AFTER_CHECKPOINT)?;
        }
        let outcome = self
            .link
            .complete(&lane.lease(), run_id, final_text)
            .await?;
        debug!(run = %run_id, %outcome, "run committed");
        self.reach(AFTER_COMMIT)?;
        Ok(AfterRun::Recheck)
    }

    /// Reports a failed run. A missing native session is retried at once in
    /// a fresh session (§8.6); any other failure releases the lane so the
    /// hub's backoff applies.
    async fn fail(
        &self,
        lane: &LaneState,
        run_id: RunId,
        failure: RunFailure,
    ) -> Result<AfterRun, LaneExit> {
        warn!(run = %run_id, ?failure, "run failed");
        match self.link.fail(&lane.lease(), run_id, &failure).await {
            Ok(()) if failure == RunFailure::SessionMissing => Ok(AfterRun::Recheck),
            Ok(()) => Ok(AfterRun::Release),
            Err(error) => Err(match LaneExit::from(error) {
                LaneExit::Link(error) => LaneExit::Release(error),
                other => other,
            }),
        }
    }

    /// Gives the lane back. Only a kill is an error: a lease that cannot be
    /// released expires on its own.
    async fn release(&self, lane: &LaneState) -> Result<(), Killed> {
        let lease = lane.lease();
        match self.link.release(&lease).await {
            Ok(()) => self.reach(AFTER_RELEASE),
            Err(LinkError::Killed(killed)) => Err(killed),
            Err(error) => {
                warn!(lane = %lease.lane_id, %error, "could not release the lane; its lease will expire");
                Ok(())
            }
        }
    }

    /// Performs the stored calls you approved, before the backend starts
    /// (Plan.md §9.4), and returns what to tell the session about them. The
    /// hub records the intent before and the outcome after, so a call takes
    /// effect at most once whatever is interrupted.
    async fn perform(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
    ) -> Result<String, LaneExit> {
        let mut told = String::new();
        for &decision in &assignment.perform {
            let begin = self
                .link
                .begin_perform(&lane.lease(), assignment.run_id, decision)
                .await?;
            let outcome = match begin {
                Perform::Recorded { outcome } => outcome,
                Perform::Repository { remote, action } => {
                    let outcome = match self.forge.apply(&remote, &action).await {
                        Ok(result) => {
                            json!({"performed": true, "summary": format!("Completed the repository action. {}", result.trim())})
                        }
                        Err(error) => json!({"performed": false, "error": error.to_string()}),
                    };
                    self.reach(perform::AFTER_EFFECT)?;
                    self.link
                        .finish_perform(&lane.lease(), assignment.run_id, decision, &outcome)
                        .await?;
                    outcome
                }
                Perform::Call {
                    tool,
                    arguments,
                    retry,
                } => {
                    let outcome = match self.call_forge(assignment, &tool, &arguments, retry).await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => json!({ "performed": false, "error": error }),
                    };
                    self.reach(perform::AFTER_EFFECT)?;
                    self.link
                        .finish_perform(&lane.lease(), assignment.run_id, decision, &outcome)
                        .await?;
                    outcome
                }
            };
            told.push_str(&describe(decision, &outcome));
        }
        Ok(told)
    }

    /// Makes the effect of one stored call, in the task's worktree of the
    /// repository it names, on the task's own branch. After an interrupted
    /// attempt (`retry`) a pull request already opened is found, not opened
    /// again.
    async fn call_forge(
        &self,
        assignment: &RunAssignment,
        tool: &str,
        arguments: &Json,
        retry: bool,
    ) -> Result<Json, String> {
        let task = assignment
            .task_id
            .ok_or("only a task's calls are performed")?;
        let name = arguments
            .get("repository")
            .and_then(Json::as_str)
            .ok_or("the call names no repository")?;
        let index = assignment
            .repositories
            .iter()
            .position(|repository| repository.name == name)
            .ok_or_else(|| format!("{name:?} is not one of the task's repositories"))?;
        let worktrees = self
            .workspaces
            .prepare(task, &assignment.repositories)
            .await
            .map_err(|error| error.to_string())?;
        let worktree = worktrees.get(index).ok_or("no worktree was set up")?;
        let branch = task_branch(task);
        let mut outcome = json!({ "performed": true, "repository": name, "branch": branch });
        match tool {
            PUBLISH_BRANCH => self
                .forge
                .push(worktree, &branch)
                .await
                .map_err(|error| error.to_string())?,
            OPEN_PULL_REQUEST => {
                let text = |field: &str| {
                    arguments
                        .get(field)
                        .and_then(Json::as_str)
                        .unwrap_or_default()
                };
                let found = if retry {
                    self.forge
                        .find_pull_request(worktree, &branch)
                        .await
                        .map_err(|error| error.to_string())?
                } else {
                    None
                };
                let url = match found {
                    Some(url) => url,
                    None => self
                        .forge
                        .open_draft_pull_request(
                            worktree,
                            &branch,
                            &assignment.repositories[index].default_branch,
                            text("title").trim(),
                            text("body"),
                        )
                        .await
                        .map_err(|error| error.to_string())?,
                };
                outcome["pull_request"] = json!(url);
            }
            other => return Err(format!("the runner does not perform {other}")),
        }
        Ok(outcome)
    }

    fn request(
        &self,
        lane: &Arc<LaneState>,
        assignment: &RunAssignment,
        working_dir: PathBuf,
        performed: String,
    ) -> RunRequest {
        let tools = LaneTools::new(
            Arc::clone(&self.link),
            Arc::clone(lane),
            assignment.run_id,
            assignment.tools.clone(),
            Arc::clone(&self.workspaces),
            assignment
                .task_id
                .map(|task| (task, assignment.repositories.clone())),
        );
        RunRequest {
            working_dir,
            prompt: format!("{}{performed}", assignment.prompt),
            appended_instructions: assignment.appended_instructions.clone(),
            model: assignment.model.clone(),
            session: assignment.native_session.clone(),
            tools: Some(Arc::new(tools)),
            deny_rules: assignment.deny_rules.clone(),
            remove_builtin_tools: assignment.remove_builtin_tools,
            wall_time_limit: assignment.wall_time_limit,
            max_turns: assignment.max_turns,
        }
    }

    /// Commits what a task run left uncommitted in its worktrees, on the
    /// task branch. Nothing to do for a lead, or a task without
    /// repositories.
    async fn checkpoint(&self, assignment: &RunAssignment) -> Result<(), String> {
        let Some(task) = assignment.task_id else {
            return Ok(());
        };
        let message = format!("Threadlane: work of {task}, run {}", assignment.run_id);
        self.workspaces
            .checkpoint(task, &assignment.repositories, &message)
            .await
            .map_err(|error| error.to_string())
    }

    /// Rebases a task's branches onto their default branches where that
    /// rebases cleanly (docs/merge-help.md decision 3), and returns what to
    /// tell the session about those it left as they were. A failure costs
    /// only the rebase: the run starts anyway.
    async fn bring_in_base(&self, assignment: &RunAssignment) -> String {
        let Some(task) = assignment.task_id else {
            return String::new();
        };
        if assignment.repositories.is_empty() {
            return String::new();
        }
        match self
            .workspaces
            .bring_in_base(task, &assignment.repositories)
            .await
        {
            Ok(checks) => checks.iter().map(conflict_note).collect(),
            Err(error) => {
                warn!(%task, %error, "could not bring the default branch into the task branch");
                String::new()
            }
        }
    }

    /// Reports the usage limit to the hub, which cools the account down.
    /// Only a kill or a lost lease stops the run from being failed after.
    async fn usage_limit(
        &self,
        lane: &LaneState,
        backend: BackendKind,
        resets_at: Option<Timestamp>,
    ) -> Result<(), LaneExit> {
        let label = self.account_label(backend).await;
        match self
            .link
            .account(&lane.lease(), backend, label.as_deref(), resets_at)
            .await
        {
            Ok(()) => Ok(()),
            Err(error @ (LinkError::Killed(_) | LinkError::StaleLease { .. })) => Err(error.into()),
            Err(error) => {
                warn!(%error, "could not report the usage limit");
                Ok(())
            }
        }
    }

    /// The task's worktree of its first repository, set up if needed, for a
    /// task that names repositories; otherwise `<lanes_dir>/<lane id>`,
    /// created if missing.
    async fn working_dir(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
    ) -> Result<PathBuf, String> {
        if let Some(task) = assignment.task_id
            && !assignment.repositories.is_empty()
        {
            let paths = self
                .workspaces
                .prepare(task, &assignment.repositories)
                .await
                .map_err(|error| format!("could not set up the task's worktrees: {error}"))?;
            return paths
                .into_iter()
                .next()
                .ok_or_else(|| "no worktree was set up".to_owned());
        }
        let dir = self.config.lanes_dir.join(lane.lease().lane_id.to_string());
        tokio::fs::create_dir_all(&dir).await.map_err(|error| {
            format!(
                "could not create the working directory {}: {error}",
                dir.display()
            )
        })?;
        Ok(dir)
    }

    fn backend(&self, kind: BackendKind) -> Option<&Arc<dyn AgentBackend>> {
        self.backends.iter().find(|backend| backend.kind() == kind)
    }

    /// The account the backend runs on: from `hello`'s probe, or probed now.
    async fn account_label(&self, kind: BackendKind) -> Option<String> {
        let known = self
            .probes
            .lock()
            .expect("probes lock poisoned")
            .iter()
            .find(|(probed, _)| *probed == kind)
            .map(|(_, probe)| probe.account_label.clone());
        match known {
            Some(label) => label,
            None => self.backend(kind)?.probe().await.ok()?.account_label,
        }
    }

    /// Reports load and renews the leases held. A lease the hub did not
    /// renew stops its lane.
    async fn heartbeat(&self) -> Result<(), RunnerError> {
        let lanes = self.running();
        let held: Vec<Lease> = lanes.iter().map(|lane| lane.lease()).collect();
        let report = self.report(saturating_u32(lanes.len()));
        let renewed = match self.link.heartbeat(&report, &held).await {
            Ok(renewed) => renewed,
            Err(LinkError::Killed(killed)) => return Err(killed.into()),
            Err(error) => {
                warn!(%error, "heartbeat failed");
                return Ok(());
            }
        };
        for (lane, lease) in lanes.iter().zip(&held) {
            let same_grant =
                |renewed: &&Lease| renewed.lane_id == lease.lane_id && renewed.epoch == lease.epoch;
            match renewed.iter().find(same_grant) {
                Some(renewed) => lane.renew(*renewed),
                None => {
                    warn!(lane = %lease.lane_id, "heartbeat did not renew the lease; stopping the lane");
                    lane.signal(Stop::LeaseLost);
                }
            }
        }
        Ok(())
    }

    fn report(&self, load: u32) -> MachineReport {
        let probes = self.probes.lock().expect("probes lock poisoned");
        MachineReport {
            capabilities: capabilities(self.config.max_parallel),
            backends: encode_backends(&probes),
            gh_login: self.gh_login.load(Ordering::Relaxed),
            load,
        }
    }

    fn running(&self) -> Vec<Arc<LaneState>> {
        let lanes = self.lanes.lock().expect("running lanes lock poisoned");
        lanes.lanes.values().cloned().collect()
    }

    fn load(&self) -> u32 {
        saturating_u32(
            self.lanes
                .lock()
                .expect("running lanes lock poisoned")
                .lanes
                .len(),
        )
    }

    fn reach(&self, step: Step) -> Result<(), Killed> {
        self.hook.reach(step)
    }
}

/// A lane counted in the load and reached by heartbeats while it is held.
#[derive(Debug)]
struct Registration {
    inner: Arc<Inner>,
    key: u64,
    lane: Arc<LaneState>,
}

impl Registration {
    fn new(inner: &Arc<Inner>, lease: Lease) -> Self {
        let lane = Arc::new(LaneState::new(lease));
        let mut running = inner.lanes.lock().expect("running lanes lock poisoned");
        let key = running.next_key;
        running.next_key += 1;
        running.lanes.insert(key, Arc::clone(&lane));
        drop(running);
        Self {
            inner: Arc::clone(inner),
            key,
            lane,
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Never panic in drop; a poisoned lock already means a panic elsewhere.
        if let Ok(mut running) = self.inner.lanes.lock() {
            running.lanes.remove(&self.key);
        }
    }
}

/// Forwards hub messages from a task of its own, so the claim loop never
/// cancels a pending `next_message` call.
#[derive(Debug)]
struct MessagePump {
    receiver: mpsc::UnboundedReceiver<HubMessage>,
    // Aborts the forwarding task when the pump is dropped.
    _task: JoinSet<()>,
}

impl MessagePump {
    fn start(inner: Arc<Inner>) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut task = JoinSet::new();
        task.spawn(async move {
            while let Some(message) = inner.link.next_message().await {
                if let HubMessage::Cancel { lane_id, epoch } = message {
                    let running = inner.lanes.lock().expect("running lanes lock poisoned");
                    for lane in running.lanes.values() {
                        let lease = lane.lease();
                        if lease.lane_id == lane_id && lease.epoch == epoch {
                            lane.signal(Stop::User);
                        }
                    }
                } else if sender.send(message).is_err() {
                    break;
                }
            }
            debug!("hub link closed its message stream");
        });
        Self {
            receiver,
            _task: task,
        }
    }

    /// The next message, or `None` once the link closed.
    async fn next(&mut self) -> Option<HubMessage> {
        self.receiver.recv().await
    }
}

async fn probe(backend: &dyn AgentBackend) -> Probe {
    match backend.probe().await {
        Ok(probe) => probe,
        Err(error) => {
            warn!(backend = %backend.kind(), %error, "backend probe failed; reporting it unusable");
            Probe {
                installed: !matches!(error, BackendError::NotInstalled),
                version: None,
                logged_in: false,
                account_label: None,
            }
        }
    }
}

/// What the final progress turn asks of a task's agent when its run is
/// about to end without its progress file written (docs/milestone-5.md §1
/// decision 7).
pub const PROGRESS_PROMPT: &str = "Before this run ends: you did not update your task's \
progress file in it. Write project/tasks/<your task id>.md now with memory_create or \
memory_edit: what is done, what is left, and what the next session on this task must know. \
Then end your turn.";

/// What the extra turn asks of a task's agent when its run is about to end
/// with nothing set to wake the task again ([`HubLink::left_unwoken`]).
/// When the progress file is also missing, it follows [`PROGRESS_PROMPT`]
/// in the same turn.
pub const QUIET_END_PROMPT: &str = "Before this run ends: you ended your turn without \
finish_task, and nothing is set to wake this task again. Anything you started in the background \
stops when the run ends, and nothing will tell you about it. Do one of these now: call \
finish_task if the task is done; call set_follow_up with what you are waiting for if you need to \
wait; or say in your final text what blocks you. Then end your turn.";

/// The rationale of a run stopped for going on after it asked you.
const STOPPED_WAITING: &str =
    "Stopped by the runner: the run went on working after a call of it waited on the user.";

/// What the session is told about a stored call the system performed.
fn describe(decision: DecisionId, outcome: &Json) -> String {
    let text = |field: &str| {
        outcome
            .get(field)
            .and_then(Json::as_str)
            .unwrap_or_default()
    };
    if outcome["unknown"] == Json::Bool(true) {
        return format!(
            "\nOutcome of {decision}: unknown after interruption. Check the repository before retrying.\n"
        );
    }
    if outcome.get("performed") != Some(&Json::Bool(true)) {
        return format!(
            "\nOutcome of {decision}: not performed. {}\n",
            text("error")
        );
    }
    // A memory change, which the hub performs, says what it did.
    if let Some(summary) = outcome.get("summary").and_then(Json::as_str) {
        return format!("\nOutcome of {decision}: the system performed it for you. {summary}\n");
    }
    let mut told = format!(
        "\nOutcome of {decision}: the system performed it for you. Branch {} of {} is pushed.",
        text("branch"),
        text("repository")
    );
    if let Some(url) = outcome.get("pull_request").and_then(Json::as_str) {
        told.push_str(&format!(" The draft pull request is {url}."));
    }
    told.push('\n');
    told
}

async fn stop_backend(run: &mut dyn BackendRun) {
    if let Err(error) = run.stop().await {
        warn!(%error, "could not stop the backend");
    }
}

fn capabilities(max_parallel: u32) -> Json {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
        "max_parallel": max_parallel,
    })
}

fn saturating_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// What a run's prompt says about a task branch left conflicting with its
/// base at the start of the run; nothing for one that merges cleanly.
fn conflict_note(check: &MergeCheck) -> String {
    if check.clean() {
        return String::new();
    }
    format!(
        "\n\nThe task branch in {repository} conflicts with {branch} at {base}, in: {files}. \
         It was not rebased. Rebase it onto {base} (already fetched as origin/{branch}) and \
         resolve the conflicts during this run.",
        repository = check.repository,
        branch = check.branch,
        base = check.base,
        files = check.conflicts.join(", "),
    )
}
