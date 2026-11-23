//! [`Runner`]: the lane cycle of docs/milestone-1.md §3 and the claim loop.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::backend::{
    AgentBackend, BackendError, BackendEvent, BackendRun, NativeSession, Probe, RunRequest,
    encode_backends,
};
use threadlane_core::forge::{
    ATTACH_IMAGE, ApprovedBranch, EXPECTED_HEAD, FAST_FORWARD_PULL_REQUEST, Forge, IMAGE_BLOB,
    MAX_IMAGE_BYTES, ONTO_TASK, OPEN_PULL_REQUEST, PUBLISH_BRANCH, PullRequestImage,
    PullRequestUpdate, image_extension,
};
use threadlane_core::ids::{DecisionId, RunId};
use threadlane_core::link::{
    Begin, HubLink, HubMessage, LinkError, PUBLISH_REVISION, Perform, RunAssignment, RunFailure,
};
use threadlane_core::model::{BackendKind, EventKind, LaneKind, Lease, MachineReport};
use threadlane_core::runtime::steps::perform;
use threadlane_core::runtime::steps::runner::{
    AFTER_BACKEND_RUN, AFTER_BACKEND_START, AFTER_CHECKPOINT, AFTER_CLAIM, AFTER_COMMIT,
    AFTER_DEBOUNCE, AFTER_PREPARE_WORKSPACE, AFTER_RECHECK, AFTER_RELEASE, AFTER_START_RUN,
};
use threadlane_core::runtime::{Killed, Step, StepHook};
use threadlane_core::time::{Clock, Timestamp};
use threadlane_core::workspace::{
    MergeCheck, ReviewCheckout, Workspaces, task_branch, task_ref, valid_commit_id,
};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinSet};
use tracing::{Instrument, debug, info, info_span, warn};

use crate::config::RunnerConfig;
use crate::error::RunnerError;
use crate::lane::{LaneExit, LaneState, Stop};
use crate::mirror::{Flush, Mirror};
use crate::tools::LaneTools;
use crate::workspace::branch_revision;

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
        let mut set = JoinSet::new();
        for (idx, backend) in self.inner.backends.iter().cloned().enumerate() {
            set.spawn(async move { (idx, backend.kind(), probe(backend.as_ref()).await) });
        }
        let collect_probes = async move {
            let mut indexed = Vec::with_capacity(set.len());
            while let Some(Ok(item)) = set.join_next().await {
                indexed.push(item);
            }
            indexed.sort_by_key(|(idx, _, _)| *idx);
            indexed
                .into_iter()
                .map(|(_, kind, p)| (kind, p))
                .collect::<Vec<_>>()
        };
        let (probes, gh_login) = tokio::join!(collect_probes, self.inner.forge.logged_in());
        *self.inner.probes.lock().expect("probes lock poisoned") = probes;
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
        self.claim(&mut lanes).await?;

        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                Some(joined) = lanes.join_next() => {
                    lane_finished(joined)?;
                    self.claim(&mut lanes).await?;
                }
                Some(message) = messages.next() => {
                    if message == HubMessage::Nudge {
                        self.claim(&mut lanes).await?;
                    } else {
                        debug!(?message, "ignoring a hub message this runner does not handle yet");
                    }
                }
                () = &mut heartbeat => {
                    self.inner.heartbeat().await?;
                    heartbeat = clock.sleep(config.heartbeat);
                }
                () = &mut poll => {
                    self.claim(&mut lanes).await?;
                    poll = clock.sleep(config.poll);
                }
            }
        }
        self.shut_down(lanes).await
    }

    /// Asks for lanes until the hub has none or `max_parallel` are running.
    async fn claim(&self, lanes: &mut JoinSet<Result<(), Killed>>) -> Result<(), RunnerError> {
        let limit = match self.inner.link.max_parallel().await {
            Ok(Some(limit)) => limit,
            Ok(None) => self.inner.config.max_parallel,
            Err(LinkError::Killed(killed)) => return Err(killed.into()),
            Err(_) => self.inner.config.max_parallel,
        };
        let max_parallel = usize::try_from(limit).unwrap_or(usize::MAX);
        while lanes.len() < max_parallel {
            match self.inner.link.request_work().await {
                Ok(Some(lease)) => {
                    let registration = Registration::new(&self.inner, lease);
                    let inner = Arc::clone(&self.inner);
                    lanes.spawn(async move { inner.work_lane(&registration.lane).await });
                }
                Ok(None) => break,
                Err(LinkError::Killed(killed)) => return Err(killed.into()),
                Err(error) => {
                    warn!(%error, "could not ask the hub for work");
                    break;
                }
            }
        }
        Ok(())
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

const JOURNAL_BATCH: usize = 16;
const JOURNAL_FLUSH_DELAY: Duration = Duration::from_millis(50);

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
                    if let Err(error) = self.checkpoint(assignment, false).await {
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
        lane.set_deferred(None);
        lane.set_progress_written(false);
        *lane.submitted.lock().expect("submission lock poisoned") = None;
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
        if assignment.review.is_some() && !assignment.perform.is_empty() {
            let message = "review runs cannot perform approved repository actions".to_owned();
            return self.fail(lane, run_id, RunFailure::Error { message }).await;
        }
        let mut performed = self.perform(lane, assignment).await?;
        performed.push_str(&self.bring_in_base(assignment).await);
        performed.push_str(&self.earlier_work(lane, assignment, &working_dir).await);
        performed.push_str(&self.review_base(assignment, &working_dir).await);
        let noted = assignment.task_id.filter(|_| assignment.review.is_none());
        if let Some(task) = noted {
            let notes = self
                .workspaces
                .checkpoint_notes(task, &assignment.repositories);
            performed.push_str(&notes.await);
        }
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
        // Told once, and only to a run that started: a failed start keeps them.
        if let Some(task) = noted {
            self.workspaces
                .clear_checkpoint_notes(task, &assignment.repositories)
                .await;
        }
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
        let mut ended = self
            .mirror_events(lane, assignment, run, &mut mirror, deadline)
            .await?;
        // The backend refused the agent's model: go once more on the
        // backend's default model instead of failing the task.
        let default_model = RunRequest {
            model: None,
            ..started.1.clone()
        };
        let mut fallback = None;
        if let Ended::Terminal(BackendEvent::Failed { error }) = &ended
            && started.1.model.is_some()
            && threadlane_core::backend::rejects_model(error)
            && let Ok(again) = started.0.run(default_model.clone()).await
        {
            warn!(run = %assignment.run_id, %error, "model refused; running on the backend's default model");
            fallback = Some(again);
        }
        let (run, started) = match fallback.as_mut() {
            Some(again) => {
                let again = again.as_mut();
                let followed = self
                    .mirror_events(lane, assignment, again, &mut mirror, deadline)
                    .await;
                if followed.is_err() {
                    stop_backend(again).await;
                }
                ended = followed?;
                (again, (started.0, &default_model))
            }
            None => (run, started),
        };
        let failure = match ended {
            Ended::Terminal(BackendEvent::Finished { final_text, .. }) => {
                // A run waiting on you ends now; it is resumed with the answer.
                if assignment.lane_kind == LaneKind::Task
                    && assignment.review.is_none()
                    && lane.deferred().is_none()
                {
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
                let final_text = final_text.as_deref().filter(|_| {
                    lane.deferred() != Some(threadlane_core::tools::ToolError::AuthorizedAndQueued)
                });
                return self.commit(lane, assignment, final_text).await;
            }
            // The runner ends the run if the agent does not (Plan.md §9.4).
            Ended::Deferred => {
                stop_backend(run).await;
                mirror
                    .flush(self.link.as_ref(), &lane.lease(), Flush::Required)
                    .await?;
                let rationale = (lane.deferred()
                    == Some(threadlane_core::tools::ToolError::WaitingOnYou))
                .then_some(STOPPED_WAITING);
                return self.commit(lane, assignment, rationale).await;
            }
            Ended::Terminal(BackendEvent::Failed { error })
                if threadlane_core::backend::rejects_model(&error) =>
            {
                RunFailure::Rejected { message: error }
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
        if let Err(error) = self.checkpoint(assignment, false).await {
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
            // The resumed session already has the run's skills loaded.
            skills: Vec::new(),
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
    /// stop signal, appending journal entries in bounded batches. Every entry has
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
        let flush_timer = tokio::time::sleep(JOURNAL_FLUSH_DELAY);
        tokio::pin!(flush_timer);
        let mut buffered = 0;
        loop {
            // The timer may flush while the backend is producing its next event.
            // Keep that read alive across a flush: it can include a tool call.
            let next = {
                let event = run.next_event();
                tokio::pin!(event);
                loop {
                    tokio::select! {
                        biased;
                        stop = lane.stopped() => break Next::Stop(stop),
                        () = &mut limit => break Next::WallTime,
                        () = &mut flush_timer, if buffered > 0 => {
                            mirror.flush(self.link.as_ref(), &lane.lease(), Flush::WhenReachable).await?;
                            buffered = 0;
                        }
                        event = &mut event => break Next::Event(event),
                    }
                }
            };
            let event = match next {
                Next::Stop(stop) => {
                    if matches!(stop, Stop::User | Stop::Shutdown) {
                        match mirror
                            .flush(self.link.as_ref(), &lane.lease(), Flush::WhenReachable)
                            .await
                        {
                            Ok(()) | Err(LinkError::Stopped) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    return Err(stop.into());
                }
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
            if buffered == 0 {
                flush_timer
                    .as_mut()
                    .reset(tokio::time::Instant::now() + JOURNAL_FLUSH_DELAY);
            }
            buffered += 1;
            let boundary = matches!(
                event,
                BackendEvent::SessionStarted { .. }
                    | BackendEvent::TurnBoundary
                    | BackendEvent::ToolUse { .. }
            );
            if event.is_terminal() || boundary || buffered >= JOURNAL_BATCH {
                let flush = if event.is_terminal() {
                    Flush::Required
                } else {
                    Flush::WhenReachable
                };
                mirror
                    .flush(self.link.as_ref(), &lane.lease(), flush)
                    .await?;
                buffered = 0;
            }
            if let BackendEvent::SessionStarted { native_session_id } = &event {
                self.native_session(lane, assignment, native_session_id)
                    .await?;
            }
            if event.is_terminal() {
                return Ok(Ended::Terminal(event));
            }
            if !assignment.dreaming
                && assignment.review.is_none()
                && assignment.space_lead_ask.is_none()
                && lane.deferred().is_none()
                && event == BackendEvent::TurnBoundary
            {
                self.fold_in(lane, assignment, run).await?;
            }
            // Further hub calls of the turn, as in a parallel batch, are the
            // hub's to answer; any other tool means the agent went on working.
            if lane.deferred().is_some()
                && let BackendEvent::ToolUse { name, .. } = &event
                && !assignment.tools.iter().any(|tool| &tool.name == name)
            {
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
                // The streaming follow-up is text-only. Preserve order and let
                // the next run deliver this image and any later messages.
                if event.payload["images"]
                    .as_array()
                    .is_some_and(|images| !images.is_empty())
                {
                    break;
                }
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
            if let Err(error) = self.checkpoint(assignment, true).await {
                let message = format!("could not commit the run's work: {error}");
                return self.fail(lane, run_id, RunFailure::Error { message }).await;
            }
            let submitted = lane
                .submitted
                .lock()
                .expect("submission lock poisoned")
                .take();
            if let (Some(task), Some(revisions)) = (assignment.task_id, submitted)
                && let Err(error) = self
                    .workspaces
                    .verify_submission(task, &assignment.repositories, &revisions)
                    .await
            {
                return self
                    .fail(
                        lane,
                        run_id,
                        RunFailure::Error {
                            message: error.to_string(),
                        },
                    )
                    .await;
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
        if tool == ATTACH_IMAGE {
            return self.attach_image(assignment, task, index, arguments).await;
        }
        let revision = arguments
            .get(PUBLISH_REVISION)
            .and_then(Json::as_str)
            .filter(|revision| valid_commit_id(revision))
            .ok_or("the publish approval has no commit SHA. Request approval again.")?;
        let worktrees = self
            .workspaces
            .prepare(task, &assignment.repositories)
            .await
            .map_err(|error| error.to_string())?;
        let worktree = worktrees.get(index).ok_or("no worktree was set up")?;
        let branch = task_branch(task);
        let current = branch_revision(worktree, &branch)
            .await
            .map_err(|error| error.to_string())?;
        if current != revision {
            return Err(format!(
                "branch {branch} moved from approved commit {revision} to {current}; request approval again"
            ));
        }
        if tool == FAST_FORWARD_PULL_REQUEST {
            let repository = &assignment.repositories[index];
            let remote = threadlane_core::github::normalize_remote(&repository.remote_url)
                .ok_or("the registered remote is not a GitHub repository")?;
            let number = arguments["number"]
                .as_u64()
                .and_then(std::num::NonZeroU64::new)
                .ok_or("the approved call has no valid PR number")?;
            let expected_head = arguments[EXPECTED_HEAD]
                .as_str()
                .filter(|head| valid_commit_id(head))
                .ok_or("the approved call has no expected PR head SHA")?;
            let update = PullRequestUpdate {
                number,
                expected_head,
                revision,
            };
            let head = self
                .forge
                .fast_forward_pull_request(worktree, repository, update)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(json!({
                "performed": true,
                "repository": name,
                "branch": head,
                "pull_request": format!("https://{remote}/pull/{number}"),
                "revision": revision,
            }));
        }
        // The hub checked the task before asking; the push itself is not
        // forced, so a branch that does not contain the target's commits fails.
        let target = publish_target(task, tool, arguments)?;
        let approved = ApprovedBranch {
            name: &target,
            revision,
        };
        let mut outcome = json!({ "performed": true, "repository": name, "branch": target });
        match tool {
            PUBLISH_BRANCH => {
                self.forge
                    .push(worktree, approved)
                    .await
                    .map_err(|error| error.to_string())?;
                // Names the pull request an onto_task push updated, so a
                // review of this task is posted there.
                if target != branch
                    && let Ok(Some(url)) = self.forge.find_pull_request(worktree, &target).await
                {
                    outcome["pull_request"] = json!(url);
                }
            }
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
                            approved,
                            match text("base") {
                                "" => &assignment.repositories[index].default_branch,
                                base => base,
                            },
                            text("title").trim(),
                            text("body"),
                            &format!("{}, {task}", assignment.agent_name),
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

    /// Attaches the approved image to the pull request, if its bytes are
    /// still the ones approved.
    async fn attach_image(
        &self,
        assignment: &RunAssignment,
        task: threadlane_core::ids::TaskId,
        index: usize,
        arguments: &Json,
    ) -> Result<Json, String> {
        let worktrees = self
            .workspaces
            .prepare(task, &assignment.repositories)
            .await
            .map_err(|error| error.to_string())?;
        let worktree = worktrees.get(index).ok_or("no worktree was set up")?;
        let (image, extension) = image_file(worktree, arguments).await?;
        let number = arguments["number"]
            .as_u64()
            .and_then(std::num::NonZeroU64::new)
            .ok_or("the approved call has no valid PR number")?;
        let blob = arguments[IMAGE_BLOB]
            .as_str()
            .filter(|blob| valid_commit_id(blob))
            .ok_or("the approval has no image blob ID. Request approval again.")?;
        let summary = self
            .forge
            .attach_image(
                worktree,
                PullRequestImage {
                    remote: &assignment.repositories[index].remote_url,
                    number,
                    image: &image,
                    extension,
                    blob,
                    caption: arguments["caption"]
                        .as_str()
                        .map_or("screenshot", str::trim),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(json!({ "performed": true, "summary": summary }))
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
                .filter(|_| assignment.review.is_none())
                .map(|task| (task, assignment.repositories.clone())),
        );
        RunRequest {
            working_dir,
            prompt: format!("{}{performed}", assignment.prompt),
            images: assignment.images.clone(),
            appended_instructions: assignment.appended_instructions.clone(),
            skills: assignment.skills.clone(),
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
    /// A run that did not finish keeps its untracked files out of the
    /// commit (#691): they may be scratch it meant to delete.
    async fn checkpoint(&self, assignment: &RunAssignment, finished: bool) -> Result<(), String> {
        if assignment.review.is_some() {
            return Ok(());
        }
        let Some(task) = assignment.task_id else {
            return Ok(());
        };
        let message = format!("Threadlane: work of {task}, run {}", assignment.run_id);
        let repositories = &assignment.repositories;
        let saved = if finished {
            self.workspaces
                .checkpoint(task, repositories, &message)
                .await
        } else {
            self.workspaces
                .checkpoint_tracked(task, repositories, &message)
                .await
        };
        saved.map_err(|error| error.to_string())
    }

    /// Brings the default branches into a task's branches where that is
    /// clean (a merge once a branch is shared, #730), and returns what to
    /// tell the session about those it left as they were. A failure costs
    /// only that: the run starts anyway.
    async fn bring_in_base(&self, assignment: &RunAssignment) -> String {
        if assignment.review.is_some() {
            return String::new();
        }
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

    /// For a PR review, the branch the PR merges into as the forge reports it,
    /// fetched into the checkout, so the reviewer diffs against the real base
    /// rather than the registration's tracked branch (#715).
    async fn review_base(&self, assignment: &RunAssignment, checkout: &Path) -> String {
        let Some(review) = &assignment.pr_review else {
            return String::new();
        };
        let Some(repository) = assignment
            .repositories
            .iter()
            .find(|repo| repo.id == review.revision.repository)
        else {
            return String::new();
        };
        let number = review.number;
        let base = match std::num::NonZeroU64::new(number) {
            Some(n) => {
                self.forge
                    .pull_request_base(&repository.remote_url, n)
                    .await
            }
            None => return String::new(),
        };
        let base = match base {
            Ok(base) => base,
            Err(error) => {
                return format!(
                    "\n\nCould not read PR #{number}'s base branch ({error}). Find it with \
                     `gh pr view {number} --json baseRefName` and diff against it, not against \
                     {}'s tracked branch {}. Name the base you compared against in your result.\n",
                    repository.name, repository.default_branch
                );
            }
        };
        let refspec = format!("+refs/heads/{base}:refs/remotes/origin/{base}");
        let mut note = match crate::workspace::git(
            Some(checkout),
            ["fetch", "--quiet", "origin", &refspec],
        )
        .await
        {
            Ok(_) => format!(
                "\n\nPR #{number} merges into `{base}`. Review its diff against that base: \
                 `git diff origin/{base}...HEAD`. Name `{base}` as the base you compared against in your result."
            ),
            Err(error) => {
                warn!(%error, %base, "could not fetch the PR's base branch");
                // origin/<base> may be missing or stale, so don't point the reviewer at it.
                format!(
                    "\n\nPR #{number} merges into `{base}`, but fetching that branch failed ({error}), \
                     so `origin/{base}` may be missing or stale. Fetch the base yourself \
                     (`git fetch origin {base}`, or find it with `gh pr view {number} --json baseRefName`) \
                     before diffing against it. Name the base you compared against in your result."
                )
            }
        };
        if base != repository.default_branch {
            note.push_str(&format!(
                " The registration {} tracks `{}`, not this PR's base: do not diff against it.",
                repository.name, repository.default_branch
            ));
        }
        note.push('\n');
        note
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

    /// Where a task that worked without a repository left its work, once a
    /// repository is attached and its runs start in a worktree instead.
    async fn earlier_work(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
        working_dir: &Path,
    ) -> String {
        if assignment.task_id.is_none() || assignment.review.is_some() {
            return String::new();
        }
        let earlier = self.config.lanes_dir.join(lane.lease().lane_id.to_string());
        if earlier == working_dir || !has_work(&earlier).await {
            return String::new();
        }
        format!(
            "\nYour work from before a repository was attached is in {}. \
             Your working directory is now {}: bring any completed work you still need into it.\n",
            earlier.display(),
            working_dir.display()
        )
    }

    /// The task's worktree of its first repository, set up if needed, for a
    /// task that names repositories; otherwise `<lanes_dir>/<lane id>`,
    /// created if missing.
    async fn working_dir(
        &self,
        lane: &LaneState,
        assignment: &RunAssignment,
    ) -> Result<PathBuf, String> {
        if let Some(task) = assignment.task_id {
            let paths = match &assignment.review {
                Some(review) => {
                    self.workspaces
                        .prepare_review(
                            task,
                            ReviewCheckout::Submission(review.submission),
                            &assignment.repositories,
                            &review.revisions,
                        )
                        .await
                }
                None if assignment.repositories.is_empty() => Ok(Vec::new()),
                None => {
                    self.workspaces
                        .prepare(task, &assignment.repositories)
                        .await
                }
            }
            .map_err(|error| format!("could not set up the task's worktrees: {error}"))?;
            // Task checkpoints and approved actions still use the managed task branches.
            // Give the Reviewer its own checkout, even when the target is not the first repository.
            if let Some(review) = &assignment.pr_review {
                let repository = assignment
                    .repositories
                    .iter()
                    .find(|repo| repo.id == review.revision.repository)
                    .ok_or_else(|| "the assigned PR repository is missing".to_owned())?;
                return self
                    .workspaces
                    .prepare_review(
                        task,
                        ReviewCheckout::PullRequest(review.number),
                        std::slice::from_ref(repository),
                        std::slice::from_ref(&review.revision),
                    )
                    .await
                    .map_err(|error| format!("could not prepare the PR review checkout: {error}"))?
                    .into_iter()
                    .next()
                    .ok_or_else(|| "no PR review checkout was set up".to_owned());
            }
            if !assignment.repositories.is_empty() {
                return paths
                    .into_iter()
                    .next()
                    .ok_or_else(|| "no worktree was set up".to_owned());
            }
        }
        let dir = self.config.lanes_dir.join(lane.lease().lane_id.to_string());
        let dir = match &assignment.review {
            Some(review) => dir.join("reviews").join(review.submission.to_string()),
            None => dir,
        };
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
         It was left as it was. Merge {base} into it (git merge origin/{branch}, already \
         fetched; do not rebase) and resolve the conflicts during this run.",
        repository = check.repository,
        branch = check.branch,
        base = check.base,
        files = check.conflicts.join(", "),
    )
}

/// The branch a stored publish call pushes to: the task's own, or for
/// `publish_branch` with `onto_task` that earlier task's.
pub(crate) fn publish_target(
    task: threadlane_core::ids::TaskId,
    tool: &str,
    arguments: &Json,
) -> Result<String, String> {
    match arguments.get(ONTO_TASK).filter(|onto| !onto.is_null()) {
        Some(onto) if tool == PUBLISH_BRANCH => task_ref(onto)
            .map(task_branch)
            .ok_or_else(|| "onto_task is not a task id".to_owned()),
        _ => Ok(task_branch(task)),
    }
}

/// The image an `attach_image` call names, given absolute or relative to
/// `worktree`, with its file extension: a PNG, JPEG, GIF or WebP file of at
/// most [`MAX_IMAGE_BYTES`]. The bytes are checked, not the file name.
pub(crate) async fn image_file(
    worktree: &Path,
    arguments: &Json,
) -> Result<(PathBuf, &'static str), String> {
    use tokio::io::AsyncReadExt as _;
    let path = arguments
        .get("path")
        .and_then(Json::as_str)
        .filter(|path| !path.is_empty())
        .ok_or("path must name an image file")?;
    let path = worktree.join(path);
    let unreadable = |error: std::io::Error| format!("cannot read {}: {error}", path.display());
    let metadata = tokio::fs::metadata(&path).await.map_err(unreadable)?;
    if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "{} must be a file of at most {} MiB",
            path.display(),
            MAX_IMAGE_BYTES >> 20
        ));
    }
    let mut head = [0; 12];
    let read = tokio::fs::File::open(&path)
        .await
        .map_err(unreadable)?
        .read(&mut head)
        .await
        .map_err(unreadable)?;
    let extension = image_extension(&head[..read])
        .ok_or_else(|| format!("{} is not a PNG, JPEG, GIF or WebP image", path.display()))?;
    Ok((path, extension))
}

/// Whether `dir` holds anything besides review checkouts, which share it.
async fn has_work(dir: &Path) -> bool {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return false;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_name() != "reviews" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod publish_target_tests {
    use super::*;

    #[test]
    fn only_publish_branch_with_onto_task_leaves_the_tasks_own_branch() {
        let own = threadlane_core::ids::TaskId(92);
        let onto = json!({ "repository": "app", "onto_task": "T-0087" });
        assert_eq!(
            publish_target(own, PUBLISH_BRANCH, &onto).unwrap(),
            "threadlane/T-0087"
        );
        assert_eq!(
            publish_target(own, OPEN_PULL_REQUEST, &onto).unwrap(),
            "threadlane/T-0092"
        );
        assert_eq!(
            publish_target(own, PUBLISH_BRANCH, &json!({ "repository": "app" })).unwrap(),
            "threadlane/T-0092"
        );
        assert!(publish_target(own, PUBLISH_BRANCH, &json!({ "onto_task": "main" })).is_err());
    }
}
