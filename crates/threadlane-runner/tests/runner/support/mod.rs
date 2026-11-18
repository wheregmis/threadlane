//! Fixtures shared by the runner tests.

mod link;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_core::backend::{
    AgentBackend, AskRequest, BackendError, BackendEvent, BackendRun, NativeSession, Probe,
    RunRequest,
};
use threadlane_core::ids::{EventId, LaneId, MachineId, ProjectId, RunId, SessionId};
use threadlane_core::link::{Begin, RunAssignment};
use threadlane_core::model::{BackendKind, Event, EventKind, EventStatus, LaneKind, Lease, Trust};
use threadlane_core::runtime::{Killed, Step, StepHook};
use threadlane_core::time::{Clock, Timestamp};
use threadlane_core::tools::{Tier, ToolDefinition};
use threadlane_runner::{GitWorkspaces, Runner, RunnerConfig, RunnerError};
use threadlane_testkit::fakes::FakeForge;
use threadlane_testkit::{CrashPlan, FakeClock, ScriptedBackend};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub use self::link::{Call, FakeLink};

pub const DEBOUNCE: Duration = Duration::from_secs(2);
pub const HEARTBEAT: Duration = Duration::from_secs(20);
pub const POLL: Duration = Duration::from_secs(30);
pub const WALL_TIME: Duration = Duration::from_secs(600);
/// The lane most tests work.
pub const LANE: i64 = 3;

pub fn lease(lane: i64) -> Lease {
    Lease {
        lane_id: LaneId(lane),
        machine_id: MachineId(1),
        epoch: 7,
        expires_at: Timestamp::from_millis(1_767_225_660_000),
    }
}

pub fn send_message_tool() -> ToolDefinition {
    ToolDefinition {
        name: "send_message".into(),
        description: "Send a message to the user".into(),
        input_schema: json!({ "type": "object" }),
        tier: Tier::Local,
        safe_to_repeat: false,
    }
}

/// A lead-lane assignment for run `run` that starts native session `native-<run>`.
pub fn assignment(run: i64) -> RunAssignment {
    RunAssignment {
        dreaming: false,
        run_id: RunId(run),
        trace_id: format!("event-{}", 1000 + run),
        session_id: SessionId(100 + run),
        project_id: ProjectId(1),
        lane_kind: LaneKind::Lead,
        task_id: None,
        repositories: Vec::new(),
        events: vec![EventId(1000 + run)],
        perform: Vec::new(),
        backend: BackendKind::Claude,
        model: Some("opus".into()),
        native_session: NativeSession::Start {
            id: format!("native-{run}"),
        },
        appended_instructions: "You are the lead.".into(),
        prompt: format!("prompt for run {run}"),
        tools: vec![send_message_tool()],
        remove_builtin_tools: true,
        deny_rules: vec!["rm -rf".into()],
        wall_time_limit: WALL_TIME,
        max_turns: Some(40),
    }
}

pub fn run(assignment: RunAssignment) -> Begin {
    Begin::Run(Box::new(assignment))
}

pub fn event(kind: EventKind) -> Event {
    Event {
        trace_id: String::new(),
        id: EventId(1),
        project_id: ProjectId(1),
        lane_id: LaneId(LANE),
        source: "client".into(),
        source_event_id: "m-1".into(),
        kind,
        payload: json!({ "text": "hi" }),
        trust: Trust::User,
        requested: true,
        session_ref: None,
        decision_ref: None,
        reply_to_ref: None,
        status: EventStatus::Pending,
        claimed_by_run: None,
        attempts: 0,
        received_at: Timestamp::from_millis(1_767_225_600_000),
        status_reason: None,
    }
}

/// Decodes the journal back into backend events.
pub fn journal_events(link: &FakeLink) -> Vec<BackendEvent> {
    link.journal()
        .into_iter()
        .map(|entry| serde_json::from_value(entry.entry).expect("journal entries are events"))
        .collect()
}

pub fn is_fail(call: &Call) -> bool {
    matches!(call, Call::Fail { .. })
}

pub fn is_release(call: &Call) -> bool {
    matches!(call, Call::Release { .. })
}

pub fn is_request_work(call: &Call) -> bool {
    matches!(call, Call::RequestWork)
}

/// Yields to spawned tasks until `condition` holds. Only real waiting is a
/// few tiny sleeps; nothing depends on elapsed time.
pub async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    for attempt in 0..2_000 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
        if attempt % 100 == 99 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    panic!("timed out waiting until {what}");
}

/// Lets every spawned task run until it waits on something.
pub async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
}

/// A [`ScriptedBackend`] that also counts [`BackendRun::stop`] calls.
#[derive(Debug, Clone)]
pub struct CountingBackend {
    pub scripted: ScriptedBackend,
    stops: Arc<AtomicUsize>,
}

impl CountingBackend {
    pub fn stops(&self) -> usize {
        self.stops.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AgentBackend for CountingBackend {
    fn kind(&self) -> BackendKind {
        self.scripted.kind()
    }

    async fn probe(&self) -> Result<Probe, BackendError> {
        self.scripted.probe().await
    }

    async fn run(&self, request: RunRequest) -> Result<Box<dyn BackendRun>, BackendError> {
        let inner = self.scripted.run(request).await?;
        Ok(Box::new(CountingRun {
            inner,
            stops: Arc::clone(&self.stops),
        }))
    }

    async fn ask(&self, request: AskRequest) -> Result<Json, BackendError> {
        self.scripted.ask(request).await
    }
}

#[derive(Debug)]
struct CountingRun {
    inner: Box<dyn BackendRun>,
    stops: Arc<AtomicUsize>,
}

#[async_trait]
impl BackendRun for CountingRun {
    async fn next_event(&mut self) -> Option<BackendEvent> {
        self.inner.next_event().await
    }

    async fn stop(&mut self) -> Result<(), BackendError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        self.inner.stop().await
    }
}

/// A hook that records, at each step, how many link calls had been made.
#[derive(Debug)]
pub struct CallCounter {
    link: Arc<FakeLink>,
    reached: Mutex<Vec<(Step, usize)>>,
}

impl CallCounter {
    pub fn new(link: Arc<FakeLink>) -> Self {
        Self {
            link,
            reached: Mutex::default(),
        }
    }

    pub fn reached(&self) -> Vec<(Step, usize)> {
        self.reached.lock().expect("counter lock poisoned").clone()
    }
}

impl StepHook for CallCounter {
    fn reach(&self, step: Step) -> Result<(), Killed> {
        let calls = self.link.calls().len();
        self.reached
            .lock()
            .expect("counter lock poisoned")
            .push((step, calls));
        Ok(())
    }
}

/// A runner on a fake clock, a fake link, and a scripted Claude backend.
#[derive(Debug)]
pub struct Harness {
    pub clock: FakeClock,
    pub link: Arc<FakeLink>,
    pub backend: CountingBackend,
    pub plan: CrashPlan,
    pub runner: Arc<Runner>,
    pub config: RunnerConfig,
    // Keeps the lanes directory alive for the test.
    _dir: tempfile::TempDir,
}

impl Harness {
    pub fn new() -> Self {
        Self::build(CrashPlan::new(), None, |_| {})
    }

    pub fn with_plan(plan: CrashPlan) -> Self {
        Self::build(plan, None, |_| {})
    }

    pub fn with_config(configure: impl FnOnce(&mut RunnerConfig)) -> Self {
        Self::build(CrashPlan::new(), None, configure)
    }

    /// A harness whose hook is `hook`, sharing `link` with it.
    pub fn with_hook(link: Arc<FakeLink>, hook: Arc<dyn StepHook>) -> Self {
        Self::build_on(link, CrashPlan::new(), Some(hook), |_| {})
    }

    fn build(
        plan: CrashPlan,
        hook: Option<Arc<dyn StepHook>>,
        configure: impl FnOnce(&mut RunnerConfig),
    ) -> Self {
        Self::build_on(Arc::default(), plan, hook, configure)
    }

    fn build_on(
        link: Arc<FakeLink>,
        plan: CrashPlan,
        hook: Option<Arc<dyn StepHook>>,
        configure: impl FnOnce(&mut RunnerConfig),
    ) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let clock = FakeClock::at_default_start();
        let backend = CountingBackend {
            scripted: ScriptedBackend::new(BackendKind::Claude, Arc::new(clock.clone())),
            stops: Arc::default(),
        };
        let mut config = RunnerConfig {
            lanes_dir: dir.path().join("lanes"),
            max_parallel: 2,
            debounce: DEBOUNCE,
            heartbeat: HEARTBEAT,
            poll: POLL,
        };
        configure(&mut config);
        let hook = hook.unwrap_or_else(|| Arc::new(plan.clone()));
        let runner = Runner::new(
            Arc::clone(&link) as Arc<_>,
            vec![Arc::new(backend.clone())],
            Arc::new(GitWorkspaces::new(dir.path().join("workspaces"))),
            Arc::new(FakeForge::default()),
            Arc::new(clock.clone()),
            hook,
            config.clone(),
        );
        Self {
            clock,
            link,
            backend,
            plan,
            runner: Arc::new(runner),
            config,
            _dir: dir,
        }
    }

    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    pub fn calls(&self) -> Vec<Call> {
        self.link.calls()
    }

    /// Runs `work_once` on its own task, so the test can move the clock.
    pub fn spawn_work_once(&self) -> JoinHandle<Result<bool, RunnerError>> {
        let runner = Arc::clone(&self.runner);
        tokio::spawn(async move { runner.work_once().await })
    }

    /// Starts `run` on its own task. Send on the returned channel to shut it down.
    pub fn spawn_run(&self) -> (oneshot::Sender<()>, JoinHandle<Result<(), RunnerError>>) {
        let (shutdown, signal) = oneshot::channel::<()>();
        let runner = Arc::clone(&self.runner);
        let task = tokio::spawn(async move {
            runner
                .run(async move {
                    // A dropped sender also shuts the runner down.
                    let _ = signal.await;
                })
                .await
        });
        (shutdown, task)
    }
}
