//! The test harness: an in-memory hub on a fake clock, and a helper that
//! plays the runner's side of the run cycle (docs/milestone-1.md §3).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use threadlane_core::backend::{Probe, encode_backends};
use threadlane_core::client::{Accepted, FeedItem, HubApi, UserMessage};
use threadlane_core::ids::{EventId, MachineId, RunId};
use threadlane_core::link::{Begin, HubLink, LinkError, RunAssignment};
use threadlane_core::model::{
    BackendKind, Event, EventKind, Lane, Lease, MachineReport, MachineTrust, NewEvent, NewMachine,
    Project, RunOutcome, RunStatus, Trust,
};
use threadlane_core::runtime::{NoHooks, StepHook};
use threadlane_core::store::{InboxStore, MachineStore, OrgStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_core::tools::ToolError;
use threadlane_hub::{HubCore, HubError, HubSettings, SEND_MESSAGE};
use threadlane_store::{Bootstrap, HubDb};
use threadlane_testkit::FakeClock;

/// What the played backend says at the end of every run: the filler, so a
/// lead run that sends nothing stays silent.
pub const RATIONALE: &str = "No response requested.";

/// One past the default lease, to let a lease expire.
pub const PAST_LEASE: Duration = Duration::from_secs(61);

/// An in-memory hub, its clock, and a link for the hub's own machine.
#[derive(Debug, Clone)]
pub struct TestHub {
    pub clock: FakeClock,
    pub db: HubDb,
    pub core: HubCore,
    pub machine: MachineId,
    pub link: Arc<dyn HubLink>,
    pub api: Arc<dyn HubApi>,
}

impl TestHub {
    pub async fn start() -> Self {
        Self::with(HubSettings::default(), Arc::new(NoHooks)).await
    }

    pub async fn with(settings: HubSettings, hook: Arc<dyn StepHook>) -> Self {
        let clock = FakeClock::at_default_start();
        let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(clock.clone()))
            .await
            .unwrap();
        Self::on(db, clock, settings, hook).await
    }

    async fn on(
        db: HubDb,
        clock: FakeClock,
        settings: HubSettings,
        hook: Arc<dyn StepHook>,
    ) -> Self {
        let core = HubCore::new(db.clone(), Arc::new(clock.clone()), hook, settings)
            .await
            .unwrap();
        Self::around(core, clock).await
    }

    /// The harness around an already built core.
    pub async fn around(core: HubCore, clock: FakeClock) -> Self {
        let machine = core.local_machine().await.unwrap();
        Self {
            db: core.db().clone(),
            link: core.link(machine),
            api: core.api(),
            machine,
            core,
            clock,
        }
    }

    /// A fresh `HubCore` on the same database and clock, as after a restart.
    pub async fn restart(&self, hook: Arc<dyn StepHook>) -> Self {
        Self::on(
            self.db.clone(),
            self.clock.clone(),
            self.core.settings().clone(),
            hook,
        )
        .await
    }

    /// Reports the hub's machine with a usable backend.
    pub async fn hello(&self) {
        self.link.hello(&usable_report()).await.unwrap();
    }

    /// Sends a message to General with `id` as its client message id.
    pub async fn send(&self, id: &str, text: &str) -> Accepted {
        self.api
            .send_message(&UserMessage {
                project: None,
                text: text.to_owned(),
                client_message_id: id.to_owned(),
                task: None,
            })
            .await
            .unwrap()
    }

    /// Registers another machine, reports it usable, and returns its link.
    pub async fn other_machine(&self, name: &str) -> (MachineId, Arc<dyn HubLink>) {
        let machine = self
            .db
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
        (machine.id, link)
    }

    pub async fn general(&self) -> Project {
        self.db.general_project().await.unwrap()
    }

    pub async fn general_lane(&self) -> Lane {
        let lane_id = self.general().await.lead_lane_id;
        self.db.lane(lane_id).await.unwrap().unwrap()
    }

    pub async fn event(&self, id: EventId) -> Event {
        self.db.event(id).await.unwrap().unwrap()
    }

    /// Inserts an event into General's lead lane directly, as another source would.
    pub async fn insert(&self, event: impl FnOnce(&mut NewEvent)) -> EventId {
        let general = self.general().await;
        let mut new = NewEvent {
            project_id: general.id,
            lane_id: general.lead_lane_id,
            source: "test".to_owned(),
            source_event_id: format!("test-{}", self.clock.now()),
            kind: EventKind::OutsideEvent,
            payload: json!({ "text": "something happened" }),
            trust: Trust::System,
            requested: true,
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: self.clock.now(),
        };
        event(&mut new);
        self.db.insert_event(&new).await.unwrap().id()
    }

    /// The client feed from the start, without waiting.
    pub async fn feed(&self) -> Vec<FeedItem> {
        self.api.feed(0, Duration::ZERO).await.unwrap()
    }

    /// Committed runs that consumed `event`.
    pub async fn committed_runs_with(&self, event: EventId) -> usize {
        let mut count = 0;
        for id in 1.. {
            let Some(run) = self.db.run(RunId(id)).await.unwrap() else {
                break;
            };
            if run.status == RunStatus::Committed
                && self
                    .db
                    .run_events(run.id)
                    .await
                    .unwrap()
                    .iter()
                    .any(|consumed| consumed.id == event)
            {
                count += 1;
            }
        }
        count
    }
}

/// A machine report with Claude installed and logged in.
pub fn usable_report() -> MachineReport {
    report_with(Probe {
        installed: true,
        version: Some("2.1.325".into()),
        logged_in: true,
        account_label: Some("me@example.com".into()),
    })
}

pub fn report_with(probe: Probe) -> MachineReport {
    MachineReport {
        capabilities: json!({}),
        backends: encode_backends(&[(BackendKind::Claude, probe)]),
        gh_login: true,
        load: 0,
    }
}

/// What the played backend does in each run.
#[derive(Debug, Clone, Copy)]
pub enum Reply<'a> {
    /// Calls nothing and finishes.
    Silent,
    /// Calls `send_message` once with this call id and text.
    Send(Option<&'a str>, &'a str),
}

/// Why playing stopped early.
#[derive(Debug)]
pub enum Stop {
    Link(LinkError),
    Tool(ToolError),
    Hub(HubError),
}

impl Stop {
    /// Whether playing stopped because the hub was killed.
    pub fn is_killed(&self) -> bool {
        matches!(
            self,
            Self::Link(LinkError::Killed(_))
                | Self::Tool(ToolError::Killed(_))
                | Self::Hub(HubError::Killed(_))
        )
    }
}

impl From<LinkError> for Stop {
    fn from(error: LinkError) -> Self {
        Self::Link(error)
    }
}

impl From<ToolError> for Stop {
    fn from(error: ToolError) -> Self {
        Self::Tool(error)
    }
}

impl From<HubError> for Stop {
    fn from(error: HubError) -> Self {
        Self::Hub(error)
    }
}

/// What one played lane cycle did.
#[derive(Debug, Default)]
pub struct Played {
    pub lease: Option<Lease>,
    pub runs: Vec<(RunAssignment, RunOutcome)>,
}

impl Played {
    pub fn only_run(&self) -> &RunAssignment {
        assert_eq!(self.runs.len(), 1, "exactly one run");
        &self.runs[0].0
    }
}

/// Plays the runner for one lane, in the order of docs/milestone-1.md §3:
/// `request_work`, then `begin_run` → optional `call_tool` → `complete`
/// until `Idle`, then `release`. Nothing granted means nothing played.
pub async fn play(link: &dyn HubLink, reply: Reply<'_>) -> Result<Played, Stop> {
    let mut played = Played::default();
    let Some(lease) = link.request_work().await? else {
        return Ok(played);
    };
    played.lease = Some(lease);
    loop {
        let assignment = match link.begin_run(&lease).await? {
            Begin::Idle => break,
            Begin::Run(assignment) => *assignment,
        };
        if let Reply::Send(call_id, text) = reply {
            link.call_tool(
                &lease,
                assignment.run_id,
                call_id,
                SEND_MESSAGE,
                &json!({ "text": text }),
            )
            .await?;
        }
        let outcome = link
            .complete(&lease, assignment.run_id, Some(RATIONALE))
            .await?;
        played.runs.push((assignment, outcome));
    }
    link.release(&lease).await?;
    Ok(played)
}

/// Grants a lane and begins one run on it, expecting both.
pub async fn begin(link: &dyn HubLink) -> (Lease, RunAssignment) {
    let lease = link
        .request_work()
        .await
        .unwrap()
        .expect("a lane is granted");
    match link.begin_run(&lease).await.unwrap() {
        Begin::Run(assignment) => (lease, *assignment),
        Begin::Idle => panic!("expected a run"),
    }
}

/// Waits for `check` to hold, yielding to other tasks; real time is only a
/// short fallback for work outside this runtime.
pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..2_000 {
        if check().await {
            return;
        }
        tokio::task::yield_now().await;
    }
    for _ in 0..400 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("never happened: {what}");
}
