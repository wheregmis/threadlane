//! The gate's new outcome and decisions at the hub (Plan.md §9.4,
//! docs/milestone-3.md §4): asking opens one decision with its card, the run
//! ends deferred, the lane waits, an answer is stored once and arrives as an
//! event, and the stored call is handed to the runner only while it still
//! hashes to what you approved (invariant 7).

use std::sync::Arc;

use serde_json::{Value as Json, json};
use threadlane_core::client::{
    ApiError, ConversationEntry, DecisionReply, DecisionRequest, OverviewReply, OverviewRequest,
    UserMessage,
};
use threadlane_core::forge::{ATTACH_IMAGE, IMAGE_BLOB};
use threadlane_core::ids::{DecisionId, TaskId};
use threadlane_core::link::{Begin, PUBLISH_REVISION, Perform, RunAssignment};
use threadlane_core::model::{
    Decision, DecisionKind, DecisionOption, DecisionStatus, EventKind, LaneKind, Lease, Mode,
    NewAgent, NewBinding, NewDecision, NewRepository, OutboxKind, RunOutcome, Task, TaskStatus,
    Trust, Urgency,
};
use threadlane_core::runtime::{NoHooks, StepHook};
use threadlane_core::store::{
    DecisionStore, InboxStore, OrgStore, OutboxStore, SessionStore, TaskStore,
};
use threadlane_core::time::Clock;
use threadlane_core::tools::ToolError;
use threadlane_hub::{
    CREATE_TASK, FAST_FORWARD_PULL_REQUEST, HubSettings, OPEN_PULL_REQUEST, PUBLISH_BRANCH,
    derived_call_key,
};

use crate::common::{TestHub, begin};

const ENGINEER: &str = "Engineer";

fn pull_request_args() -> Json {
    json!({
        "repository": "app",
        "title": "Add the page",
        "body": "Adds the page.",
        PUBLISH_REVISION: "a".repeat(40),
    })
}

/// A hub with an engineer, the repositories `app` and `other` in General,
/// and a task in `app` whose first run is in progress.
async fn task_run() -> (TestHub, Lease, RunAssignment) {
    task_run_with(Arc::new(NoHooks)).await
}

/// [`task_run`] on a hub stopped by `hook`.
async fn task_run_with(hook: Arc<dyn StepHook>) -> (TestHub, Lease, RunAssignment) {
    let hub = TestHub::with(HubSettings::default(), hook).await;
    hub.hello().await;
    let general = hub.general().await;
    let agent = hub
        .db
        .create_agent(&NewAgent::named(ENGINEER, "engineer"))
        .await
        .unwrap();
    let binding = hub
        .db
        .bind(&NewBinding {
            project_id: general.id,
            agent_id: agent.id,
            mode: Some(Mode::OnRequest),
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    // These tests are about asking: an override turns off the default that
    // opens draft pull requests and pushes the task branch without asking.
    hub.db
        .set_binding_overrides(binding.id, &json!({"shared_reversible": "ask"}))
        .await
        .unwrap();
    for name in ["app", "other"] {
        hub.db
            .create_repository(&NewRepository {
                project_id: general.id,
                name: name.to_owned(),
                remote_url: format!("/remotes/{name}.git"),
                default_branch: "main".to_owned(),
                protected_branches: Vec::new(),
            })
            .await
            .unwrap();
    }
    hub.send("m-1", "Put the page up").await;
    let (lease, lead) = begin(&*hub.link).await;
    hub.link
        .call_tool(
            &lease,
            lead.run_id,
            Some("toolu_c"),
            CREATE_TASK,
            &json!({
                "title": "page",
                "goal": "the page is up",
                "agent": ENGINEER,
                "repositories": ["app"],
            }),
        )
        .await
        .unwrap();
    end(&hub, &lease, &lead).await;
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.lane_kind, LaneKind::Task);
    (hub, lease, run)
}

/// Commits the run and releases the lane; returns the outcome.
async fn end(hub: &TestHub, lease: &Lease, run: &RunAssignment) -> RunOutcome {
    let outcome = hub.link.complete(lease, run.run_id, None).await.unwrap();
    assert_eq!(hub.link.begin_run(lease).await.unwrap(), Begin::Idle);
    hub.link.release(lease).await.unwrap();
    outcome
}

async fn task(hub: &TestHub) -> Task {
    hub.db.task(TaskId(1)).await.unwrap().unwrap()
}

async fn decision(hub: &TestHub, id: i64) -> Decision {
    hub.db.decision(DecisionId(id)).await.unwrap().unwrap()
}

async fn answer(hub: &TestHub, id: i64, option: &str, note: Option<&str>) -> (Decision, bool) {
    match hub
        .api
        .decisions(DecisionRequest::Answer {
            decision: DecisionId(id),
            option: option.to_owned(),
            note: note.map(str::to_owned),
        })
        .await
        .unwrap()
    {
        DecisionReply::Answered { decision, recorded } => (*decision, recorded),
        other => panic!("not an answer: {other:?}"),
    }
}

/// The decision cards in the outbox.
async fn cards(hub: &TestHub) -> Vec<threadlane_core::model::OutboxMessage> {
    let mut cards = Vec::new();
    for id in 1.. {
        match hub
            .db
            .message(threadlane_core::ids::OutboxId(id))
            .await
            .unwrap()
        {
            Some(message) if message.kind == OutboxKind::Decision => cards.push(message),
            Some(_) => {}
            None => return cards,
        }
    }
    unreachable!()
}

/// The task run asks to open a pull request, and ends.
async fn asked() -> TestHub {
    asked_with(Arc::new(NoHooks)).await
}

async fn asked_with(hook: Arc<dyn StepHook>) -> TestHub {
    let (hub, lease, run) = task_run_with(hook).await;
    let asked = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_pr"),
            OPEN_PULL_REQUEST,
            &pull_request_args(),
        )
        .await;
    assert_eq!(asked, Err(ToolError::WaitingOnYou));
    assert_eq!(end(&hub, &lease, &run).await, RunOutcome::Deferred);
    hub
}

/// [`asked`], then you approve, on a hub stopped by `hook`.
pub async fn approved(hook: Arc<dyn StepHook>) -> TestHub {
    let hub = asked_with(hook).await;
    assert!(answer(&hub, 1, "approve", None).await.1);
    hub
}

#[tokio::test]
async fn asking_opens_one_decision_and_one_card_and_the_run_ends_deferred() {
    let (hub, lease, run) = task_run().await;
    for _ in 0..2 {
        let asked = hub
            .link
            .call_tool(
                &lease,
                run.run_id,
                Some("toolu_pr"),
                OPEN_PULL_REQUEST,
                &pull_request_args(),
            )
            .await;
        assert_eq!(asked, Err(ToolError::WaitingOnYou), "a repeat replays");
    }
    let pending = hub
        .db
        .pending_decisions(run_lane(&hub).await)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "one decision for one call");
    let opened = &pending[0];
    assert_eq!(opened.id.to_string(), "D-0001");
    assert_eq!(opened.kind, DecisionKind::Approval);
    assert_eq!(opened.tool, OPEN_PULL_REQUEST);
    assert_eq!(opened.arguments, pull_request_args());
    assert_eq!(
        opened.arguments_hash,
        derived_call_key(OPEN_PULL_REQUEST, &pull_request_args())
    );
    assert_eq!(opened.run_id, Some(run.run_id));
    assert_eq!(opened.session_id, Some(run.session_id));
    assert_eq!(opened.task_id, Some(TaskId(1)));
    let labels: Vec<&str> = opened.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, ["approve", "decline"]);
    assert_eq!(opened.recommended, Some(0));
    assert!(
        opened.question.contains("threadlane/T-0001") && opened.question.contains("Add the page"),
        "{}",
        opened.question
    );

    let cards = cards(&hub).await;
    assert_eq!(cards.len(), 1, "one card per decision");
    assert_eq!(cards[0].decision_id, Some(opened.id));
    assert_eq!(cards[0].channel, "client");
    assert_eq!(cards[0].urgency, Urgency::High);
    assert_eq!(cards[0].body["decision"], json!("D-0001"));
    assert_eq!(cards[0].body["recommended"], json!("approve"));
    assert_eq!(cards[0].body["options"], json!(["approve", "decline"]));

    // The run ends deferred; the task waits on you, and so does its lane.
    assert_eq!(end(&hub, &lease, &run).await, RunOutcome::Deferred);
    let waiting = task(&hub).await;
    assert_eq!(waiting.status, TaskStatus::WaitingOnYou);
    assert_eq!(
        waiting.waiting_reason.as_deref(),
        Some("waiting on your answer to D-0001")
    );
    hub.db
        .insert_event(&threadlane_core::model::NewEvent {
            project_id: waiting.project_id,
            lane_id: waiting.lane_id,
            source: "test".to_owned(),
            source_event_id: "later".to_owned(),
            kind: EventKind::OutsideEvent,
            payload: json!({ "text": "something else" }),
            trust: Trust::System,
            requested: true,
            session_ref: None,
            decision_ref: None,
            reply_to_ref: None,
            received_at: hub.clock.now(),
        })
        .await
        .unwrap();
    assert!(
        !hub.db
            .ready_lanes(hub.clock.now())
            .await
            .unwrap()
            .contains(&waiting.lane_id),
        "other events wait while the decision is open"
    );
    assert_eq!(hub.link.request_work().await.unwrap(), None);
    assert_eq!(task(&hub).await.status, TaskStatus::WaitingOnYou);
}

async fn run_lane(hub: &TestHub) -> threadlane_core::ids::LaneId {
    task(hub).await.lane_id
}

#[tokio::test]
async fn an_answer_is_stored_once_and_arrives_in_the_lane_as_requested_work() {
    let hub = asked().await;
    let listed = hub.api.decisions(DecisionRequest::Pending).await.unwrap();
    let DecisionReply::Pending { decisions } = listed else {
        panic!("{listed:?}");
    };
    assert_eq!(decisions.len(), 1);

    let refused = hub
        .api
        .decisions(DecisionRequest::Answer {
            decision: DecisionId(1),
            option: "maybe".to_owned(),
            note: None,
        })
        .await;
    assert!(refused.is_err(), "an unknown option is refused");

    let (answered, recorded) = answer(&hub, 1, "Approve", Some("go ahead")).await;
    assert!(recorded);
    assert_eq!(answered.status, DecisionStatus::Answered);
    assert!(answered.approved());
    let (again, recorded) = answer(&hub, 1, "decline", None).await;
    assert!(!recorded, "a second answer is ignored");
    assert!(again.approved(), "the first answer stands");
    let DecisionReply::Pending { decisions } =
        hub.api.decisions(DecisionRequest::Pending).await.unwrap()
    else {
        panic!("not a list");
    };
    assert!(decisions.is_empty());

    let queued = task(&hub).await;
    assert_eq!(queued.status, TaskStatus::Queued);
    assert_eq!(queued.waiting_reason, None);
    let answers: Vec<_> = hub
        .db
        .pending_events(queued.lane_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == EventKind::DecisionAnswer)
        .collect();
    assert_eq!(answers.len(), 1, "one answer event");
    assert!(answers[0].requested && answers[0].trust == Trust::User);

    // The next run resumes the session that asked and is told to perform
    // the stored call.
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.perform, [DecisionId(1)]);
    assert_eq!(Some(run.session_id), decision(&hub, 1).await.session_id);
    assert!(
        run.prompt.contains("You approved D-0001") && run.prompt.contains("go ahead"),
        "{}",
        run.prompt
    );
    let Perform::Call {
        tool,
        arguments,
        retry,
    } = hub
        .link
        .begin_perform(&lease, run.run_id, DecisionId(1))
        .await
        .unwrap()
    else {
        panic!("the call is handed over");
    };
    assert_eq!(
        (tool.as_str(), arguments, retry),
        (OPEN_PULL_REQUEST, pull_request_args(), false)
    );
    // Asked again before an outcome: the intent was recorded.
    assert!(matches!(
        hub.link
            .begin_perform(&lease, run.run_id, DecisionId(1))
            .await
            .unwrap(),
        Perform::Call { retry: true, .. }
    ));
    let outcome = json!({ "performed": true, "branch": "threadlane/T-0001", "pull_request": "u" });
    hub.link
        .finish_perform(&lease, run.run_id, DecisionId(1), &outcome)
        .await
        .unwrap();
    assert_eq!(
        hub.link
            .begin_perform(&lease, run.run_id, DecisionId(1))
            .await
            .unwrap(),
        Perform::Recorded {
            outcome: outcome.clone()
        }
    );

    // Finishing records the branch and pull request in the task's result.
    hub.link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_p"),
            threadlane_hub::MEMORY_CREATE,
            &json!({ "path": "project/tasks/T-0001.md", "content": "# T-0001\nOpened it." }),
        )
        .await
        .unwrap();
    hub.link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_f"),
            threadlane_hub::FINISH_TASK,
            &json!({ "summary": "Opened it." }),
        )
        .await
        .unwrap();
    end(&hub, &lease, &run).await;
    let done = task(&hub).await;
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(
        done.result,
        Some(json!({
            "summary": "Opened it.",
            "branch": "threadlane/T-0001",
            "pull_request": "u",
        }))
    );
}

#[tokio::test]
async fn a_message_from_you_in_a_task_lane_declines_its_open_decision() {
    let hub = asked().await;
    let waiting = task(&hub).await;
    // Sent to the task, not the lead (wireframe 5, note 5).
    let accepted = hub
        .api
        .send_message(&UserMessage {
            project: Some("ignored for a task".to_owned()),
            text: "Not yet, add tests first.".to_owned(),
            images: Vec::new(),
            client_message_id: "m-2".to_owned(),
            task: Some(waiting.id),
        })
        .await
        .unwrap();
    assert_eq!(accepted.project_id, waiting.project_id);
    let event = hub.db.event(accepted.event_id).await.unwrap().unwrap();
    assert_eq!(
        (event.lane_id, event.kind, event.trust),
        (waiting.lane_id, EventKind::UserMessage, Trust::User)
    );
    match hub
        .api
        .overview(OverviewRequest::Task { task: waiting.id })
        .await
        .unwrap()
    {
        OverviewReply::Task { task } => assert_eq!(
            task.messages,
            [ConversationEntry::You {
                event_id: accepted.event_id,
                text: "Not yet, add tests first.".to_owned(),
                images: Vec::new(),
                at: event.received_at,
            }]
        ),
        other => panic!("not a task: {other:?}"),
    }
    let declined = decision(&hub, 1).await;
    assert_eq!(declined.status, DecisionStatus::Answered);
    assert!(!declined.approved());
    assert_eq!(
        declined.answer.as_ref().unwrap()["note"],
        json!("Not yet, add tests first.")
    );
    assert_eq!(task(&hub).await.status, TaskStatus::Queued);

    let (lease, run) = begin(&*hub.link).await;
    assert!(run.perform.is_empty(), "nothing to perform");
    assert!(
        run.prompt.contains("You declined D-0001") && run.prompt.contains("add tests first"),
        "{}",
        run.prompt
    );
    assert!(
        hub.link
            .begin_perform(&lease, run.run_id, DecisionId(1))
            .await
            .is_err(),
        "a declined call is never handed over"
    );
    // Declining is information, not failure.
    let events = hub.db.run_events(run.run_id).await.unwrap();
    assert!(events.iter().all(|event| event.attempts == 0));
}

#[tokio::test]
async fn a_message_to_a_task_that_is_unknown_or_has_ended_is_refused() {
    let hub = asked().await;
    let message = |task: i64| UserMessage {
        project: None,
        text: "hello".to_owned(),
        images: Vec::new(),
        client_message_id: format!("m-{task}"),
        task: Some(TaskId(task)),
    };
    assert!(matches!(
        hub.api.send_message(&message(9)).await,
        Err(ApiError::NotFound(_))
    ));
    hub.db
        .cancel_task(None, TaskId(1), hub.clock.now())
        .await
        .unwrap();
    let refused = hub.api.send_message(&message(1)).await;
    assert!(
        matches!(&refused, Err(ApiError::Invalid(why)) if why.contains("has ended")),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_stored_call_that_no_longer_hashes_to_what_you_approved_is_not_handed_over() {
    let (hub, lease, run) = task_run().await;
    let lane = run_lane(&hub).await;
    // A call stored with other arguments than the ones its hash binds.
    let tampered = hub
        .db
        .open_decision(
            Some(&lease),
            &NewDecision {
                project_id: hub.general().await.id,
                lane_id: lane,
                task_id: Some(TaskId(1)),
                session_id: Some(run.session_id),
                run_id: Some(run.run_id),
                tool_call_id: Some("toolu_pr".to_owned()),
                tool: OPEN_PULL_REQUEST.to_owned(),
                arguments: json!({ "repository": "app", "title": "Something else" }),
                arguments_hash: derived_call_key(OPEN_PULL_REQUEST, &pull_request_args()),
                kind: DecisionKind::Approval,
                question: "Open a draft pull request?".to_owned(),
                context: String::new(),
                options: vec![
                    DecisionOption {
                        label: "approve".to_owned(),
                        effect: json!({ "approve": true }),
                    },
                    DecisionOption {
                        label: "decline".to_owned(),
                        effect: json!({ "approve": false }),
                    },
                ],
                recommended: Some(0),
                offers_standing_rule: false,
                expires_at: None,
                created_at: hub.clock.now(),
                urgent: true,
            },
        )
        .await
        .unwrap();
    end(&hub, &lease, &run).await;
    answer(&hub, tampered.id.get(), "approve", None).await;
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.perform, [tampered.id]);
    let Perform::Recorded { outcome } = hub
        .link
        .begin_perform(&lease, run.run_id, tampered.id)
        .await
        .unwrap()
    else {
        panic!("a changed call must not be handed over");
    };
    assert_eq!(outcome["performed"], json!(false));
    assert!(
        outcome["error"]
            .as_str()
            .unwrap()
            .contains("no longer match"),
        "{outcome}"
    );
    assert_eq!(decision(&hub, 1).await.outcome, Some(outcome));
}

/// Retargeting a pull request is asked about like any tier 2 change, and
/// onto a branch other than the default one it is yours alone: warned on
/// the card, with no standing rule offered (#788).
#[tokio::test]
async fn a_retarget_off_the_default_branch_is_yours_with_a_warning() {
    use threadlane_core::forge::RETARGET_PULL_REQUEST;
    let (hub, lease, run) = task_run().await;
    for (call, base) in [("toolu_main", "main"), ("toolu_v2", "v2")] {
        let asked = hub
            .link
            .call_tool(
                &lease,
                run.run_id,
                Some(call),
                RETARGET_PULL_REQUEST,
                &json!({ "repository": "app", "number": 7, "base": base }),
            )
            .await;
        assert_eq!(asked, Err(ToolError::WaitingOnYou), "{base}");
    }
    let onto_main = decision(&hub, 1).await;
    assert_eq!(onto_main.tool, RETARGET_PULL_REQUEST);
    assert!(
        onto_main
            .question
            .starts_with("Change the base of pull request #7 in app to main?"),
        "{}",
        onto_main.question
    );
    assert!(!onto_main.question.contains("Warning"));
    assert!(onto_main.offers_standing_rule);
    let onto_v2 = decision(&hub, 2).await;
    assert!(
        onto_v2
            .question
            .contains("Warning: v2 is not the repository's default branch main"),
        "{}",
        onto_v2.question
    );
    assert!(!onto_v2.offers_standing_rule);
}

#[tokio::test]
async fn the_gate_refuses_a_call_it_cannot_ask_about_and_opens_nothing() {
    let (hub, lease, run) = task_run().await;
    for (n, (tool, arguments)) in [
        (PUBLISH_BRANCH, json!({ "repository": "elsewhere" })),
        (PUBLISH_BRANCH, json!({ "repository": "other" })),
        (PUBLISH_BRANCH, json!({ "repository": "../app" })),
        (
            PUBLISH_BRANCH,
            json!({ "repository": "app", "branch": "main" }),
        ),
        (PUBLISH_BRANCH, json!({ "repository": "app" })),
        (FAST_FORWARD_PULL_REQUEST, json!({
            "repository": "app", "expected_head": "b".repeat(40), PUBLISH_REVISION: "a".repeat(40)
        })),
        (FAST_FORWARD_PULL_REQUEST, json!({
            "repository": "app", "number": 7, PUBLISH_REVISION: "a".repeat(40)
        })),
        (
            PUBLISH_BRANCH,
            json!({ "repository": "app", PUBLISH_REVISION: "HEAD" }),
        ),
        // onto_task: not the task itself, an unknown task, or a branch name.
        (
            PUBLISH_BRANCH,
            json!({ "repository": "app", "onto_task": "T-0001", PUBLISH_REVISION: "a".repeat(40) }),
        ),
        (
            PUBLISH_BRANCH,
            json!({ "repository": "app", "onto_task": "T-0099", PUBLISH_REVISION: "a".repeat(40) }),
        ),
        (
            PUBLISH_BRANCH,
            json!({ "repository": "app", "onto_task": "main", PUBLISH_REVISION: "a".repeat(40) }),
        ),
        (
            OPEN_PULL_REQUEST,
            json!({ "repository": "app", "title": "ok", "onto_task": "T-0002", PUBLISH_REVISION: "a".repeat(40) }),
        ),
        (
            OPEN_PULL_REQUEST,
            json!({ "repository": "app", PUBLISH_REVISION: "a".repeat(40) }),
        ),
        (
            OPEN_PULL_REQUEST,
            json!({ "repository": "app", "title": "--draft=false", PUBLISH_REVISION: "a".repeat(40) }),
        ),
        (
            OPEN_PULL_REQUEST,
            json!({ "repository": "app", "title": "two\nlines", PUBLISH_REVISION: "a".repeat(40) }),
        ),
        (
            OPEN_PULL_REQUEST,
            json!({ "repository": "app", "title": "ok", "body": "bell\u{7}", PUBLISH_REVISION: "a".repeat(40) }),
        ),
        // attach_image: no blob from the runner, another repository, a
        // caption that would break the Markdown, or an extra field.
        (ATTACH_IMAGE, json!({ "repository": "app", "number": 7, "path": "/s.png" })),
        (
            ATTACH_IMAGE,
            json!({ "repository": "other", "number": 7, "path": "/s.png", IMAGE_BLOB: "a".repeat(40) }),
        ),
        (
            ATTACH_IMAGE,
            json!({ "repository": "app", "number": 7, "path": "/s.png", "caption": "a](x", IMAGE_BLOB: "a".repeat(40) }),
        ),
        (
            ATTACH_IMAGE,
            json!({ "repository": "app", "number": 7, "path": "/s.png", "body": "x", IMAGE_BLOB: "a".repeat(40) }),
        ),
        // create_tag: a branch name for the commit, or a name git could misread.
        (
            threadlane_core::forge::CREATE_TAG,
            json!({ "repository": "app", "tag": "v1", "commit": "main" }),
        ),
        (
            threadlane_core::forge::CREATE_TAG,
            json!({ "repository": "app", "tag": "../v1", "commit": "a".repeat(40) }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let refused = hub
            .link
            .call_tool(
                &lease,
                run.run_id,
                Some(&format!("bad-{n}")),
                tool,
                &arguments,
            )
            .await;
        assert!(
            matches!(refused, Err(ToolError::InvalidArguments(_))),
            "{tool} {arguments}: {refused:?}"
        );
    }
    assert!(hub.db.open_decisions().await.unwrap().is_empty());
    assert!(cards(&hub).await.is_empty());
    assert_eq!(end(&hub, &lease, &run).await, RunOutcome::Silent);

    // The lead may not publish at all.
    hub.send("m-2", "Publish it yourself").await;
    let (lease, lead) = begin(&*hub.link).await;
    assert_eq!(lead.lane_kind, LaneKind::Lead);
    let refused = hub
        .link
        .call_tool(
            &lease,
            lead.run_id,
            Some("toolu_p"),
            PUBLISH_BRANCH,
            &json!({ "repository": "app" }),
        )
        .await;
    assert!(matches!(refused, Err(ToolError::Blocked(_))), "{refused:?}");
    assert!(hub.db.open_decisions().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_decision_reminder_is_claimed_once() {
    let hub = asked().await;
    for first in [true, false] {
        let reply = hub
            .api
            .decisions(DecisionRequest::Remind {
                decision: DecisionId(1),
            })
            .await
            .unwrap();
        assert_eq!(reply, DecisionReply::Reminder { first });
    }
}

#[tokio::test]
async fn an_image_attachment_is_asked_about_by_file_and_blob_and_handed_to_the_runner() {
    let (hub, lease, run) = task_run().await;
    let arguments = json!({
        "repository": "app",
        "number": 7,
        "path": "/work/shot.png",
        "caption": "Settings page",
        IMAGE_BLOB: "b".repeat(40),
    });
    let asked = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("toolu_img"),
            ATTACH_IMAGE,
            &arguments,
        )
        .await;
    assert_eq!(asked, Err(ToolError::WaitingOnYou));
    let opened = decision(&hub, 1).await;
    assert_eq!(opened.tool, ATTACH_IMAGE);
    assert_eq!(
        opened.question,
        "Attach image /work/shot.png to pull request #7 in app?"
    );
    assert!(
        opened.context.contains(&"b".repeat(40)) && opened.context.contains("\"Settings page\""),
        "{}",
        opened.context
    );
    assert_eq!(end(&hub, &lease, &run).await, RunOutcome::Deferred);
    answer(&hub, 1, "approve", None).await;
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.perform, [DecisionId(1)]);
    let Perform::Call {
        tool,
        arguments: handed,
        ..
    } = hub
        .link
        .begin_perform(&lease, run.run_id, DecisionId(1))
        .await
        .unwrap()
    else {
        panic!("the runner performs it in the task's worktree");
    };
    assert_eq!((tool.as_str(), handed), (ATTACH_IMAGE, arguments));
}
