//! Seeded traffic and crash interleavings on the real hub, with no external backend.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use threadlane_core::client::{DecisionReply, DecisionRequest, OrgRequest, TaskReply, TaskRequest};
use threadlane_core::ids::EventId;
use threadlane_core::memory::progress_path;
use threadlane_core::model::{
    Decision, DecisionStatus, EventKind, EventStatus, Lease, Mode, NewAgent, NewBinding, NewEvent,
    OutboxStatus, TaskStatus, TimerStatus, Trust,
};
use threadlane_core::runtime::Step;
use threadlane_core::runtime::steps::{
    begin_run, dispatcher, placement, runner, scheduler, tool_call,
};
use threadlane_core::store::{
    DecisionStore, InboxStore, MemoryStore, OrgStore, StoreError, TaskStore, TimerStore,
};
use threadlane_core::time::Clock;
use threadlane_hub::HubError;
use threadlane_runner::RunnerError;
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{ENGINEER, PAST_LEASE, Sim, finish_after, progress_content, reply_with};
use crate::invariants::{assert_invariants, assert_one_active_run_per_lane, assert_quiet};
use crate::operations::{asking, follow_up};

// SplitMix64 gives the same choices on every platform, including for seed zero.
struct Choices(u64);

impl Choices {
    fn pick(&mut self, count: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut bits = self.0;
        bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((bits ^ (bits >> 31)) % count as u64) as usize
    }
}

async fn answer(sim: &Sim, decision: &Decision, option: &str) {
    let reply = sim
        .api
        .decisions(DecisionRequest::Answer {
            decision: decision.id,
            option: option.to_owned(),
            note: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        reply,
        DecisionReply::Answered { recorded: true, .. }
    ));
}

async fn drive(sim: &Sim) -> Option<Step> {
    match sim.core.schedule_once().await {
        Ok(_) => {}
        Err(HubError::Killed(killed)) => return Some(killed.step),
        Err(error) => panic!("scheduler failed: {error}"),
    }
    match sim.work_until_idle().await {
        Ok(_) => {}
        Err(RunnerError::Killed(killed)) => return Some(killed.step),
        Err(error) => panic!("runner failed: {error}"),
    }
    match sim.dispatch().await {
        Ok(_) => None,
        Err(HubError::Killed(killed)) => Some(killed.step),
        Err(error) => panic!("dispatcher failed: {error}"),
    }
}

async fn recover(sim: Sim) -> Sim {
    let live: Vec<_> = sim
        .lanes()
        .await
        .into_iter()
        .filter_map(|lane| {
            Some(Lease {
                lane_id: lane.id,
                machine_id: lane.lease_owner?,
                epoch: lane.lease_epoch,
                expires_at: lane.lease_expires_at?,
            })
        })
        .collect();
    sim.clock().advance(PAST_LEASE);
    let sim = sim.restart(CrashPlan::new()).await;
    sim.core.sweep_once().await.unwrap();
    for stale in live {
        assert!(matches!(
            sim.db()
                .skip_events(&stale, &[EventId(0)], "stale write")
                .await,
            Err(StoreError::StaleLease { .. })
        ));
    }
    sim
}

async fn check_mode_ceiling(sim: &Sim) {
    let requested = sim
        .send("off-probe", "Accepted before you turned off the lead.")
        .await;
    for mode in [Mode::Off, Mode::OnRequest] {
        sim.api
            .org(OrgRequest::SetCeiling {
                project: None,
                mode,
            })
            .await
            .unwrap();
        let event = if mode == Mode::Off {
            requested
        } else {
            let project = sim.db().general_project().await.unwrap();
            sim.db()
                .insert_event(&NewEvent {
                    project_id: project.id,
                    lane_id: project.lead_lane_id,
                    source: "load".into(),
                    source_event_id: "unrequested-probe".into(),
                    kind: EventKind::System,
                    payload: serde_json::json!({}),
                    trust: Trust::System,
                    requested: false,
                    session_ref: None,
                    decision_ref: None,
                    reply_to_ref: None,
                    received_at: sim.clock().now(),
                })
                .await
                .unwrap()
                .id()
        };
        assert_eq!(drive(sim).await, None);
        assert_eq!(sim.event(event).await.status, EventStatus::Skipped);
    }
    assert!(
        sim.runs().await.is_empty(),
        "Off and unrequested work never opened a run"
    );
}

async fn scenario(seed: u64, project_count: usize, rounds: usize) {
    eprintln!("Threadlane load seed {seed} ({project_count} projects, {rounds} rounds)");
    let label = format!("seed {seed}");
    let mut choices = Choices(seed);
    let mut sim = Sim::start().await;
    check_mode_ceiling(&sim).await;
    let engineer = sim
        .db()
        .create_agent(&NewAgent::named(ENGINEER, "engineer"))
        .await
        .unwrap();
    sim.backend()
        .route("Your task, T-", finish_after(Duration::ZERO));
    sim.backend().route(
        "decision_answer",
        reply_with("answered", "Thanks for the answer."),
    );
    sim.backend()
        .set_default(Script::new().finish("Nothing to add."));
    let mut projects = Vec::with_capacity(project_count);
    for n in 0..project_count {
        let name = format!("Load {n}");
        sim.api
            .org(OrgRequest::CreateProject {
                name: name.clone(),
                description: String::new(),
            })
            .await
            .unwrap();
        let project = sim
            .db()
            .projects()
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap();
        sim.db()
            .bind(&NewBinding {
                project_id: project.id,
                agent_id: engineer.id,
                mode: Mode::OnRequest,
                is_lead: false,
                max_parallel: 2,
            })
            .await
            .unwrap();
        projects.push(name);
    }

    let steps = [
        scheduler::ALL,
        placement::ALL,
        begin_run::ALL,
        runner::TASK,
        tool_call::ALL,
        dispatcher::ALL,
    ]
    .concat();
    let mut kills = 0;
    let mut accepted = Vec::new();
    let mut timers = Vec::new();
    let mut tasks = Vec::new();
    for round in 0..rounds {
        sim.clock()
            .advance(Duration::from_secs(1 + choices.pick(5) as u64));
        let arrivals = 1 + choices.pick(3);
        for arrival in 0..arrivals {
            let key = format!("load-{round}-{arrival}");
            let project = &projects[choices.pick(projects.len())];
            match choices.pick(5) {
                action @ (0 | 1) => {
                    let marker = format!("[{key}]");
                    let script = if action == 0 {
                        reply_with(&key, "Processed incoming work.")
                    } else {
                        asking(Script::new(), &key)
                    };
                    sim.backend().route(&marker, script);
                    accepted.push(sim.send_to(Some(project), &key, &marker).await);
                }
                2 => {
                    let delay = Duration::from_secs(1 + choices.pick(20) as u64);
                    timers.push(follow_up(&sim, Some(project), delay).await);
                }
                3 => {
                    let TaskReply::Task { task } = sim
                        .api
                        .tasks(TaskRequest::Create {
                            project: Some(project.clone()),
                            title: key.clone(),
                            goal: "Write a progress note.".into(),
                            agent: ENGINEER.into(),
                            repositories: Vec::new(),
                            depends_on: Vec::new(),
                            client_request_id: key,
                        })
                        .await
                        .unwrap()
                    else {
                        panic!("expected a task");
                    };
                    tasks.push(task);
                }
                _ => {
                    let pending = sim.db().open_decisions().await.unwrap();
                    if !pending.is_empty() {
                        let option = ["blue", "green"][choices.pick(2)];
                        answer(&sim, &pending[choices.pick(pending.len())], option).await;
                    }
                }
            }
        }

        let kill = (choices.pick(3) != 0).then(|| steps[choices.pick(steps.len())]);
        if let Some(step) = kill {
            sim = sim.restart(CrashPlan::kill_at(step)).await;
        }
        if let Some(step) = drive(&sim).await {
            assert_eq!(Some(step), kill, "{label}, round {round}");
            assert_eq!(sim.plan.killed(), Some(step));
            assert_one_active_run_per_lane(&sim, &label).await;
            sim = recover(sim).await;
            kills += 1;
        } else if kill.is_some() {
            // A boundary this round did not visit must not kill a later round.
            sim = sim.restart(CrashPlan::new()).await;
        }
    }
    assert!(kills > 0, "{label} must exercise recovery");

    sim.clock().advance(PAST_LEASE);
    assert_eq!(drive(&sim).await, None);
    for pending in sim.db().open_decisions().await.unwrap() {
        answer(&sim, &pending, ["blue", "green"][choices.pick(2)]).await;
    }
    assert_eq!(drive(&sim).await, None);
    while sim.dispatch().await.unwrap() != 0 {}

    assert_invariants(&sim, &label).await;
    assert_quiet(&sim, &label).await;
    let events = sim.events().await;
    assert!(
        !accepted.is_empty() && !timers.is_empty() && !tasks.is_empty(),
        "{label}"
    );
    for id in accepted {
        assert_eq!(
            sim.event(id).await.status,
            EventStatus::Done,
            "{label}, message {id}"
        );
    }
    let lanes: HashMap<_, _> = sim
        .lanes()
        .await
        .into_iter()
        .map(|lane| (lane.id, lane.project_id))
        .collect();
    let control = sim.db().general_project().await.unwrap().id;
    for run in sim.runs().await {
        assert_eq!(
            run.machine_id, sim.machine,
            "{label}, run assigned outside the local membership"
        );
        assert_ne!(
            lanes[&run.lane_id], control,
            "{label}, control project started a run"
        );
    }
    assert!(
        sim.backend()
            .received()
            .iter()
            .all(|run| run.working_dir.starts_with(sim.world.lanes.path()))
    );
    assert!(
        sim.world.forge.calls().is_empty(),
        "{label}, no shared effect was approved"
    );
    let mut occurrences = HashSet::new();
    for event in events
        .iter()
        .filter(|event| event.kind == EventKind::TimerFired)
    {
        assert!(
            occurrences.insert(event.source_event_id.as_str()),
            "{label}, duplicate occurrence"
        );
    }
    assert_eq!(
        occurrences.len(),
        timers.len(),
        "{label}, no timer was lost"
    );
    for input in timers {
        let timer = sim.db().timer(input.id).await.unwrap().unwrap();
        assert_eq!(
            (timer.status, timer.fire_count),
            (TimerStatus::Done, 1),
            "{label}"
        );
    }
    for input in tasks {
        let task = sim.db().task(input.id).await.unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Done, "{label}, task {}", task.id);
        let path = progress_path(task.project_id, task.id);
        let history = sim.db().history(&path).await.unwrap();
        assert_eq!(
            history.len(),
            1,
            "{label}, task {} progress written once",
            task.id
        );
        assert_eq!(history[0].content, progress_content(&task.id.to_string()));
    }
    let decisions = sim.decisions().await;
    assert!(!decisions.is_empty(), "{label} must exercise decisions");
    for decision in decisions {
        assert_eq!(decision.status, DecisionStatus::Answered, "{label}");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.decision_ref == Some(decision.id))
                .count(),
            1,
            "{label}, answer event for {}",
            decision.id,
        );
    }
    let outbox = sim.outbox().await;
    let mut cursor = 0;
    let mut delivered = HashSet::new();
    loop {
        let page = sim.api.feed(cursor, Duration::ZERO).await.unwrap();
        let Some(last) = page.last() else {
            break;
        };
        cursor = last.seq;
        for item in page {
            assert!(
                delivered.insert(item.message.id),
                "{label}, duplicate delivery"
            );
        }
    }
    assert!(!outbox.is_empty(), "{label} must exercise delivery");
    for row in outbox {
        assert_eq!(row.status, OutboxStatus::Sent, "{label}, outbox {}", row.id);
        assert!(
            delivered.contains(&row.id),
            "{label}, missing delivery {}",
            row.id
        );
    }
}

fn requested_seed() -> Option<u64> {
    std::env::var("THREADLANE_LOAD_SEED").ok().map(|seed| {
        seed.parse()
            .expect("THREADLANE_LOAD_SEED must be a decimal u64")
    })
}

#[tokio::test]
async fn seeded_load_small() {
    if let Some(seed) = requested_seed() {
        scenario(seed, 4, 24).await;
    } else {
        for seed in [1, 42, 20261117] {
            scenario(seed, 4, 24).await;
        }
    }
}

#[tokio::test]
#[ignore = "large deterministic crash simulation"]
async fn seeded_load_large() {
    scenario(requested_seed().unwrap_or(20261117), 24, 350).await;
}
