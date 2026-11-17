use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;
use threadlane_client::ClientDirs;
use threadlane_core::client::{
    Accepted, ApiError, DecisionReply, DecisionRequest, FeedItem, HubApi, HubInfo, OrgReply,
    OrgRequest, OverviewReply, OverviewRequest, PairingReply, PairingRequest, UserMessage,
};
use threadlane_core::ids::{LaneId, OutboxId};
use threadlane_core::model::{
    DecisionKind, DecisionOption, DecisionStatus, Mode, OutboxMessage, OutboxStatus, Urgency,
};
use threadlane_core::time::Timestamp;

use super::*;

/// A hub with one project, `api`, and the decisions it was given. While
/// `down`, it answers nothing.
#[derive(Debug)]
struct FakeHub {
    space: String,
    down: AtomicBool,
    decisions: Mutex<Vec<Decision>>,
}

impl FakeHub {
    fn new(space: &str, decisions: Vec<Decision>) -> Arc<Self> {
        Arc::new(Self {
            space: space.to_owned(),
            down: AtomicBool::new(false),
            decisions: Mutex::new(decisions),
        })
    }

    fn up(&self) -> Result<(), ApiError> {
        if self.down.load(Ordering::SeqCst) {
            Err(ApiError::Unreachable("connection refused".to_owned()))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl HubApi for FakeHub {
    async fn info(&self) -> Result<HubInfo, ApiError> {
        self.up()?;
        Ok(HubInfo {
            space: self.space.clone(),
            mode_ceiling: Mode::Assisted,
            schema_version: 1,
        })
    }

    async fn projects(&self) -> Result<Vec<ProjectSummary>, ApiError> {
        self.up()?;
        Ok(vec![ProjectSummary {
            id: ProjectId(1),
            name: "api".to_owned(),
            is_general: false,
            paused: false,
        }])
    }

    async fn send_message(&self, _message: &UserMessage) -> Result<Accepted, ApiError> {
        Err(ApiError::Invalid("unsupported".to_owned()))
    }

    async fn feed(&self, _after: i64, _wait: Duration) -> Result<Vec<FeedItem>, ApiError> {
        Err(ApiError::Invalid("unsupported".to_owned()))
    }

    async fn org(&self, _request: OrgRequest) -> Result<OrgReply, ApiError> {
        Err(ApiError::Invalid("unsupported".to_owned()))
    }

    async fn decisions(&self, request: DecisionRequest) -> Result<DecisionReply, ApiError> {
        self.up()?;
        let mut decisions = self.decisions.lock().expect("lock");
        match request {
            DecisionRequest::Pending => Ok(DecisionReply::Pending {
                decisions: decisions.clone(),
            }),
            DecisionRequest::Answer { decision: id, .. } => {
                let index = decisions
                    .iter()
                    .position(|decision| decision.id == id)
                    .ok_or_else(|| ApiError::NotFound(id.to_string()))?;
                let mut decision = decisions.remove(index);
                decision.status = DecisionStatus::Answered;
                Ok(DecisionReply::Answered {
                    decision: Box::new(decision),
                    recorded: true,
                })
            }
            other => Err(ApiError::Invalid(format!("unsupported {other:?}"))),
        }
    }

    async fn pairing(&self, _request: PairingRequest) -> Result<PairingReply, ApiError> {
        Err(ApiError::Invalid("unsupported".to_owned()))
    }
    async fn overview(&self, _request: OverviewRequest) -> Result<OverviewReply, ApiError> {
        Err(ApiError::Invalid("unsupported".to_owned()))
    }
}

fn decision(id: i64, created_at: i64) -> Decision {
    let option = |label: &str, approve: bool| DecisionOption {
        label: label.to_owned(),
        effect: json!({ "approve": approve }),
    };
    Decision {
        id: DecisionId(id),
        project_id: ProjectId(1),
        lane_id: LaneId(1),
        task_id: None,
        session_id: None,
        run_id: None,
        tool_call_id: None,
        tool: "open_pull_request".to_owned(),
        arguments: json!({}),
        arguments_hash: String::new(),
        kind: DecisionKind::Approval,
        question: format!("Question {id}?"),
        context: String::new(),
        options: vec![option("approve", true), option("decline", false)],
        recommended: Some(0),
        offers_standing_rule: false,
        status: DecisionStatus::Pending,
        answer: None,
        expires_at: None,
        created_at: Timestamp::from_millis(created_at),
        outcome: None,
    }
}

fn item(space: &str, seq: i64, kind: OutboxKind, body: Json) -> SpaceFeedItem {
    SpaceFeedItem {
        space: space.to_owned(),
        item: FeedItem {
            seq,
            message: OutboxMessage {
                id: OutboxId(seq),
                project_id: Some(ProjectId(1)),
                lane_id: None,
                session_id: None,
                run_id: None,
                tool_call_id: None,
                kind,
                decision_id: None,
                channel: "client".to_owned(),
                body,
                urgency: Urgency::Normal,
                status: OutboxStatus::Sent,
                not_before: None,
                attempts: 1,
                channel_message_id: None,
                created_at: Timestamp::from_millis(1_000),
                sent_at: Some(Timestamp::from_millis(2_000)),
                delivery_seq: Some(seq),
            },
            project_name: Some("api".to_owned()),
            delivered_at: Timestamp::from_millis(2_000),
        },
    }
}

/// A core with each `(name, hub)` paired for this process only.
fn core(hubs: &[(&str, &Arc<FakeHub>)]) -> (TempDir, ClientCore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut core = ClientCore::load(ClientDirs::new(dir.path())).expect("load");
    for (name, hub) in hubs {
        core = core.with_hub(name, Arc::clone(hub) as Arc<dyn HubApi>);
    }
    (dir, core)
}

fn queue(state: &AppState) -> Vec<(String, String)> {
    state
        .queue()
        .iter()
        .map(|queued| (queued.space.clone(), queued.decision.id.to_string()))
        .collect()
}

fn pair(space: &str, id: &str) -> (String, String) {
    (space.to_owned(), id.to_owned())
}

#[tokio::test]
async fn decisions_from_two_hubs_merge_oldest_first() {
    let work = FakeHub::new("Work", vec![decision(1, 3_000), decision(2, 1_000)]);
    let home = FakeHub::new("Home", vec![decision(1, 2_000)]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();

    state.apply(refresh(&core).await);

    assert_eq!(
        queue(&state),
        [
            pair("work", "D-0002"),
            pair("home", "D-0001"),
            pair("work", "D-0001")
        ]
    );
    let names: Vec<&str> = state.spaces().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["work", "home"]);
    assert!(state.spaces().iter().all(|s| s.unreachable.is_none()));
    assert_eq!(state.project_name("home", ProjectId(1)), Some("api"));
}

#[tokio::test]
async fn an_unreachable_hub_does_not_hide_the_others() {
    let work = FakeHub::new("Work", vec![decision(1, 1_000)]);
    let home = FakeHub::new("Home", vec![decision(1, 2_000)]);
    let (_dir, core) = core(&[("work", &work), ("home", &home)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);

    work.down.store(true, Ordering::SeqCst);
    home.decisions.lock().expect("lock").push(decision(2, 500));
    state.apply(refresh(&core).await);

    let work_space = &state.spaces()[0];
    assert!(work_space.unreachable.is_some(), "work is shown as away");
    assert_eq!(work_space.projects.len(), 1, "its last projects are kept");
    assert!(state.spaces()[1].unreachable.is_none());
    assert_eq!(
        queue(&state),
        [
            pair("home", "D-0002"),
            pair("work", "D-0001"),
            pair("home", "D-0001")
        ],
        "home's new decision shows; work's last known one stays"
    );
}

#[tokio::test]
async fn answering_removes_the_decision_from_the_queue() {
    let work = FakeHub::new("Work", vec![decision(1, 1_000), decision(2, 2_000)]);
    let (_dir, core) = core(&[("work", &work)]);
    let mut state = AppState::default();
    state.apply(refresh(&core).await);
    assert!(state.is_pending("work", "D-0001"));

    let answer = core
        .answer(Some("work"), "D-0001", "approve", Some("go ahead"))
        .await
        .expect("answer");
    state.answered(&answer);

    assert!(!state.is_pending("work", "D-0001"));
    assert_eq!(queue(&state), [pair("work", "D-0002")]);
    state.apply(refresh(&core).await);
    assert_eq!(queue(&state), [pair("work", "D-0002")], "the hub agrees");
}

#[test]
fn decisions_and_notices_raise_one_notification_and_messages_none() {
    let mut state = AppState::default();
    let card = item(
        "work",
        2,
        OutboxKind::Decision,
        json!({
            "text": "D-0003: Open a pull request? Answer with ...",
            "decision": "D-0003",
            "question": "Open a pull request?",
            "options": ["approve", "decline"],
        }),
    );

    assert_eq!(
        state.receive(item(
            "work",
            1,
            OutboxKind::Message,
            json!({ "text": "hi" })
        )),
        None
    );
    let alert = state.receive(card.clone()).expect("a decision notifies");
    assert_eq!(alert.tag(), "work/D-0003");
    assert_eq!(decision_of_tag(&alert.tag()), Some(("work", "D-0003")));
    assert_eq!(alert.body(), "Open a pull request?");
    assert_eq!(alert.options(), ["approve", "decline"]);
    let notice = state.receive(item(
        "work",
        3,
        OutboxKind::SystemNotice,
        json!({ "text": "the lead stopped" }),
    ));
    assert!(matches!(notice, Some(Alert::Notice { ref text, .. }) if text == "the lead stopped"));
    let notice = notice.expect("a notice notifies");
    assert_eq!(decision_of_tag(&notice.tag()), None, "nothing to answer");

    assert_eq!(
        state.receive(card),
        None,
        "a replayed card does not notify again"
    );
}

#[test]
fn an_item_seen_twice_is_shown_once() {
    let mut state = AppState::default();
    let first = item("work", 1, OutboxKind::Message, json!({ "text": "hi" }));
    state.receive(first.clone());
    state.receive(first);
    // The same seq from another space is another item.
    state.receive(item(
        "home",
        1,
        OutboxKind::Message,
        json!({ "text": "hi" }),
    ));

    assert_eq!(state.conversation("work", "api").count(), 1);
    assert_eq!(state.conversation("home", "api").count(), 1);
    assert_eq!(state.conversation("work", "other").count(), 0);
}
