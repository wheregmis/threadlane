//! The whole run cycle, batching, and the initiative check (Plan.md §4.3, §4.8).

use serde_json::json;
use threadlane_core::forge::REPORT_LIMITATION;
use threadlane_core::link::Begin;
use threadlane_core::model::{
    EventKind, EventStatus, LaneKind, OutboxKind, RunOutcome, RunStatus, Trust,
};
use threadlane_core::store::{InboxStore, SessionStore};
use threadlane_core::time::Clock;
use threadlane_core::tools::ToolError;
use threadlane_hub::{
    ASK_PROJECT_LEAD, ASK_USER, CANCEL_TIMER, CREATE_SCHEDULE, LIST_TIMERS, SET_FOLLOW_UP,
};
use threadlane_hub::{
    CANCEL_TASK, CREATE_TASK, LIST_TASKS, MEMORY_CREATE, MEMORY_DELETE, MEMORY_EDIT, MEMORY_RENAME,
    MEMORY_VIEW, SEND_MESSAGE, SKIP_REASON, STAY_SILENT, UPDATE_TASK,
};

use crate::common::{RATIONALE, Reply, TestHub, begin, play};

#[tokio::test]
async fn a_message_produces_one_sent_reply_in_the_feed() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let accepted = hub.send("m-1", "Are the tests green?").await;

    let played = play(&*hub.link, Reply::Send(Some("toolu_1"), "Yes, all green."))
        .await
        .unwrap();
    assert_eq!(played.runs.len(), 1);
    let (assignment, outcome) = &played.runs[0];
    assert_eq!(*outcome, RunOutcome::Sent);
    assert_eq!(assignment.events, vec![accepted.event_id]);
    assert_eq!(assignment.lane_kind, LaneKind::Lead);
    assert!(assignment.remove_builtin_tools);
    assert!(assignment.deny_rules.is_empty());
    assert_eq!(
        assignment
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            SEND_MESSAGE,
            STAY_SILENT,
            CREATE_TASK,
            UPDATE_TASK,
            threadlane_hub::RETRY_TASK,
            CANCEL_TASK,
            LIST_TASKS,
            threadlane_hub::PR_REVIEW_STATUS,
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
        ]
    );
    assert_eq!(assignment.max_turns, Some(150));

    assert_eq!(hub.event(accepted.event_id).await.status, EventStatus::Done);
    let run = hub.db.run(assignment.run_id).await.unwrap().unwrap();
    assert_eq!(run.status, RunStatus::Committed);
    assert_eq!(run.outcome, Some(RunOutcome::Sent));
    assert_eq!(run.rationale.as_deref(), Some(RATIONALE));
    assert_eq!(hub.general_lane().await.lease_owner, None, "released");

    assert!(
        hub.feed().await.is_empty(),
        "nothing is in the feed before dispatch"
    );
    assert_eq!(hub.core.dispatch_once().await.unwrap(), 1);
    let feed = hub.feed().await;
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].seq, 1);
    assert_eq!(feed[0].project_name.as_deref(), Some("General"));
    assert_eq!(feed[0].message.kind, OutboxKind::Message);
    assert_eq!(feed[0].message.body, json!({ "text": "Yes, all green." }));
    assert_eq!(feed[0].message.run_id, Some(assignment.run_id));
    assert_eq!(feed[0].delivered_at, hub.clock.now());
    assert_eq!(hub.core.dispatch_once().await.unwrap(), 0, "delivered once");
}

#[tokio::test]
async fn a_run_that_calls_no_tool_is_silent() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let accepted = hub.send("m-1", "fyi, nothing to do").await;
    let played = play(&*hub.link, Reply::Silent).await.unwrap();
    assert_eq!(played.runs.len(), 1);
    assert_eq!(played.runs[0].1, RunOutcome::Silent);
    let run = hub.db.run(played.runs[0].0.run_id).await.unwrap().unwrap();
    assert_eq!(run.rationale.as_deref(), Some(RATIONALE));
    assert_eq!(hub.event(accepted.event_id).await.status, EventStatus::Done);
    assert_eq!(hub.core.dispatch_once().await.unwrap(), 0);
    assert!(hub.feed().await.is_empty());
}

#[tokio::test]
async fn three_messages_before_the_claim_become_one_run() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let texts = ["first thought", "second thought", "third thought"];
    let mut ids = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        ids.push(hub.send(&format!("m-{index}"), text).await.event_id);
    }
    let (lease, assignment) = begin(&*hub.link).await;
    assert_eq!(assignment.events, ids);
    let positions: Vec<usize> = texts
        .iter()
        .map(|text| {
            assignment
                .prompt
                .find(text)
                .expect("every message is in the prompt")
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "oldest first"
    );
    hub.link
        .call_tool(
            &lease,
            assignment.run_id,
            Some("toolu_1"),
            SEND_MESSAGE,
            &json!({ "text": "One answer to all three." }),
        )
        .await
        .unwrap();
    assert_eq!(
        hub.link
            .complete(&lease, assignment.run_id, None)
            .await
            .unwrap(),
        RunOutcome::Sent
    );
    assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
    hub.link.release(&lease).await.unwrap();
    for id in ids {
        assert_eq!(hub.event(id).await.status, EventStatus::Done);
    }
    hub.core.dispatch_once().await.unwrap();
    assert_eq!(hub.feed().await.len(), 1);
}

#[tokio::test]
async fn events_nobody_asked_for_are_skipped_without_a_run() {
    // Under On request, the lead's default mode; Assisted and Autonomous
    // run on them (`initiative`).
    let hub = TestHub::start().await;
    hub.hello().await;
    let mut ids = Vec::new();
    for n in 0..2 {
        ids.push(
            hub.insert(|event| {
                event.source = "github".into();
                event.source_event_id = format!("gh-{n}");
                event.kind = EventKind::OutsideEvent;
                event.trust = Trust::External;
                event.requested = false;
            })
            .await,
        );
    }
    let lease = hub.link.request_work().await.unwrap().expect("granted");
    assert_eq!(hub.link.begin_run(&lease).await.unwrap(), Begin::Idle);
    hub.link.release(&lease).await.unwrap();
    for id in ids {
        let event = hub.event(id).await;
        assert_eq!(event.status, EventStatus::Skipped);
        assert_eq!(event.status_reason.as_deref(), Some(SKIP_REASON));
    }
    assert!(
        hub.db
            .run(threadlane_core::ids::RunId(1))
            .await
            .unwrap()
            .is_none()
    );
    let lane = hub.general().await.lead_lane_id;
    assert!(hub.db.pending_events(lane).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_mix_of_requested_and_unrequested_events_runs() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let unrequested = hub
        .insert(|event| {
            event.source_event_id = "quiet".into();
            event.requested = false;
        })
        .await;
    let requested = hub.send("m-1", "and what about this?").await.event_id;
    let played = play(&*hub.link, Reply::Silent).await.unwrap();
    assert_eq!(played.only_run().events, vec![unrequested, requested]);
    assert_eq!(hub.event(unrequested).await.status, EventStatus::Done);
    assert_eq!(hub.event(requested).await.status, EventStatus::Done);
}

#[tokio::test]
async fn a_lead_that_calls_stay_silent_sends_nothing_and_keeps_its_reason() {
    let hub = TestHub::start().await;
    hub.hello().await;
    hub.send("m-1", "fyi, the build is green").await;
    let (lease, run) = begin(&*hub.link).await;
    hub.link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_1"),
            STAY_SILENT,
            &json!({ "reason": "Only an FYI." }),
        )
        .await
        .unwrap();
    let empty = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_2"),
            STAY_SILENT,
            &json!({ "reason": " " }),
        )
        .await;
    assert!(
        matches!(empty, Err(ToolError::InvalidArguments(_))),
        "{empty:?}"
    );
    let outcome = hub
        .link
        .complete(&lease, run.run_id, Some("I sent nothing: it was an FYI."))
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Silent);
    let record = hub.db.run(run.run_id).await.unwrap().unwrap();
    assert_eq!(record.rationale.as_deref(), Some("Only an FYI."));
    assert_eq!(hub.core.dispatch_once().await.unwrap(), 0);
    assert!(hub.feed().await.is_empty());
}

#[tokio::test]
async fn a_routine_or_github_wake_ending_in_text_is_silent_and_keeps_the_text() {
    for kind in [EventKind::TimerFired, EventKind::OutsideEvent] {
        let hub = TestHub::start().await;
        hub.hello().await;
        hub.insert(|event| event.kind = kind).await;
        let (lease, run) = begin(&*hub.link).await;
        let text = "Nothing changed, so per the routine I stay silent.";
        let outcome = hub
            .link
            .complete(&lease, run.run_id, Some(text))
            .await
            .unwrap();
        assert_eq!(outcome, RunOutcome::Silent, "{kind}");
        let record = hub.db.run(run.run_id).await.unwrap().unwrap();
        assert_eq!(record.rationale.as_deref(), Some(text), "{kind}");
        assert_eq!(hub.core.dispatch_once().await.unwrap(), 0, "{kind}");
        assert!(hub.feed().await.is_empty(), "{kind}");
    }
}

#[tokio::test]
async fn a_lead_reply_to_the_owners_answer_is_delivered_once() {
    // The owner answering ask_user is a reply; a decision that expires is not.
    for (trust, delivered) in [(Trust::User, 1), (Trust::System, 0)] {
        let hub = TestHub::start().await;
        hub.hello().await;
        hub.insert(|event| {
            event.kind = EventKind::DecisionAnswer;
            event.trust = trust;
            event.payload = json!({ "text": "Use the backend repo." });
        })
        .await;
        let (lease, run) = begin(&*hub.link).await;
        let text = "Got it, created T-12 for Backend.";
        let outcome = hub
            .link
            .complete(&lease, run.run_id, Some(text))
            .await
            .unwrap();
        let expected = if delivered == 1 {
            RunOutcome::Sent
        } else {
            RunOutcome::Silent
        };
        assert_eq!(outcome, expected, "{trust:?}");
        assert_eq!(
            hub.core.dispatch_once().await.unwrap(),
            delivered,
            "{trust:?}"
        );
        assert_eq!(
            hub.core.dispatch_once().await.unwrap(),
            0,
            "once, {trust:?}"
        );
        let feed = hub.feed().await;
        assert_eq!(feed.len(), delivered as usize, "{trust:?}");
        if delivered == 1 {
            assert_eq!(feed[0].message.body, json!({ "text": text }));
        }
    }
}

#[tokio::test]
async fn a_lead_run_that_waited_on_you_never_delivers_its_final_text() {
    use threadlane_core::client::{DecisionReply, DecisionRequest};
    use threadlane_core::ids::DecisionId;
    // Answered before the run commits, the ask no longer holds the run as
    // deferred; neither the runner's stop notice nor the lead's text may reach you.
    for text in [
        "Stopped by the runner: the run went on working after a call of it waited on the user.",
        "I asked which repo; nothing more to say until you answer.",
    ] {
        let hub = TestHub::start().await;
        hub.hello().await;
        hub.send("m-1", "Set up the docs site").await;
        let (lease, run) = begin(&*hub.link).await;
        let asked = hub
            .link
            .call_tool(
                &lease,
                run.run_id,
                Some("toolu_ask"),
                ASK_USER,
                &json!({ "question": "Which repo?", "options": ["docs", "app"] }),
            )
            .await;
        assert_eq!(asked, Err(ToolError::WaitingOnYou));
        let answered = hub
            .api
            .decisions(DecisionRequest::Answer {
                decision: DecisionId(1),
                option: "docs".to_owned(),
                note: None,
            })
            .await
            .unwrap();
        assert!(matches!(answered, DecisionReply::Answered { .. }));
        let outcome = hub
            .link
            .complete(&lease, run.run_id, Some(text))
            .await
            .unwrap();
        assert_ne!(outcome, RunOutcome::Sent, "{text}");
        let record = hub.db.run(run.run_id).await.unwrap().unwrap();
        assert_eq!(
            record.rationale.as_deref(),
            Some(text),
            "kept for the run detail"
        );
        hub.core.dispatch_once().await.unwrap();
        assert!(
            hub.feed()
                .await
                .iter()
                .all(|entry| entry.message.kind != OutboxKind::Message),
            "{text}: {:?}",
            hub.feed().await
        );
    }
}
