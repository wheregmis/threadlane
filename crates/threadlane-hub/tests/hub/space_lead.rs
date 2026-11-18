//! The space lead (docs/one-place-to-talk.md stage 1): the General lead's
//! space summary, `ask_project_lead` and its gate and refusals, replies
//! routed back, and invariant 12.

use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::client::{DecisionReply, DecisionRequest, OrgReply, OrgRequest, UserMessage};
use threadlane_core::link::{Begin, HubLink, RunAssignment};
use threadlane_core::model::{Event, EventKind, LaneKind, Lease, Mode, Project, Trust};
use threadlane_core::store::{InboxStore, OrgStore, TimerStore};
use threadlane_core::time::Clock as _;
use threadlane_core::tools::ToolError;
use threadlane_hub::{
    ASK_PROJECT_LEAD, ASK_USER, CREATE_TASK, MEMORY_CREATE, MEMORY_VIEW, PROJECT_LEAD_SOURCE,
    SEND_MESSAGE, SET_FOLLOW_UP, SPACE_LEAD_SOURCE,
};

use crate::common::{TestHub, begin};

async fn create(hub: &TestHub, name: &str, description: &str) -> Project {
    let OrgReply::Project { project } = hub
        .api
        .org(OrgRequest::CreateProject {
            name: name.into(),
            description: description.into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected a project");
    };
    hub.db.project(project.id).await.unwrap().unwrap()
}

async fn send_to(hub: &TestHub, project: &str, id: &str, text: &str) {
    hub.api
        .send_message(&UserMessage {
            project: Some(project.into()),
            text: text.into(),
            images: Vec::new(),
            client_message_id: id.into(),
            task: None,
        })
        .await
        .unwrap();
}

/// One run in progress on the only ready lane.
struct Running<'a> {
    link: &'a dyn HubLink,
    lease: Lease,
    run: RunAssignment,
}

impl<'a> Running<'a> {
    async fn start(hub: &'a TestHub) -> Self {
        let (lease, run) = begin(&*hub.link).await;
        Self {
            link: &*hub.link,
            lease,
            run,
        }
    }

    async fn call(&self, call_id: &str, tool: &str, arguments: Json) -> Result<Json, ToolError> {
        self.link
            .call_tool(
                &self.lease,
                self.run.run_id,
                Some(call_id),
                tool,
                &arguments,
            )
            .await
    }

    fn offers(&self, tool: &str) -> bool {
        self.run
            .tools
            .iter()
            .any(|definition| definition.name == tool)
    }

    async fn end(self) {
        self.link
            .complete(&self.lease, self.run.run_id, None)
            .await
            .unwrap();
        assert_eq!(self.link.begin_run(&self.lease).await.unwrap(), Begin::Idle);
        self.link.release(&self.lease).await.unwrap();
    }
}

async fn pending(hub: &TestHub, project: &Project) -> Vec<Event> {
    hub.db.pending_events(project.lead_lane_id).await.unwrap()
}

fn ask(project: &str) -> Json {
    json!({ "project": project, "message": "Write the API docs." })
}

#[tokio::test]
async fn the_general_lead_sees_the_space_byte_stable_while_nothing_changes() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let api = create(&hub, "API", "The public\nAPI").await;
    create(&hub, "Old", "").await;
    hub.api
        .org(OrgRequest::SetArchived {
            project: Some("Old".into()),
            archived: true,
        })
        .await
        .unwrap();

    hub.send("m-1", "first").await;
    let first = Running::start(&hub).await;
    assert!(first.offers(ASK_PROJECT_LEAD));
    let summary = first.run.appended_instructions.clone();
    first.end().await;
    assert!(summary.contains("You are the space lead."), "{summary}");
    assert!(
        summary.contains("- API: The public API\n  No open tasks."),
        "{summary}"
    );
    assert!(
        summary.contains("- Old (archived; cannot be asked)"),
        "{summary}"
    );
    assert!(!summary.contains("- General"), "{summary}");

    hub.send("m-2", "second").await;
    let second = Running::start(&hub).await;
    assert_eq!(
        second.run.appended_instructions.as_bytes(),
        summary.as_bytes()
    );
    second.end().await;

    // The API lead answers you, and starts a task: both show.
    send_to(&hub, "API", "m-3", "Plan the docs").await;
    let lead = Running::start(&hub).await;
    assert_eq!(lead.run.project_id, api.id);
    assert!(!lead.offers(ASK_PROJECT_LEAD), "only the General lead asks");
    assert!(
        !lead
            .run
            .appended_instructions
            .contains("You are the space lead.")
    );
    lead.call("s1", SEND_MESSAGE, json!({ "text": "Planning\nthe docs." }))
        .await
        .unwrap();
    lead.call(
        "c1",
        CREATE_TASK,
        json!({ "title": "Draft docs", "goal": "A draft" }),
    )
    .await
    .unwrap();
    lead.end().await;
    hub.core.dispatch_once().await.unwrap();
    // Paused, so its task's lane is not granted ahead of General's.
    hub.api
        .org(OrgRequest::SetPaused {
            project: Some("API".into()),
            paused: true,
        })
        .await
        .unwrap();

    hub.send("m-4", "third").await;
    let third = Running::start(&hub).await;
    let changed = &third.run.appended_instructions;
    assert!(
        changed.contains("- API: The public API (paused)"),
        "{changed}"
    );
    assert!(
        changed.contains("  Open tasks:\n  - T-0001 Draft docs (queued)"),
        "{changed}"
    );
    assert!(
        changed.contains("  Its lead's latest message: Planning the docs."),
        "{changed}"
    );
}

#[tokio::test]
async fn an_ask_reaches_the_project_lead_requested_when_the_asking_run_was() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let api = create(&hub, "API", "").await;
    hub.send("m-1", "Get the docs written").await;
    let general = Running::start(&hub).await;
    let result = general
        .call("a1", ASK_PROJECT_LEAD, ask("API"))
        .await
        .unwrap();
    // Repeating the call inserts nothing more.
    let again = general
        .call("a1", ASK_PROJECT_LEAD, ask("API"))
        .await
        .unwrap();
    assert_eq!(result, again);
    general.end().await;

    let events = pending(&hub, &api).await;
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.source, SPACE_LEAD_SOURCE);
    assert_eq!(event.kind, EventKind::UserMessage);
    assert_eq!(event.trust, Trust::System);
    assert!(event.requested);
    assert!(
        event.payload["text"]
            .as_str()
            .unwrap()
            .contains("Write the API docs.")
    );
    assert_eq!(result["event_id"], json!(event.id));
}

#[tokio::test]
async fn an_ask_from_unrequested_work_is_not_requested() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let api = create(&hub, "API", "").await;
    let mut space = hub.db.space().await.unwrap();
    space.mode_ceiling = Mode::Assisted;
    hub.db.update_space(&space).await.unwrap();
    let binding = hub
        .db
        .lead_binding(hub.general().await.id)
        .await
        .unwrap()
        .unwrap();
    hub.db
        .set_binding_mode(binding.id, Mode::Assisted)
        .await
        .unwrap();
    hub.insert(|event| event.requested = false).await;
    let general = Running::start(&hub).await;
    general
        .call("a1", ASK_PROJECT_LEAD, ask("API"))
        .await
        .unwrap();
    general.end().await;
    let events = pending(&hub, &api).await;
    assert!(!events[0].requested);
}

#[tokio::test]
async fn an_unknown_archived_or_general_project_is_refused_and_others_cannot_ask() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let api = create(&hub, "API", "").await;
    create(&hub, "Old", "").await;
    hub.api
        .org(OrgRequest::SetArchived {
            project: Some("Old".into()),
            archived: true,
        })
        .await
        .unwrap();
    hub.send("m-1", "Ask around").await;
    let general = Running::start(&hub).await;
    for (n, name) in ["Nope", "Old", "General"].into_iter().enumerate() {
        let refused = general
            .call(&format!("a{n}"), ASK_PROJECT_LEAD, ask(name))
            .await;
        assert!(
            matches!(refused, Err(ToolError::InvalidArguments(_))),
            "{name}: {refused:?}"
        );
    }
    general.end().await;

    // A project lead named a tool it was never offered.
    send_to(&hub, "API", "m-2", "Ask General").await;
    let lead = Running::start(&hub).await;
    assert_eq!(lead.run.project_id, api.id);
    let refused = lead.call("a9", ASK_PROJECT_LEAD, ask("API")).await;
    assert!(matches!(refused, Err(ToolError::Blocked(_))), "{refused:?}");
    lead.end().await;
}

#[tokio::test]
async fn only_replies_to_an_ask_reach_the_general_lane() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let api = create(&hub, "API", "").await;
    let general = hub.general().await;

    // A message to the API lead from you is not routed.
    send_to(&hub, "API", "m-1", "Hello API").await;
    let direct = Running::start(&hub).await;
    direct
        .call("s1", SEND_MESSAGE, json!({ "text": "Hi you." }))
        .await
        .unwrap();
    direct.end().await;
    assert!(pending(&hub, &general).await.is_empty());

    hub.send("m-2", "Get the docs written").await;
    let asking = Running::start(&hub).await;
    asking
        .call("a1", ASK_PROJECT_LEAD, ask("API"))
        .await
        .unwrap();
    asking.end().await;

    let answering = Running::start(&hub).await;
    assert_eq!(answering.run.project_id, api.id);
    assert!(
        answering
            .run
            .prompt
            .contains("user_message from space_lead")
    );
    answering
        .call("s2", SEND_MESSAGE, json!({ "text": "On it." }))
        .await
        .unwrap();
    answering.end().await;

    let routed = pending(&hub, &general).await;
    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0].source, PROJECT_LEAD_SOURCE);
    assert_eq!(routed[0].kind, EventKind::System);
    assert!(routed[0].requested);
    let text = routed[0].payload["text"].as_str().unwrap();
    assert!(
        text.starts_with("The lead of API replied") && text.contains("On it."),
        "{text}"
    );
}

#[tokio::test]
async fn overlapping_asks_keep_their_own_clarifications_and_follow_ups() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let api = create(&hub, "API", "").await;
    let general = hub.general().await;
    hub.send("owner", "Send two requests to API").await;
    let lead = Running::start(&hub).await;
    for (call, message) in [("first", "Check forms"), ("second", "Check docs")] {
        lead.call(
            call,
            ASK_PROJECT_LEAD,
            json!({ "project": "API", "message": message }),
        )
        .await
        .unwrap();
    }
    lead.end().await;
    let asks = pending(&hub, &api).await;
    let [first, second] = asks.as_slice() else {
        panic!("expected two asks")
    };

    let clarifying = Running::start(&hub).await;
    assert_eq!(clarifying.run.events, [first.id]);
    assert_eq!(clarifying.run.space_lead_ask, Some(first.id));
    assert_eq!(
        clarifying
            .call(
                "clarify",
                ASK_USER,
                json!({
                    "question": "Which forms?", "options": ["Signup", "Checkout"],
                }),
            )
            .await,
        Err(ToolError::WaitingOnYou)
    );
    clarifying
        .link
        .complete(&clarifying.lease, clarifying.run.run_id, None)
        .await
        .unwrap();
    clarifying.link.release(&clarifying.lease).await.unwrap();

    let follow = Running::start(&hub).await;
    assert_eq!(follow.run.events, [second.id]);
    follow
        .call(
            "later",
            SET_FOLLOW_UP,
            json!({
                "in_minutes": 1, "instruction": "Check the docs",
            }),
        )
        .await
        .unwrap();
    follow.end().await;

    let DecisionReply::Pending { decisions } =
        hub.api.decisions(DecisionRequest::Pending).await.unwrap()
    else {
        panic!("expected a pending decision")
    };
    hub.api
        .decisions(DecisionRequest::Answer {
            decision: decisions[0].id,
            option: "Signup".into(),
            note: None,
        })
        .await
        .unwrap();
    let clarified = Running::start(&hub).await;
    assert_eq!(clarified.run.space_lead_ask, Some(first.id));
    clarified
        .call("reply", SEND_MESSAGE, json!({ "text": "Signup it is." }))
        .await
        .unwrap();
    clarified.end().await;
    let replies = pending(&hub, &general).await;
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].payload["ask_event_id"], json!(first.id));
    let report = Running::start(&hub).await;
    assert_eq!(report.run.project_id, general.id);
    report.end().await;

    hub.clock.advance(Duration::from_secs(60));
    let now = hub.clock.now();
    let timers = hub.db.due_timers(now).await.unwrap();
    assert_eq!(timers.len(), 1);
    let timer = &timers[0];
    hub.db
        .fire(timer.id, timer.due_at, None, now)
        .await
        .unwrap();
    let follow = Running::start(&hub).await;
    assert_eq!(follow.run.space_lead_ask, Some(second.id));
    follow
        .call(
            "follow-up-reply",
            SEND_MESSAGE,
            json!({ "text": "Docs checked." }),
        )
        .await
        .unwrap();
    follow.end().await;
    let replies = pending(&hub, &general).await;
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].payload["ask_event_id"], json!(second.id));
}

#[tokio::test]
async fn the_space_lead_never_reads_another_projects_memory() {
    let hub = TestHub::start().await;
    hub.hello().await;
    create(&hub, "API", "").await;
    const SECRET: &str = "The API's private note.";
    send_to(&hub, "API", "m-1", "Note this").await;
    let lead = Running::start(&hub).await;
    lead.call(
        "w1",
        MEMORY_CREATE,
        json!({ "path": "project/notes/secret.md", "content": format!("# Secret\nA note.\n{SECRET}\n") }),
    )
    .await
    .unwrap();
    lead.call(
        "w2",
        MEMORY_CREATE,
        json!({ "path": "project/instructions.md", "content": SECRET }),
    )
    .await
    .unwrap();
    lead.end().await;

    hub.send("m-2", "What does API know?").await;
    let general = Running::start(&hub).await;
    assert_eq!(general.run.lane_kind, LaneKind::Lead);
    assert!(!general.run.appended_instructions.contains(SECRET));
    assert!(!general.run.appended_instructions.contains("secret.md"));
    assert!(!general.run.prompt.contains(SECRET));
    let read = general
        .call(
            "r1",
            MEMORY_VIEW,
            json!({ "path": "project/notes/secret.md" }),
        )
        .await;
    assert!(
        !format!("{read:?}").contains(SECRET),
        "General's project/ is its own: {read:?}"
    );
    general.end().await;
}
