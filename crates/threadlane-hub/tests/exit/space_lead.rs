//! docs/one-place-to-talk.md stage 1, the exit scenario: the owner asks
//! General for a goal that belongs to a project; the project's lead does it
//! and General reports back. The real hub and the real runner.

use std::time::Duration;

use serde_json::json;
use threadlane_core::client::{
    ConversationEntry, OrgReply, OrgRequest, OverviewReply, OverviewRequest,
};
use threadlane_core::model::{EventKind, Mode, NewAgent, NewBinding, TaskStatus};
use threadlane_core::store::OrgStore;
use threadlane_hub::{
    ASK_PROJECT_LEAD, CREATE_TASK, PROJECT_LEAD_SOURCE, SEND_MESSAGE, SPACE_LEAD_SOURCE,
};
use threadlane_testkit::{CrashPlan, Script};

use crate::harness::{ENGINEER, World, finish_after};

const GOAL: &str = "Get the API docs written";
const API_REPORT: &str = "The API docs are written.";
const GENERAL_REPORT: &str = "API's lead reports the docs are written.";

#[tokio::test]
async fn the_owner_asks_general_for_a_projects_goal_its_lead_does_it_and_general_reports_back() {
    let world = World::new().await;
    let sim = world.boot(CrashPlan::new()).await;
    let OrgReply::Project { project } = sim
        .api
        .org(OrgRequest::CreateProject {
            name: "API".into(),
            description: "The public API".into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected a project");
    };
    let engineer = sim
        .db()
        .create_agent(&NewAgent::named(ENGINEER, "engineer"))
        .await
        .unwrap();
    sim.db()
        .bind(&NewBinding {
            project_id: project.id,
            agent_id: engineer.id,
            mode: Mode::OnRequest,
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();

    let backend = sim.backend();
    // General, on the API lead's reply.
    backend.route(
        "replied to your ask",
        Script::new()
            .call("toolu_g2", SEND_MESSAGE, json!({ "text": GENERAL_REPORT }))
            .finish("Reported back."),
    );
    // The API lead, on its task's result.
    backend.route(
        "task_result",
        Script::new()
            .call("toolu_a2", SEND_MESSAGE, json!({ "text": API_REPORT }))
            .finish("Reported."),
    );
    // The API lead, on the space lead's ask.
    backend.route(
        "The space lead, the General project's lead, asks you",
        Script::new()
            .call(
                "toolu_a1",
                CREATE_TASK,
                json!({ "title": "Write the docs", "goal": "The docs exist", "agent": ENGINEER }),
            )
            .finish("Started a task."),
    );
    backend.route("Your task, ", finish_after(Duration::ZERO));
    // General, on the owner's message.
    backend.route(
        GOAL,
        Script::new()
            .call(
                "toolu_g1",
                ASK_PROJECT_LEAD,
                json!({ "project": "API", "message": "Write the API docs." }),
            )
            .finish("Handed to API's lead."),
    );

    sim.send("goal", GOAL).await;
    sim.work_until_idle().await.unwrap();

    let task = sim.task("T-0001").await;
    assert_eq!(task.project_id, project.id);
    assert_eq!(task.status, TaskStatus::Done);
    let events = sim.events().await;
    let ask = events
        .iter()
        .find(|event| event.source == SPACE_LEAD_SOURCE)
        .expect("the ask reached the API lead");
    let lead_lane = sim
        .db()
        .project(project.id)
        .await
        .unwrap()
        .unwrap()
        .lead_lane_id;
    assert_eq!(ask.lane_id, lead_lane);
    assert!(ask.requested, "the owner asked, so the ask is requested");
    let general = sim.db().general_project().await.unwrap();
    let reply = events
        .iter()
        .find(|event| event.source == PROJECT_LEAD_SOURCE)
        .expect("the reply reached General");
    assert_eq!(reply.lane_id, general.lead_lane_id);
    assert_eq!(reply.kind, EventKind::System);
    assert!(reply.requested);
    assert!(reply.payload["text"].as_str().unwrap().contains(API_REPORT));

    let sent: Vec<_> = sim
        .outbox()
        .await
        .into_iter()
        .map(|message| (message.project_id, message.body["text"].clone()))
        .collect();
    assert!(
        sent.contains(&(Some(project.id), json!(API_REPORT))),
        "{sent:?}"
    );
    assert!(
        sent.contains(&(Some(general.id), json!(GENERAL_REPORT))),
        "{sent:?}"
    );

    // Stage 2: the app's conversations show the ask as the space lead's,
    // and General's reply entry names the project it came from.
    let conversation = |project: Option<&str>| {
        let request = OverviewRequest::Conversation {
            project: project.map(str::to_owned),
            before: None,
            limit: 50,
        };
        let api = &sim.api;
        async move {
            match api.overview(request).await.unwrap() {
                OverviewReply::Conversation { entries } => entries,
                other => panic!("expected a conversation, got {other:?}"),
            }
        }
    };
    let in_api = conversation(Some("API")).await;
    assert!(
        in_api.iter().any(|entry| matches!(entry,
            ConversationEntry::SpaceLead { event_id, text, .. }
                if *event_id == ask.id && text == "Write the API docs.")),
        "{in_api:?}"
    );
    assert!(
        !in_api
            .iter()
            .any(|entry| matches!(entry, ConversationEntry::You { .. })),
        "the ask is not the owner's"
    );
    let in_general = conversation(None).await;
    assert!(
        in_general.iter().any(|entry| matches!(entry,
            ConversationEntry::ProjectReply { event_id, project, ask_event_id, text, .. }
                if *event_id == reply.id && project == "API"
                    && *ask_event_id == Some(ask.id) && text.contains(API_REPORT))),
        "{in_general:?}"
    );
}
