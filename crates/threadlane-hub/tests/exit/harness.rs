//! The real hub and the real runner together: one in-memory database, one
//! fake clock, one scripted backend, and one `CrashPlan` as the step hook of
//! both (docs/milestone-1.md §10).
//!
//! A [`World`] is what outlives a process: the stores, the clock, and the
//! machine's disk (the lanes directory and the backend's native sessions). A
//! [`Sim`] is one process on it: a `HubCore` and a `Runner`. Restarting means
//! building a fresh `Sim` on the same `World`.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value as Json, json};
use tempfile::TempDir;
use threadlane_core::backend::{AgentBackend, Probe, encode_backends};
use threadlane_core::client::{FeedItem, HubApi, UserMessage};
use threadlane_core::ids::{DecisionId, EventId, MachineId, OutboxId, RunId};
use threadlane_core::link::HubLink;
use threadlane_core::model::{
    BackendKind, Decision, Event, JournalEntry, Lane, MachineReport, MachineTrust, Mode, NewAgent,
    NewBinding, NewMachine, NewRepository, OutboxMessage, Repository, Run, RunStatus, Task,
};
use threadlane_core::runtime::Step;
use threadlane_core::runtime::steps::runner::{AFTER_CLAIM, AFTER_DEBOUNCE};
use threadlane_core::store::{
    DecisionStore, InboxStore, MachineStore, OrgStore, OutboxStore, SessionStore, TaskStore,
};
use threadlane_hub::{
    CREATE_TASK, FINISH_TASK, HubCore, HubError, HubSettings, MEMORY_CREATE, SEND_MESSAGE,
};
use threadlane_runner::{GitWorkspaces, PROGRESS_PROMPT, Runner, RunnerConfig, RunnerError};
use threadlane_store::{Bootstrap, HubDb};
use threadlane_testkit::fakes::FakeForge;
use threadlane_testkit::git::bare_repository;
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
/// Only yields, except while `git` runs (task worktrees): see [`until`].
const YIELDS: u32 = 200_000;

/// How long [`until`] waits in all for real work outside the process, such
/// as `git` setting up a worktree.
const REAL_WAIT: Duration = Duration::from_secs(30);

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

/// The agent the lead gives tasks to in the task scenarios.
pub const ENGINEER: &str = "Engineer";

/// What a task agent reports when it finishes.
pub const TASK_SUMMARY: &str = "Done as asked.";

/// A task agent that writes its progress file and finishes its task, after
/// waiting `wait` on the clock.
pub fn finish_after(wait: Duration) -> Script {
    Script::new()
        .wait(wait)
        .text("Working on it.")
        .call_for("toolu_p", MEMORY_CREATE, write_progress)
        .call("toolu_f", FINISH_TASK, json!({ "summary": TASK_SUMMARY }))
        .finish("Finished the task.")
}

/// The task a task run's prompt names, as `T-0001`.
pub fn prompt_task(prompt: &str) -> &str {
    prompt
        .split_once("Your task, ")
        .and_then(|(_, rest)| rest.split_once(':'))
        .map_or("T-unknown", |(id, _)| id)
}

/// The progress file of a task as the agent of the task its prompt names
/// writes it.
pub fn progress_content(task: &str) -> String {
    format!("# {task}\nDone as asked; nothing is left.\n")
}

/// The arguments of a `memory_create` call writing the progress file of the
/// task the prompt names.
pub fn write_progress(prompt: &str) -> Json {
    let task = prompt_task(prompt);
    json!({ "path": format!("project/tasks/{task}.md"), "content": progress_content(task) })
}

/// A lead that turns the goal into tasks for the engineer: a title and the
/// titles of the tasks, created before it, that it depends on.
pub fn plan_tasks(tasks: &[(&str, &[&str])]) -> Script {
    plan_tasks_in(&[], tasks)
}

/// [`plan_tasks`], each task working in `repositories`.
pub fn plan_tasks_in(repositories: &[&str], tasks: &[(&str, &[&str])]) -> Script {
    let mut script = Script::new().text("Splitting the goal into tasks.");
    for (n, (title, depends_on)) in tasks.iter().enumerate() {
        let depends_on: Vec<String> = depends_on
            .iter()
            .map(|title| {
                let index = tasks.iter().position(|(other, _)| other == title).unwrap();
                format!("T-{:04}", index + 1)
            })
            .collect();
        script = script.call(
            format!("toolu_c{n}"),
            CREATE_TASK,
            json!({
                "title": title,
                "goal": format!("{title} is done"),
                "agent": ENGINEER,
                "depends_on": depends_on,
                "repositories": repositories,
            }),
        );
    }
    script.finish("Planned the tasks.")
}

/// Binds an engineer to General that may run `max_parallel` tasks at once.
pub async fn add_engineer(db: &HubDb, max_parallel: u32) {
    let agent = db
        .create_agent(&NewAgent::named(ENGINEER, "engineer"))
        .await
        .unwrap();
    db.bind(&NewBinding {
        project_id: db.general_project().await.unwrap().id,
        agent_id: agent.id,
        mode: Mode::OnRequest,
        is_lead: false,
        max_parallel,
    })
    .await
    .unwrap();
}

/// How many times `plan` saw `step`.
pub fn count(plan: &CrashPlan, step: Step) -> usize {
    plan.reached().iter().filter(|seen| **seen == step).count()
}

/// Yields until `check` holds. Panics, naming `what`, if it never does.
/// Once yielding alone has not made it hold, it also waits a little real
/// time between checks, for `git` subprocesses to finish.
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
    let deadline = std::time::Instant::now() + REAL_WAIT;
    while std::time::Instant::now() < deadline {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("never happened: {what}");
}

/// Waits for a spawned task to finish, yielding only.
pub async fn join<T>(what: &str, task: JoinHandle<T>) -> T {
    until(what, || std::future::ready(task.is_finished())).await;
    task.await.unwrap_or_else(|error| panic!("{what}: {error}"))
}

/// A machine report with Claude and `gh` installed and logged in.
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
        gh_login: true,
        load: 0,
    }
}

/// What a task agent's final progress turn does unless a scenario says
/// otherwise: it ends the turn.
pub fn progress_turn() -> Script {
    Script::new().finish("Nothing more to save.")
}

/// A scripted backend whose final progress turns play [`progress_turn`],
/// before any route a scenario adds.
fn scripted(kind: BackendKind, clock: &FakeClock) -> ScriptedBackend {
    let backend = ScriptedBackend::new(kind, Arc::new(clock.clone()));
    backend.route(PROGRESS_PROMPT, progress_turn());
    backend
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
    /// The repositories' forge: what was pushed and opened survives too.
    pub forge: FakeForge,
    /// A second backend on the machine, if the scenario has one.
    pub codex: Option<ScriptedBackend>,
}

impl World {
    pub async fn new() -> Self {
        Self::with_settings(HubSettings::default()).await
    }

    pub async fn with_settings(settings: HubSettings) -> Self {
        Self::on(settings, None).await
    }

    /// A world whose hub database is the file `hub.db` in `dir`.
    pub async fn on_disk(dir: &std::path::Path) -> Self {
        Self::on(HubSettings::default(), Some(dir.join("hub.db"))).await
    }

    async fn on(settings: HubSettings, file: Option<PathBuf>) -> Self {
        let clock = FakeClock::at_default_start();
        let db = match file {
            Some(path) => HubDb::open(&path, &Bootstrap::default(), Arc::new(clock.clone())).await,
            None => HubDb::open_in_memory(&Bootstrap::default(), Arc::new(clock.clone())).await,
        }
        .unwrap();
        let backend = scripted(BackendKind::Claude, &clock);
        let lanes = Arc::new(tempfile::tempdir().unwrap());
        let runner_config = RunnerConfig::new(lanes.path());
        Self {
            clock,
            db,
            backend,
            lanes,
            settings,
            runner_config,
            forge: FakeForge::default(),
            codex: None,
        }
    }

    /// Adds a scripted Codex next to the scripted Claude, on its own account.
    pub fn with_codex(mut self) -> Self {
        self.codex = Some(scripted(BackendKind::Codex, &self.clock));
        self
    }

    /// Where the machine keeps its clones and task worktrees.
    pub fn workspaces(&self) -> PathBuf {
        self.lanes.path().join("workspaces")
    }

    /// Registers a local bare repository named `name` in General, made in
    /// the machine's temp directory.
    pub async fn add_repository(&self, name: &str) -> Repository {
        let remote = bare_repository(&self.lanes.path().join("remotes"), name);
        self.db
            .create_repository(&NewRepository {
                project_id: self.db.general_project().await.unwrap().id,
                name: name.to_owned(),
                remote_url: remote.display().to_string(),
                default_branch: "main".to_owned(),
                protected_branches: Vec::new(),
            })
            .await
            .unwrap()
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
        let mut backends: Vec<Arc<dyn AgentBackend>> = vec![Arc::new(self.backend.clone())];
        if let Some(codex) = &self.codex {
            backends.push(Arc::new(codex.clone()));
        }
        Runner::new(
            link,
            backends,
            Arc::new(GitWorkspaces::new(self.workspaces())),
            Arc::new(self.forge.clone()),
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
                task: None,
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

    /// Every lane of every project: lead lanes, then task lanes.
    pub async fn lanes(&self) -> Vec<Lane> {
        let mut ids = Vec::new();
        for project in self.db().projects().await.unwrap() {
            ids.push(project.lead_lane_id);
        }
        ids.extend(self.tasks().await.iter().map(|task| task.lane_id));
        let mut lanes = Vec::new();
        for id in ids {
            lanes.push(self.db().lane(id).await.unwrap().unwrap());
        }
        lanes
    }

    /// Every decision, oldest first.
    pub async fn decisions(&self) -> Vec<Decision> {
        let mut all = Vec::new();
        for id in 1.. {
            match self.db().decision(DecisionId(id)).await.unwrap() {
                Some(decision) => all.push(decision),
                None => return all,
            }
        }
        unreachable!()
    }

    /// Every task of every project.
    pub async fn tasks(&self) -> Vec<Task> {
        let mut tasks = Vec::new();
        for project in self.db().projects().await.unwrap() {
            tasks.extend(self.db().tasks(project.id).await.unwrap());
        }
        tasks
    }

    /// The task with this short id, for example `T-0001`.
    pub async fn task(&self, id: &str) -> Task {
        self.tasks()
            .await
            .into_iter()
            .find(|task| task.id.to_string() == id)
            .unwrap_or_else(|| panic!("no task {id}"))
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
        until("work_once finishes", || {
            if !self.debounced && self.in_debounce(sim) {
                sim.clock().advance(sim.world.runner_config.debounce);
                self.debounced = true;
            }
            std::future::ready(self.task.is_finished())
        })
        .await;
        self.task.await.expect("work_once panicked")
    }
}
