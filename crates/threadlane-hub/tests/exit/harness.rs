//! The real hub and the real runner together: one in-memory database, one
//! fake clock, one scripted backend, and one `CrashPlan` as the step hook of
//! both (docs/milestone-1.md §10).
//!
//! A [`World`] is what outlives a process: the stores, the clock, and the
//! machine's disk (the lanes directory and the backend's native sessions). A
//! [`Sim`] is one process on it: a `HubCore` and a `Runner`. Restarting means
//! building a fresh `Sim` on the same `World`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value as Json, json};
use tempfile::TempDir;
use threadlane_core::backend::{AgentBackend, Probe, encode_backends};
use threadlane_core::client::{FeedItem, HubApi, UserMessage};
use threadlane_core::ids::{EventId, MachineId, OutboxId, RunId};
use threadlane_core::link::HubLink;
use threadlane_core::model::{
    BackendKind, Event, JournalEntry, Lane, MachineReport, MachineTrust, NewMachine, OutboxMessage,
    Run, RunStatus,
};
use threadlane_core::runtime::Step;
use threadlane_core::runtime::steps::runner::{AFTER_CLAIM, AFTER_DEBOUNCE};
use threadlane_core::store::{InboxStore, MachineStore, OrgStore, OutboxStore, SessionStore};
use threadlane_hub::{HubCore, HubError, HubSettings, SEND_MESSAGE};
use threadlane_runner::{Runner, RunnerConfig, RunnerError};
use threadlane_store::{Bootstrap, HubDb};
use threadlane_testkit::{CrashPlan, FakeClock, Script, ScriptedBackend};
use tokio::task::JoinHandle;

/// One past the default lease, to let a lease expire.
pub const PAST_LEASE: Duration = Duration::from_secs(61);
/// The call id the reply script's `send_message` call carries.
pub const CALL_ID: &str = "toolu_1";
/// What the reply script sends.
pub const REPLY: &str = "Here is your answer.";
/// What the reply script ends with: the run's rationale.
pub const RATIONALE: &str = "Answered the question.";

/// How many times a test yields to other tasks before it gives up waiting.
/// Only yields: no test waits on real time.
const YIELDS: u32 = 200_000;

/// A lead that thinks aloud, replies once through `send_message`, and finishes.
pub fn reply_script() -> Script {
    reply_with(CALL_ID, REPLY)
}

/// The reply script with another call id and text.
pub fn reply_with(call_id: &str, text: &str) -> Script {
    reply_from(Script::new(), call_id, text)
}

/// The reply script, after waiting `wait` on the clock.
pub fn reply_after(wait: Duration) -> Script {
    reply_from(Script::new().wait(wait), CALL_ID, REPLY)
}

fn reply_from(script: Script, call_id: &str, text: &str) -> Script {
    script
        .text("Let me answer.")
        .call(call_id, SEND_MESSAGE, json!({ "text": text }))
        .finish(RATIONALE)
}

/// How many times `plan` saw `step`.
pub fn count(plan: &CrashPlan, step: Step) -> usize {
    plan.reached().iter().filter(|seen| **seen == step).count()
}

/// Yields until `check` holds. Panics, naming `what`, if it never does.
pub async fn until<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..YIELDS {
        if check().await {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("never happened: {what}");
}

/// Waits for a spawned task to finish, yielding only.
pub async fn join<T>(what: &str, task: JoinHandle<T>) -> T {
    until(what, || std::future::ready(task.is_finished())).await;
    task.await.unwrap_or_else(|error| panic!("{what}: {error}"))
}

/// A machine report with Claude installed and logged in.
pub fn usable_report() -> MachineReport {
    MachineReport {
        capabilities: json!({}),
        backends: encode_backends(&[(
            BackendKind::Claude,
            Probe {
                installed: true,
                version: Some("scripted".into()),
                logged_in: true,
                account_label: None,
            },
        )]),
        gh_login: false,
        load: 0,
    }
}

/// What survives a restart.
#[derive(Debug, Clone)]
pub struct World {
    pub clock: FakeClock,
    pub db: HubDb,
    /// Its native sessions live on the machine's disk, so they survive too.
    pub backend: ScriptedBackend,
    pub lanes: Arc<TempDir>,
    pub settings: HubSettings,
    pub runner_config: RunnerConfig,
}

impl World {
    pub async fn new() -> Self {
        Self::with_settings(HubSettings::default()).await
    }

    pub async fn with_settings(settings: HubSettings) -> Self {
        let clock = FakeClock::at_default_start();
        let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(clock.clone()))
            .await
            .unwrap();
        let backend = ScriptedBackend::new(BackendKind::Claude, Arc::new(clock.clone()));
        let lanes = Arc::new(tempfile::tempdir().unwrap());
        let runner_config = RunnerConfig::new(lanes.path());
        Self {
            clock,
            db,
            backend,
            lanes,
            settings,
            runner_config,
        }
    }

    /// Starts a process: a fresh `HubCore` and `Runner`, both stopped by
    /// `plan`, and the runner's `hello`.
    pub async fn boot(&self, plan: CrashPlan) -> Sim {
        let core = HubCore::new(
            self.db.clone(),
            Arc::new(self.clock.clone()),
            Arc::new(plan.clone()),
            self.settings.clone(),
        )
        .await
        .unwrap();
        let machine = core.local_machine().await.unwrap();
        let sim = Sim {
            world: self.clone(),
            runner: Arc::new(self.runner(core.link(machine), &plan)),
            api: core.api(),
            plan,
            core,
            machine,
        };
        sim.runner.hello().await.unwrap();
        sim
    }

    fn runner(&self, link: Arc<dyn HubLink>, plan: &CrashPlan) -> Runner {
        let backend: Arc<dyn AgentBackend> = Arc::new(self.backend.clone());
        Runner::new(
            link,
            vec![backend],
            Arc::new(self.clock.clone()),
            Arc::new(plan.clone()),
            self.runner_config.clone(),
        )
    }
}

/// One process: the real hub and the real runner, sharing one crash plan.
#[derive(Debug, Clone)]
pub struct Sim {
    pub world: World,
    pub plan: CrashPlan,
    pub core: HubCore,
    pub machine: MachineId,
    pub runner: Arc<Runner>,
    pub api: Arc<dyn HubApi>,
}

impl Sim {
    /// A new world with one process on it that kills nothing.
    pub async fn start() -> Self {
        World::new().await.boot(CrashPlan::new()).await
    }

    /// The process after this one died: fresh objects on the same world.
    pub async fn restart(&self, plan: CrashPlan) -> Self {
        self.world.boot(plan).await
    }

    /// The same process with its runner talking to the hub through `link`.
    pub fn with_runner_link(mut self, link: Arc<dyn HubLink>) -> Self {
        self.runner = Arc::new(self.world.runner(link, &self.plan));
        self
    }

    pub fn clock(&self) -> &FakeClock {
        &self.world.clock
    }

    pub fn db(&self) -> &HubDb {
        &self.world.db
    }

    pub fn backend(&self) -> &ScriptedBackend {
        &self.world.backend
    }

    /// Sends a message to General through the client interface.
    pub async fn send(&self, id: &str, text: &str) -> EventId {
        self.send_to(None, id, text).await
    }

    /// Sends a message to `project` (General if `None`).
    pub async fn send_to(&self, project: Option<&str>, id: &str, text: &str) -> EventId {
        let accepted = self
            .api
            .send_message(&UserMessage {
                project: project.map(str::to_owned),
                text: text.to_owned(),
                client_message_id: id.to_owned(),
            })
            .await
            .unwrap();
        assert!(!accepted.duplicate, "{id} is new");
        accepted.event_id
    }

    /// Starts `Runner::work_once` on its own task.
    pub fn spawn_work_once(&self) -> Work {
        let runner = Arc::clone(&self.runner);
        Work {
            claims_before: count(&self.plan, AFTER_CLAIM),
            debounces_before: count(&self.plan, AFTER_DEBOUNCE),
            debounced: false,
            task: tokio::spawn(async move { runner.work_once().await }),
        }
    }

    /// `Runner::work_once`, with the fake clock moved past the debounce
    /// window when the runner waits in it.
    pub async fn work_once(&self) -> Result<bool, RunnerError> {
        self.spawn_work_once().finish(self).await
    }

    /// Works lanes until none is granted. Returns how many were.
    pub async fn work_until_idle(&self) -> Result<u32, RunnerError> {
        for granted in 0..50 {
            if !self.work_once().await? {
                return Ok(granted);
            }
        }
        panic!("lanes kept being granted");
    }

    pub async fn dispatch(&self) -> Result<u32, HubError> {
        self.core.dispatch_once().await
    }

    /// The client feed from the start, without waiting.
    pub async fn feed(&self) -> Vec<FeedItem> {
        self.api.feed(0, Duration::ZERO).await.unwrap()
    }

    pub async fn event(&self, id: EventId) -> Event {
        self.db().event(id).await.unwrap().unwrap()
    }

    pub async fn events(&self) -> Vec<Event> {
        let mut all = Vec::new();
        for id in 1.. {
            match self.db().event(EventId(id)).await.unwrap() {
                Some(event) => all.push(event),
                None => return all,
            }
        }
        unreachable!()
    }

    pub async fn runs(&self) -> Vec<Run> {
        let mut all = Vec::new();
        for id in 1.. {
            match self.db().run(RunId(id)).await.unwrap() {
                Some(run) => all.push(run),
                None => return all,
            }
        }
        unreachable!()
    }

    pub async fn outbox(&self) -> Vec<OutboxMessage> {
        let mut all = Vec::new();
        for id in 1.. {
            match self.db().message(OutboxId(id)).await.unwrap() {
                Some(message) => all.push(message),
                None => return all,
            }
        }
        unreachable!()
    }

    pub async fn journal(&self, run: RunId) -> Vec<JournalEntry> {
        self.db().journal(run).await.unwrap()
    }

    /// The `type` of each journal entry of `run`, in order.
    pub async fn journal_types(&self, run: RunId) -> Vec<String> {
        self.journal(run)
            .await
            .iter()
            .map(|entry| {
                entry
                    .entry
                    .get("type")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    /// Every lane of every project.
    pub async fn lanes(&self) -> Vec<Lane> {
        let mut lanes = Vec::new();
        for project in self.db().projects().await.unwrap() {
            lanes.push(self.db().lane(project.lead_lane_id).await.unwrap().unwrap());
        }
        lanes
    }

    pub async fn general_lane(&self) -> Lane {
        let project = self.db().general_project().await.unwrap();
        self.db().lane(project.lead_lane_id).await.unwrap().unwrap()
    }

    /// Committed runs that consumed `event`.
    pub async fn committed_runs_with(&self, event: EventId) -> Vec<Run> {
        let mut committed = Vec::new();
        for run in self.runs().await {
            if run.status == RunStatus::Committed
                && self
                    .db()
                    .run_events(run.id)
                    .await
                    .unwrap()
                    .iter()
                    .any(|consumed| consumed.id == event)
            {
                committed.push(run);
            }
        }
        committed
    }

    /// Registers another machine, reports it usable, and returns its link.
    pub async fn other_machine(&self, name: &str) -> Arc<dyn HubLink> {
        let machine = self
            .db()
            .register_machine(&NewMachine {
                name: name.to_owned(),
                endpoint_id: format!("{name}-key"),
                is_hub: false,
                trust: MachineTrust::Trusted,
                max_parallel: 2,
            })
            .await
            .unwrap();
        let link = self.core.link(machine.id);
        link.hello(&usable_report()).await.unwrap();
        link
    }
}

/// A `work_once` call in progress on its own task.
#[derive(Debug)]
pub struct Work {
    task: JoinHandle<Result<bool, RunnerError>>,
    claims_before: usize,
    debounces_before: usize,
    debounced: bool,
}

impl Work {
    /// Whether the runner claimed a lane and now waits out its debounce
    /// window on the clock.
    fn in_debounce(&self, sim: &Sim) -> bool {
        count(&sim.plan, AFTER_CLAIM) > self.claims_before
            && count(&sim.plan, AFTER_DEBOUNCE) == self.debounces_before
            && sim.clock().sleepers() > 0
    }

    /// Waits until the runner sits in its debounce window. The caller moves
    /// the clock past it.
    pub async fn wait_in_debounce(&mut self, sim: &Sim) {
        until("the runner waits in its debounce window", || {
            std::future::ready(self.in_debounce(sim))
        })
        .await;
        self.debounced = true;
    }

    /// Lets `work_once` finish, moving the clock past the debounce window
    /// if the runner waits in it and the caller has not done so.
    pub async fn finish(mut self, sim: &Sim) -> Result<bool, RunnerError> {
        for _ in 0..YIELDS {
            if self.task.is_finished() {
                return self.task.await.expect("work_once panicked");
            }
            if !self.debounced && self.in_debounce(sim) {
                sim.clock().advance(sim.world.runner_config.debounce);
                self.debounced = true;
            }
            tokio::task::yield_now().await;
        }
        panic!("work_once never finished");
    }
}
