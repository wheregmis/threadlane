//! The run cycle end to end (docs/milestone-1.md §10 items 1, 2 and 4, and
//! Plan.md §4.3 step 6).

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;
use threadlane_core::backend::NativeSession;
use threadlane_core::client::{ApiError, OrgRequest, UserMessage};
use threadlane_core::forge::REPORT_LIMITATION;
use threadlane_core::model::{
    BackendKind, EventStatus, Mode, NewAgent, OutboxKind, RunOutcome, RunStatus,
};
use threadlane_core::runtime::steps::{placement, runner};
use threadlane_core::store::{OrgStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_hub::{
    ASK_PROJECT_LEAD, ASK_USER, CANCEL_TIMER, CREATE_SCHEDULE, LIST_TIMERS, SET_FOLLOW_UP,
};
use threadlane_hub::{
    CANCEL_TASK, CREATE_TASK, HubSettings, LEAD_NAME, LEAD_ROLE, LIST_TASKS, MEMORY_CREATE,
    MEMORY_DELETE, MEMORY_EDIT, MEMORY_RENAME, MEMORY_VIEW, OFF_REASON, SEND_MESSAGE, STAY_SILENT,
    UPDATE_TASK,
};
use threadlane_testkit::{CrashPlan, Script, SessionOutcome};

use crate::harness::{RATIONALE, REPLY, Sim, World, count, reply_script, reply_with, until};
use crate::invariants::{assert_invariants, assert_quiet};

#[tokio::test]
async fn a_message_produces_exactly_one_reply() {
    let sim = Sim::start().await;
    sim.backend().set_default(reply_script());
    let event = sim.send("m-1", "Are you there?").await;

    assert_eq!(sim.work_until_idle().await.unwrap(), 1);
    assert_eq!(sim.dispatch().await.unwrap(), 1);

    assert_eq!(sim.event(event).await.status, EventStatus::Done);
    let runs = sim.committed_runs_with(event).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, Some(RunOutcome::Sent));
    assert_eq!(runs[0].rationale.as_deref(), Some(RATIONALE));
    assert_eq!(
        sim.journal_types(runs[0].id).await,
        [
            "session_started",
            "text",
            "tool_use",
            "tool_result",
            "finished"
        ],
        "every backend event was mirrored"
    );

    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].message.kind, OutboxKind::Message);
    assert_eq!(feed[0].message.body, json!({ "text": REPLY }));
    assert_eq!(feed[0].project_name.as_deref(), Some("General"));
    assert_eq!(sim.dispatch().await.unwrap(), 0, "delivered once");

    let received = sim.backend().received();
    assert_eq!(received.len(), 1, "one backend start");
    let run = &received[0];
    assert!(matches!(run.session, NativeSession::Start { .. }));
    assert_eq!(run.session_outcome, SessionOutcome::Created);
    assert!(run.prompt.contains("Are you there?"), "{}", run.prompt);
    assert_eq!(
        run.tools,
        [
            SEND_MESSAGE,
            STAY_SILENT,
            CREATE_TASK,
            UPDATE_TASK,
            threadlane_hub::RETRY_TASK,
            CANCEL_TASK,
            LIST_TASKS,
            threadlane_hub::VIEW_ISSUE,
            threadlane_hub::PR_READINESS,
            threadlane_hub::PR_REVIEW_STATUS,
            threadlane_hub::MERGE,
            MEMORY_VIEW,
            MEMORY_CREATE,
            MEMORY_EDIT,
            MEMORY_DELETE,
            MEMORY_RENAME,
            SET_FOLLOW_UP,
            CREATE_SCHEDULE,
            LIST_TIMERS,
            CANCEL_TIMER,
            ASK_USER,
            ASK_PROJECT_LEAD,
            REPORT_LIMITATION,
        ],
        "a lead gets the lead's tools only"
    );
    assert!(run.remove_builtin_tools, "a lead has no built-in tools");
    assert_eq!(run.model, None, "no model configured anywhere");
    let lane = sim.general_lane().await;
    assert_eq!(
        run.working_dir,
        sim.world.lanes.path().join(lane.id.to_string())
    );
    assert!(run.working_dir.is_dir());

    assert_quiet(&sim, "one message").await;
    assert_invariants(&sim, "one message").await;
}

#[tokio::test]
async fn an_agent_set_to_off_never_runs() {
    let sim = Sim::start().await;
    sim.backend().set_default(reply_script());
    // Accepted while the lead was on, turned off before the runner gets to it.
    let event = sim.send("m-1", "Are you there?").await;
    sim.api
        .org(OrgRequest::SetMode {
            project: None,
            agent: LEAD_NAME.into(),
            mode: Some(Mode::Off),
        })
        .await
        .unwrap();
    let refused = sim
        .api
        .send_message(&UserMessage {
            project: None,
            text: "Hello?".into(),
            images: Vec::new(),
            client_message_id: "m-2".into(),
            task: None,
        })
        .await;
    assert!(matches!(refused, Err(ApiError::Invalid(_))), "{refused:?}");

    sim.work_until_idle().await.unwrap();
    assert_eq!(sim.dispatch().await.unwrap(), 0);

    let event = sim.event(event).await;
    assert_eq!(event.status, EventStatus::Skipped);
    assert_eq!(event.status_reason.as_deref(), Some(OFF_REASON));
    assert_eq!(
        sim.events().await.len(),
        1,
        "the refused message is not stored"
    );
    assert!(sim.runs().await.is_empty(), "no run was opened");
    assert!(sim.backend().received().is_empty(), "no backend started");
    assert!(sim.feed().await.is_empty());

    assert_quiet(&sim, "agent off").await;
    assert_invariants(&sim, "agent off").await;
}

#[tokio::test]
async fn three_rapid_messages_produce_one_run_and_one_reply() {
    let sim = Sim::start().await;
    sim.backend().set_default(reply_script());
    let start = sim.clock().now();
    let first = sim.send("m-1", "I need the report").await;
    let mut work = sim.spawn_work_once();
    work.wait_in_debounce(&sim).await;

    // The window is 2 s; the other two arrive inside it.
    let second = sim.send("m-2", "for the third quarter").await;
    sim.clock().advance(Duration::from_secs(1));
    let third = sim.send("m-3", "as a table, please").await;
    sim.clock().advance(Duration::from_secs(1));
    assert!(work.finish(&sim).await.unwrap());
    assert_eq!(sim.clock().now(), start + Duration::from_secs(2));
    assert!(!sim.work_once().await.unwrap(), "nothing is left");
    assert_eq!(sim.dispatch().await.unwrap(), 1);

    let received = sim.backend().received();
    assert_eq!(received.len(), 1, "one run");
    for text in [
        "I need the report",
        "for the third quarter",
        "as a table, please",
    ] {
        assert!(
            received[0].prompt.contains(text),
            "the prompt holds {text:?}: {}",
            received[0].prompt
        );
    }
    let runs = sim.runs().await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, Some(RunOutcome::Sent));
    let consumed: Vec<_> = sim
        .db()
        .run_events(runs[0].id)
        .await
        .unwrap()
        .iter()
        .map(|event| event.id)
        .collect();
    assert_eq!(consumed, [first, second, third]);
    assert_eq!(sim.feed().await.len(), 1, "one reply");

    assert_quiet(&sim, "three messages").await;
    assert_invariants(&sim, "three messages").await;
}

#[tokio::test]
async fn a_lead_run_that_ends_in_text_delivers_it_once() {
    let sim = Sim::start().await;
    sim.backend().set_default(
        Script::new()
            .text("Thinking.")
            .finish("You're welcome, the work is merged."),
    );
    let event = sim.send("m-1", "Thanks!").await;

    assert_eq!(sim.work_until_idle().await.unwrap(), 1);
    assert_eq!(sim.dispatch().await.unwrap(), 1);

    assert_eq!(sim.event(event).await.status, EventStatus::Done);
    let runs = sim.committed_runs_with(event).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, Some(RunOutcome::Sent));
    assert_eq!(
        runs[0].rationale.as_deref(),
        Some("You're welcome, the work is merged."),
        "the final text is still the rationale"
    );
    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1, "delivered once");
    assert_eq!(
        feed[0].message.body,
        json!({ "text": "You're welcome, the work is merged." })
    );
    assert_eq!(sim.outbox().await.len(), 1);

    assert_quiet(&sim, "final text").await;
    assert_invariants(&sim, "final text").await;
}

#[tokio::test]
async fn a_lead_run_that_called_send_message_delivers_nothing_more() {
    let sim = Sim::start().await;
    sim.backend().set_default(reply_script());
    sim.send("m-1", "Hello").await;

    assert_eq!(sim.work_until_idle().await.unwrap(), 1);
    assert_eq!(sim.dispatch().await.unwrap(), 1);

    let feed = sim.feed().await;
    assert_eq!(feed.len(), 1, "only the sent message, not the final text");
    assert_eq!(feed[0].message.body, json!({ "text": REPLY }));
    assert_eq!(sim.outbox().await.len(), 1);

    assert_invariants(&sim, "sent and final text").await;
}

#[tokio::test]
async fn a_lead_run_ending_in_filler_or_nothing_is_silent() {
    for last in ["No response requested.", "  "] {
        let sim = Sim::start().await;
        sim.backend()
            .set_default(Script::new().text("Nothing to add.").finish(last));
        let event = sim.send("m-1", "Thanks!").await;

        assert_eq!(sim.work_until_idle().await.unwrap(), 1);
        assert_eq!(sim.dispatch().await.unwrap(), 0);
        let runs = sim.committed_runs_with(event).await;
        assert_eq!(runs[0].outcome, Some(RunOutcome::Silent), "{last:?}");
        assert!(sim.feed().await.is_empty(), "{last:?} is not delivered");
        assert!(sim.outbox().await.is_empty());

        assert_quiet(&sim, "silent").await;
        assert_invariants(&sim, "silent").await;
    }
}

#[tokio::test]
async fn a_message_during_a_run_gets_a_second_run_under_the_same_claim() {
    let sim = Sim::start().await;
    sim.backend().push(
        Script::new()
            .text("Looking into it.")
            .wait(Duration::from_secs(10))
            .call("toolu_1", SEND_MESSAGE, json!({ "text": "First answer." }))
            .finish(RATIONALE),
    );
    sim.backend().push(reply_with("toolu_2", "Second answer."));
    let first = sim.send("m-1", "First question").await;

    let mut work = sim.spawn_work_once();
    work.wait_in_debounce(&sim).await;
    sim.clock().advance(sim.world.runner_config.debounce);
    let backend = sim.backend().clone();
    let clock = sim.clock().clone();
    // The run waits on the clock: its wall-time limit and its script's wait.
    until("the first run waits in its script", || {
        std::future::ready(backend.received().len() == 1 && clock.sleepers() >= 2)
    })
    .await;
    let second = sim.send("m-2", "Second question").await;
    sim.clock().advance(Duration::from_secs(10));
    assert!(work.finish(&sim).await.unwrap());

    assert_eq!(count(&sim.plan, runner::AFTER_CLAIM), 1, "one claim");
    assert_eq!(count(&sim.plan, placement::AFTER_GRANT), 1, "one grant");
    assert_eq!(sim.general_lane().await.lease_epoch, 1, "one lease");
    let runs = sim.runs().await;
    assert_eq!(runs.len(), 2, "two runs");
    for (run, event) in runs.iter().zip([first, second]) {
        assert_eq!(run.status, RunStatus::Committed);
        assert_eq!(run.outcome, Some(RunOutcome::Sent));
        assert_eq!(run.lease_epoch, 1);
        let consumed: Vec<_> = sim
            .db()
            .run_events(run.id)
            .await
            .unwrap()
            .iter()
            .map(|event| event.id)
            .collect();
        assert_eq!(consumed, [event]);
    }
    let received = sim.backend().received();
    assert_eq!(received.len(), 2);
    assert_eq!(received[1].session_outcome, SessionOutcome::Resumed);
    assert!(received[1].prompt.contains("Second question"));

    assert_eq!(sim.dispatch().await.unwrap(), 2);
    let texts: Vec<_> = sim
        .feed()
        .await
        .iter()
        .map(|item| item.message.body["text"].clone())
        .collect();
    assert_eq!(texts, [json!("First answer."), json!("Second answer.")]);

    assert_quiet(&sim, "message during a run").await;
    assert_invariants(&sim, "message during a run").await;
}

#[tokio::test]
async fn the_configured_model_is_used_when_the_agent_names_none() {
    let settings = HubSettings {
        default_models: HashMap::from([(BackendKind::Claude, "haiku".to_owned())]),
        ..HubSettings::default()
    };
    let sim = World::with_settings(settings.clone())
        .await
        .boot(CrashPlan::new())
        .await;
    sim.backend().set_default(reply_script());
    sim.send("m-1", "hi").await;
    sim.work_until_idle().await.unwrap();
    assert_eq!(sim.backend().received()[0].model.as_deref(), Some("haiku"));
    assert_invariants(&sim, "configured model").await;

    // A lead agent that names its own model wins over the hub's setting.
    let world = World::with_settings(settings).await;
    world
        .db
        .create_agent(&NewAgent {
            models: json!({ "claude": "sonnet" }),
            ..NewAgent::named("Lead", LEAD_ROLE)
        })
        .await
        .unwrap();
    let sim = world.boot(CrashPlan::new()).await;
    sim.backend().set_default(reply_script());
    sim.send("m-1", "hi").await;
    sim.work_until_idle().await.unwrap();
    assert_eq!(sim.backend().received()[0].model.as_deref(), Some("sonnet"));
    assert_invariants(&sim, "agent's model").await;
}
