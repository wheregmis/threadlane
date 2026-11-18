//! The GitHub poller (docs/milestone-8.md §3) on the fake GitHub: routing to
//! a task lane, a lead lane, or nowhere; deduplication; downtime; restarts
//! at every step boundary; and that it only ever reads.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use threadlane_core::github::{GITHUB_SOURCE, GithubItem, GithubKind, PullTarget};
use threadlane_core::ids::EventId;
use threadlane_core::model::{
    DecisionAnswer, DecisionKind, DecisionOption, Event, EventKind, MachineReport, NewDecision,
    NewProject, NewRepository, NewTask, Project, Task, TaskOrigin, TaskStatus, Trust,
};
use threadlane_core::runtime::steps::github;
use threadlane_core::runtime::{NoHooks, StepHook};
use threadlane_core::store::{DecisionStore, InboxStore, OrgStore, TaskStore};
use threadlane_core::time::Clock;
use threadlane_hub::{HubCore, HubSettings, OPEN_PULL_REQUEST};
use threadlane_store::{Bootstrap, HubDb};
use threadlane_testkit::fakes::{FakeGithub, GithubCall};
use threadlane_testkit::{CrashPlan, FakeClock};

use crate::common::{TestHub, usable_report};

const WIDGETS: &str = "https://github.com/acme/widgets.git";
/// The same repository as GitHub names it, in another case.
const WIDGETS_NAME: &str = "Acme/Widgets";
const BRANCH: &str = "threadlane/T-0001";

/// A hub with the fake GitHub as its source.
async fn hub_on(
    db: &HubDb,
    clock: &FakeClock,
    github: &FakeGithub,
    hook: Arc<dyn StepHook>,
) -> TestHub {
    let core = HubCore::new(
        db.clone(),
        Arc::new(clock.clone()),
        hook,
        HubSettings::default(),
    )
    .await
    .unwrap()
    .with_github(Arc::new(github.clone()));
    TestHub::around(core, clock.clone()).await
}

/// A hub that knows `acme/widgets` in General and `acme/gadgets`, by its
/// ssh remote, in Gadgets, with `gh` logged in.
async fn start(github: &FakeGithub, hook: Arc<dyn StepHook>) -> (TestHub, Project) {
    let clock = FakeClock::at_default_start();
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(clock.clone()))
        .await
        .unwrap();
    let hub = hub_on(&db, &clock, github, hook).await;
    hub.hello().await;
    let general = hub.general().await;
    register(&hub, &general, "widgets", WIDGETS).await;
    let gadgets = hub
        .db
        .create_project(&NewProject {
            name: "Gadgets".into(),
            ..NewProject::default()
        })
        .await
        .unwrap();
    register(&hub, &gadgets, "gadgets", "git@github.com:acme/gadgets.git").await;
    (hub, gadgets)
}

async fn register(hub: &TestHub, project: &Project, name: &str, remote: &str) {
    hub.db
        .create_repository(&NewRepository {
            project_id: project.id,
            name: name.into(),
            remote_url: remote.into(),
            default_branch: "main".into(),
            protected_branches: Vec::new(),
        })
        .await
        .unwrap();
}

/// A task in `widgets` whose pull request #42 was opened from its branch,
/// as the runner records it when it performs your approved call.
async fn task_with_pull_request(hub: &TestHub) -> Task {
    let general = hub.general().await;
    let widgets = hub.db.repositories(general.id).await.unwrap()[0].clone();
    let agent = hub.db.agents().await.unwrap()[0].id;
    let task = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: general.id,
                agent_id: agent,
                title: "Add the settings page".into(),
                goal: "The page saves".into(),
                repositories: vec![widgets.id],
                parent_task_id: None,
                depends_on: Vec::new(),
                origin: TaskOrigin::User,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    assert_eq!(task.id.to_string(), "T-0001");
    let lease = hub
        .db
        .grant_lease(
            task.lane_id,
            hub.machine,
            hub.clock.now(),
            Duration::from_secs(60),
        )
        .await
        .unwrap()
        .unwrap();
    record_pull_request(hub, &task, &lease).await;
    hub.db.release_lease(&lease).await.unwrap();
    task
}

pub(super) async fn record_pull_request(
    hub: &TestHub,
    task: &Task,
    lease: &threadlane_core::model::Lease,
) {
    let decision = hub
        .db
        .open_decision(
            Some(lease),
            &NewDecision {
                project_id: task.project_id,
                lane_id: task.lane_id,
                task_id: Some(task.id),
                session_id: None,
                run_id: None,
                tool_call_id: Some("toolu_pr".into()),
                tool: OPEN_PULL_REQUEST.into(),
                arguments: json!({ "repository": "widgets", "title": "Add the page" }),
                arguments_hash: "hash".into(),
                kind: DecisionKind::Approval,
                question: "Open a draft pull request?".into(),
                context: String::new(),
                options: vec![
                    DecisionOption {
                        label: "approve".into(),
                        effect: json!({ "approve": true }),
                    },
                    DecisionOption {
                        label: "decline".into(),
                        effect: json!({ "approve": false }),
                    },
                ],
                recommended: Some(0),
                offers_standing_rule: false,
                expires_at: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    hub.db
        .answer(
            decision.id,
            &DecisionAnswer {
                option: 0,
                note: None,
                standing_rule: false,
                answered_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    hub.db
        .begin_perform(lease, decision.id, hub.clock.now())
        .await
        .unwrap();
    hub.db
        .finish_perform(
            lease,
            decision.id,
            &json!({
                "performed": true,
                "repository": "widgets",
                "branch": BRANCH,
                "pull_request": "https://github.com/acme/widgets/pull/42",
            }),
        )
        .await
        .unwrap();
}

fn item(
    id: &str,
    kind: GithubKind,
    repository: &str,
    pull_request: Option<u64>,
    at: &str,
) -> GithubItem {
    GithubItem {
        id: id.into(),
        kind,
        repository: repository.into(),
        pull_request,
        branch: None,
        author: Some("octocat".into()),
        title: format!("{id} title"),
        body: format!("{id} body"),
        url: None,
        at: at.into(),
    }
}

fn check(id: &str, at: &str) -> GithubItem {
    GithubItem {
        branch: Some(BRANCH.into()),
        author: Some("github-actions".into()),
        ..item(id, GithubKind::CheckResult, "acme/widgets", Some(42), at)
    }
}

/// Every event the GitHub source inserted, oldest first.
async fn github_events(hub: &TestHub) -> Vec<Event> {
    let mut events = Vec::new();
    for id in 1.. {
        match hub.db.event(EventId(id)).await.unwrap() {
            Some(event) if event.source == GITHUB_SOURCE => events.push(event),
            Some(_) => {}
            None => return events,
        }
    }
    unreachable!()
}

fn ids(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event.source_event_id.as_str())
        .collect()
}

#[tokio::test]
async fn items_go_to_their_task_their_lead_or_nowhere() {
    let github = FakeGithub::default();
    let (hub, gadgets) = start(&github, Arc::new(NoHooks)).await;
    let task = task_with_pull_request(&hub).await;
    let general = hub.general().await;
    let at = "2026-11-17T08:00:00Z";
    github.notify(item(
        "n:review-42",
        GithubKind::ReviewRequest,
        WIDGETS_NAME,
        Some(42),
        at,
    ));
    github.notify(item(
        "n:review-50",
        GithubKind::ReviewRequest,
        "acme/widgets",
        Some(50),
        at,
    ));
    github.notify(GithubItem {
        branch: Some(BRANCH.into()),
        ..item("n:ci", GithubKind::CheckResult, "acme/widgets", None, at)
    });
    github.notify(item(
        "n:mention",
        GithubKind::Mention,
        "acme/gadgets",
        None,
        at,
    ));
    github.notify(item(
        "n:elsewhere",
        GithubKind::Comment,
        "other/thing",
        Some(42),
        at,
    ));
    github.add_to("acme/widgets", BRANCH, check("check_run:1", at));

    let pass = hub.core.poll_github_once().await.unwrap();
    assert_eq!(pass.inserted, 5);
    assert_eq!(
        pass.next_in,
        Duration::from_secs(60),
        "the default interval"
    );
    let events = github_events(&hub).await;
    let lane_of = |id: &str| {
        let event = events
            .iter()
            .find(|event| event.source_event_id == id)
            .unwrap_or_else(|| panic!("no event {id}"));
        (event.project_id, event.lane_id, event.requested)
    };
    assert_eq!(
        lane_of("n:review-42"),
        (general.id, task.lane_id, true),
        "its pull request"
    );
    assert_eq!(
        lane_of("n:ci"),
        (general.id, task.lane_id, true),
        "its branch"
    );
    assert_eq!(
        lane_of("check_run:1"),
        (general.id, task.lane_id, true),
        "its own work"
    );
    assert_eq!(
        lane_of("n:review-50"),
        (general.id, general.lead_lane_id, false),
        "another pull request of a registered repository"
    );
    assert_eq!(
        lane_of("n:mention"),
        (gadgets.id, gadgets.lead_lane_id, false),
        "matched through the ssh remote"
    );
    assert!(
        !ids(&events).contains(&"n:elsewhere"),
        "not registered here"
    );
    for event in &events {
        assert_eq!(event.kind, EventKind::OutsideEvent);
        assert_eq!(event.trust, Trust::External);
        assert_eq!(event.payload["id"], json!(event.source_event_id));
    }
    assert_eq!(
        github.calls()[1],
        GithubCall::PullRequest {
            target: PullTarget {
                repository: "acme/widgets".into(),
                branch: BRANCH.into(),
                number: Some(42),
            },
            since: None,
        },
        "the task's work is polled from its recorded outcome"
    );
}

#[tokio::test]
async fn the_same_item_twice_inserts_once_and_an_unchanged_answer_nothing() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    task_with_pull_request(&hub).await;
    github.set_poll_interval(Some(Duration::from_secs(90)));
    github.notify(item(
        "n:1",
        GithubKind::Mention,
        "acme/widgets",
        None,
        "2026-11-17T08:00:00Z",
    ));
    github.add_to(
        "acme/widgets",
        BRANCH,
        check("check_run:1", "2026-11-17T08:00:00Z"),
    );
    let first = hub.core.poll_github_once().await.unwrap();
    assert_eq!(first.inserted, 2);
    assert_eq!(
        first.next_in,
        Duration::from_secs(90),
        "the interval GitHub asked for"
    );

    // Unchanged: the feed is not modified and the work answers only what
    // its cursor already covers.
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 0);
    // Changed, but only by an item that is dropped: the old one comes again.
    github.notify(item(
        "n:2",
        GithubKind::Comment,
        "other/thing",
        None,
        "2026-11-17T08:05:00Z",
    ));
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 0);
    assert_eq!(ids(&github_events(&hub).await), ["n:1", "check_run:1"]);
    assert_eq!(
        hub.db
            .source_cursor("github:acme/widgets@threadlane/T-0001")
            .await
            .unwrap()
            .as_deref(),
        Some("2026-11-17T08:00:00Z")
    );
}

#[tokio::test]
async fn an_hour_with_the_poller_stopped_arrives_exactly_once() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    task_with_pull_request(&hub).await;
    hub.core.poll_github_once().await.unwrap();
    let mut expected = Vec::new();
    for minute in (0..60).step_by(10) {
        hub.clock.advance(Duration::from_secs(10 * 60));
        let at = format!("2026-11-17T09:{minute:02}:00Z");
        let notification = format!("n:{minute}");
        github.notify(item(
            &notification,
            GithubKind::Comment,
            "acme/widgets",
            Some(7),
            &at,
        ));
        let run = format!("check_run:{minute}");
        github.add_to("acme/widgets", BRANCH, check(&run, &at));
        expected.extend([notification, run]);
    }
    let pass = hub.core.poll_github_once().await.unwrap();
    assert_eq!(pass.inserted, expected.len());
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 0);
    let got: BTreeSet<String> = github_events(&hub)
        .await
        .into_iter()
        .map(|event| event.source_event_id)
        .collect();
    assert_eq!(got, expected.into_iter().collect::<BTreeSet<_>>());
}

#[tokio::test]
async fn the_poller_only_reads_and_only_while_gh_is_logged_in() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    task_with_pull_request(&hub).await;
    hub.link
        .hello(&MachineReport {
            gh_login: false,
            ..usable_report()
        })
        .await
        .unwrap();
    github.notify(item(
        "n:1",
        GithubKind::Mention,
        "acme/widgets",
        None,
        "2026-11-17T08:00:00Z",
    ));
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 0);
    assert!(github.calls().is_empty(), "no gh login, no call");

    hub.hello().await;
    hub.core.poll_github_once().await.unwrap();
    hub.core.poll_github_once().await.unwrap();
    let target = PullTarget {
        repository: "acme/widgets".into(),
        branch: BRANCH.into(),
        number: Some(42),
    };
    // The interface has reads only; nothing in it marks read or writes.
    assert_eq!(
        github.calls(),
        [
            GithubCall::Notifications { cursor: None },
            GithubCall::PullRequest {
                target: target.clone(),
                since: None
            },
            GithubCall::Notifications {
                cursor: Some("1".into())
            },
            GithubCall::PullRequest {
                target,
                since: None
            },
        ]
    );
}

#[tokio::test]
async fn a_kill_at_each_poller_step_then_a_restart_inserts_each_item_once() {
    for &step in github::ALL {
        let github = FakeGithub::default();
        let plan = CrashPlan::kill_at(step);
        let (hub, _) = start(&github, Arc::new(plan.clone())).await;
        task_with_pull_request(&hub).await;
        let at = "2026-11-17T08:00:00Z";
        github.notify(item("n:1", GithubKind::Mention, "acme/widgets", None, at));
        github.notify(item(
            "n:2",
            GithubKind::ReviewRequest,
            "acme/widgets",
            Some(42),
            at,
        ));
        github.add_to("acme/widgets", BRANCH, check("check_run:1", at));
        github.add_to(
            "acme/widgets",
            BRANCH,
            check("check_run:2", "2026-11-17T08:01:00Z"),
        );
        assert!(
            hub.core.poll_github_once().await.is_err(),
            "killed at {step}"
        );
        assert_eq!(plan.killed(), Some(step));

        let restarted = hub_on(&hub.db, &hub.clock, &github, Arc::new(NoHooks)).await;
        restarted.core.poll_github_once().await.unwrap();
        restarted.core.poll_github_once().await.unwrap();
        assert_eq!(
            ids(&github_events(&restarted).await),
            ["n:1", "n:2", "check_run:1", "check_run:2"],
            "killed at {step}"
        );
    }
}

#[tokio::test]
async fn invariant_19_a_hub_creates_events_only_for_its_own_repositories() {
    // One account, one feed, two hubs: each keeps only its own.
    let github = FakeGithub::default();
    let (work, _) = start(&github, Arc::new(NoHooks)).await;
    let clock = FakeClock::at_default_start();
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(clock.clone()))
        .await
        .unwrap();
    let personal = hub_on(&db, &clock, &github, Arc::new(NoHooks)).await;
    personal.hello().await;
    let general = personal.general().await;
    register(&personal, &general, "notes", "git@github.com:me/notes.git").await;
    // A repository that is not on GitHub is never matched to one that is.
    register(&personal, &general, "local", "/srv/git/acme/widgets.git").await;

    let at = "2026-11-17T08:00:00Z";
    github.notify(item(
        "n:work",
        GithubKind::ReviewRequest,
        "acme/widgets",
        Some(3),
        at,
    ));
    github.notify(item("n:notes", GithubKind::Mention, "me/notes", None, at));
    github.notify(item(
        "n:neither",
        GithubKind::Mention,
        "someone/else",
        None,
        at,
    ));
    work.core.poll_github_once().await.unwrap();
    personal.core.poll_github_once().await.unwrap();
    assert_eq!(ids(&github_events(&work).await), ["n:work"]);
    assert_eq!(ids(&github_events(&personal).await), ["n:notes"]);
}

#[tokio::test]
async fn archived_repository_github_events_are_recorded_and_never_run() {
    use threadlane_core::client::{OrgRequest, OverviewReply, OverviewRequest};
    use threadlane_core::model::EventStatus;

    let github = FakeGithub::default();
    let (hub, project) = start(&github, Arc::new(NoHooks)).await;
    let archive = |archived| {
        hub.api.org(OrgRequest::SetArchived {
            project: Some(project.name.clone()),
            archived,
        })
    };
    archive(true).await.unwrap();
    github.notify(item(
        "n:archived-review",
        GithubKind::ReviewRequest,
        "acme/gadgets",
        Some(9),
        "2026-11-17T08:00:00Z",
    ));
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
    let events = github_events(&hub).await;
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.project_id, project.id);
    assert_eq!(event.status, EventStatus::Skipped);
    assert!(matches!(
        hub.api.overview(OverviewRequest::Recorded {
            project: Some(project.name.clone()),
            limit: 5,
        }).await.unwrap(),
        OverviewReply::Recorded { events, total: 1 } if events[0].id == event.id
    ));
    assert!(hub.link.request_work().await.unwrap().is_none());
    archive(false).await.unwrap();
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 0);
    assert!(
        hub.db
            .pending_events(project.lead_lane_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(hub.link.request_work().await.unwrap().is_none());
}
