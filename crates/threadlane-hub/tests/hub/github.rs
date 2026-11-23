//! The GitHub poller (docs/milestone-8.md §3) on the fake GitHub: routing to
//! a task lane, a lead lane, or nowhere; deduplication; downtime; restarts
//! at every step boundary; and that it only ever reads.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use threadlane_core::client::{
    ApiError, OrgRequest, OverviewReply, OverviewRequest, PullRequestFiles, TaskCheckRun,
    TaskPullRequest,
};
use threadlane_core::github::{
    CheckRun, CheckStatus, Feed, GITHUB_SOURCE, GithubError, GithubItem, GithubKind, GithubSource,
    PullRequestFile, PullRequestState, PullRequestStatus, PullTarget,
};
use threadlane_core::ids::EventId;
use threadlane_core::link::HubMessage;
use threadlane_core::model::{
    DecisionAnswer, DecisionKind, DecisionOption, Event, EventKind, MachineReport, NewDecision,
    NewProject, NewRepository, NewTask, Project, Task, TaskOrigin, TaskStatus, Trust,
};
use threadlane_core::runtime::steps::github;
use threadlane_core::runtime::{NoHooks, StepHook};
use threadlane_core::store::{DecisionStore, InboxStore, OrgStore, TaskStore};
use threadlane_core::time::Clock;
use threadlane_hub::{COMMENT, HubCore, HubSettings, OPEN_PULL_REQUEST};
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
    let (task, lease) = leased_task(hub).await;
    record_pull_request(hub, &task, &lease).await;
    hub.db.release_lease(&lease).await.unwrap();
    task
}

/// A queued task T-0001 in `widgets`, with its lane leased.
async fn leased_task(hub: &TestHub) -> (Task, threadlane_core::model::Lease) {
    let general = hub.general().await;
    let (task, lease) = leased_task_for(hub, &general).await;
    assert_eq!(task.id.to_string(), "T-0001");
    (task, lease)
}

async fn leased_task_for(
    hub: &TestHub,
    project: &Project,
) -> (Task, threadlane_core::model::Lease) {
    let repository = hub.db.repositories(project.id).await.unwrap()[0].clone();
    let agent = hub.db.agents().await.unwrap()[0].id;
    let task = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: project.id,
                agent_id: agent,
                title: "Add the settings page".into(),
                goal: "The page saves".into(),
                repositories: vec![repository.id],
                parent_task_id: None,
                depends_on: Vec::new(),
                origin: TaskOrigin::User,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
                playbooks: Vec::new(),
            },
        )
        .await
        .unwrap();
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
    (task, lease)
}

pub(super) async fn record_pull_request(
    hub: &TestHub,
    task: &Task,
    lease: &threadlane_core::model::Lease,
) {
    record_performed(
        hub,
        task,
        lease,
        OPEN_PULL_REQUEST,
        json!({ "repository": "widgets", "title": "Add the page" }),
        json!({
            "performed": true,
            "repository": "widgets",
            "branch": BRANCH,
            "pull_request": "https://github.com/acme/widgets/pull/42",
        }),
    )
    .await;
}

/// A push onto an earlier task's branch, which updated that task's pull request.
pub(super) async fn record_updated_pull_request(
    hub: &TestHub,
    task: &Task,
    lease: &threadlane_core::model::Lease,
) {
    record_performed(
        hub,
        task,
        lease,
        threadlane_hub::PUBLISH_BRANCH,
        json!({ "repository": "widgets", "onto_task": "T-0001" }),
        json!({
            "performed": true,
            "repository": "widgets",
            "branch": BRANCH,
            "pull_request": "https://github.com/acme/widgets/pull/42",
        }),
    )
    .await;
}

/// Records an approved call of `tool` as performed with `outcome`.
async fn record_performed(
    hub: &TestHub,
    task: &Task,
    lease: &threadlane_core::model::Lease,
    tool: &str,
    arguments: serde_json::Value,
    outcome: serde_json::Value,
) {
    let decision = record_begun(hub, task, lease, tool, arguments).await;
    hub.db
        .finish_perform(lease, decision, &outcome)
        .await
        .unwrap();
}

/// Records an approved call of `tool` whose perform has begun.
async fn record_begun(
    hub: &TestHub,
    task: &Task,
    lease: &threadlane_core::model::Lease,
    tool: &str,
    arguments: serde_json::Value,
) -> threadlane_core::ids::DecisionId {
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
                tool_call_id: Some(format!("toolu_{tool}")),
                tool: tool.into(),
                arguments,
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
                urgent: true,
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
    decision.id
}

/// A task that only commented on someone else's pull request #529 is
/// woken by that pull request's finished checks, read on its head branch.
#[tokio::test]
async fn a_pull_request_the_task_commented_on_reaches_its_lane() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let (task, lease) = leased_task(&hub).await;
    record_performed(
        &hub,
        &task,
        &lease,
        COMMENT,
        json!({ "repository": "widgets", "target": "pull_request", "number": 529, "body": "Looks good." }),
        json!({ "performed": true, "summary": "Completed the repository action." }),
    )
    .await;
    hub.db.release_lease(&lease).await.unwrap();
    let at = "2026-11-17T08:00:00Z";
    github.add_to(
        "acme/widgets",
        "codex/agent-model-preferences",
        GithubItem {
            branch: Some("codex/agent-model-preferences".into()),
            ..item(
                "check_run:529",
                GithubKind::CheckResult,
                "acme/widgets",
                Some(529),
                at,
            )
        },
    );

    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
    let events = github_events(&hub).await;
    assert_eq!(ids(&events), ["check_run:529"]);
    assert_eq!(events[0].lane_id, task.lane_id);
    assert!(events[0].requested, "it continues the task");
    assert!(github.calls().contains(&GithubCall::PullRequest {
        target: PullTarget {
            repository: "acme/widgets".into(),
            branch: String::new(),
            number: Some(529),
        },
        since: None,
    }));
}

/// An approved batch merge or mark-ready links each pull request it handled
/// to the task, as a single call does (#746); one that stopped partway links
/// those before the stop, and the stopped one only when the error says its
/// merge may have happened.
#[tokio::test]
async fn pull_requests_a_batch_handled_reach_the_tasks_lane() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let (task, lease) = leased_task(&hub).await;
    let head = "a".repeat(40);
    record_performed(
        &hub,
        &task,
        &lease,
        threadlane_hub::MERGE,
        json!({ "repository": "widgets", "pulls": [
            { "number": 601, "expected_head": head },
            { "number": 602, "expected_head": head },
        ] }),
        json!({ "performed": true, "summary": "Completed the repository action. merged #601, #602 in order" }),
    )
    .await;
    record_performed(
        &hub,
        &task,
        &lease,
        threadlane_hub::MARK_READY,
        json!({ "repository": "widgets", "numbers": [701, 702, 703] }),
        json!({ "performed": false, "error": "forge: batch stopped at #702, marked #701 ready, the rest not attempted: boom" }),
    )
    .await;
    record_performed(
        &hub,
        &task,
        &lease,
        threadlane_hub::MERGE,
        json!({ "repository": "widgets", "pulls": [
            { "number": 801, "expected_head": head },
            { "number": 802, "expected_head": head },
            { "number": 803, "expected_head": head },
        ] }),
        json!({ "performed": false, "error": format!(
            "forge: batch stopped at #802 (expected head {head}{}), merged #801, the rest not \
             attempted: the merge was requested but its state could not be read",
            threadlane_core::forge::BATCH_MAY_HAVE_CHANGED,
        ) }),
    )
    .await;
    hub.db.release_lease(&lease).await.unwrap();
    let at = "2026-11-17T08:00:00Z";
    for number in [601, 602, 701, 702, 703, 801, 802, 803] {
        github.add_to(
            "acme/widgets",
            &format!("feature-{number}"),
            GithubItem {
                branch: Some(format!("feature-{number}")),
                ..item(
                    &format!("check_run:{number}"),
                    GithubKind::CheckResult,
                    "acme/widgets",
                    Some(number),
                    at,
                )
            },
        );
    }

    hub.core.poll_github_once().await.unwrap();
    let events = github_events(&hub).await;
    let mut routed: Vec<_> = events
        .iter()
        .filter(|event| event.lane_id == task.lane_id)
        .map(|event| event.source_event_id.as_str())
        .collect();
    routed.sort();
    assert_eq!(
        routed,
        [
            "check_run:601",
            "check_run:602",
            "check_run:701",
            "check_run:801",
            "check_run:802"
        ],
        "{events:?}"
    );
    for number in [601, 602, 701, 801, 802] {
        assert!(github.calls().contains(&GithubCall::PullRequest {
            target: PullTarget {
                repository: "acme/widgets".into(),
                branch: String::new(),
                number: Some(number),
            },
            since: None,
        }));
    }
}

/// A batch can merge in a project repository the task has no worktree in,
/// and the task page lists its pull requests even when it was read, empty,
/// before the batch and the task then ended without a poll (#746).
#[tokio::test]
async fn a_batch_in_an_unattached_repository_shows_on_the_ended_tasks_page() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let general = hub.general().await;
    register(&hub, &general, "tools", "https://github.com/acme/tools.git").await;
    let (task, lease) = leased_task(&hub).await;
    hub.db.release_lease(&lease).await.unwrap();
    for number in [801, 802] {
        github.set_pull_request("acme/tools", number, Ok(conflicted_draft(&"a".repeat(40))));
    }
    assert!(
        pull_requests(&hub, &task).await.is_empty(),
        "the page caches none"
    );

    let (lease, run) = crate::common::begin(&*hub.link).await;
    let head = "a".repeat(40);
    let decision = record_begun(
        &hub,
        &task,
        &lease,
        threadlane_hub::MERGE,
        json!({ "repository": "tools", "pulls": [
            { "number": 801, "expected_head": head },
            { "number": 802, "expected_head": head },
        ] }),
    )
    .await;
    hub.link
        .finish_perform(
            &lease,
            run.run_id,
            decision,
            &json!({ "performed": true, "summary": "merged #801, #802 in order" }),
        )
        .await
        .unwrap();
    for (id, tool, arguments) in [
        (
            "p",
            threadlane_hub::MEMORY_CREATE,
            json!({ "path": "project/tasks/T-0001.md", "content": "# T-0001\nDone." }),
        ),
        (
            "f",
            threadlane_hub::FINISH_TASK,
            json!({ "summary": "Done" }),
        ),
    ] {
        hub.link
            .call_tool(&lease, run.run_id, Some(id), tool, &arguments)
            .await
            .unwrap();
    }
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    hub.link.release(&lease).await.unwrap();

    let mut numbers: Vec<_> = pull_requests(&hub, &task)
        .await
        .iter()
        .map(|pull| (pull.repository.clone(), pull.number))
        .collect();
    numbers.sort();
    assert_eq!(
        numbers,
        [
            ("acme/tools".to_owned(), 801),
            ("acme/tools".to_owned(), 802)
        ]
    );
}

/// A cached task page also reads a pull request an outcome links later,
/// however the cache was filled, and keeps the known ones when GitHub then
/// fails to read them (#746).
#[tokio::test]
async fn a_newly_linked_pull_request_joins_the_cached_page_without_dropping_known_ones() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let (task, lease) = leased_task(&hub).await;
    let head = "a".repeat(40);
    github.set_pull_request("acme/widgets", 901, Ok(conflicted_draft(&head)));
    github.set_pull_request("acme/widgets", 902, Ok(conflicted_draft(&head)));
    record_performed(
        &hub,
        &task,
        &lease,
        threadlane_hub::MARK_READY,
        json!({ "repository": "widgets", "number": 901 }),
        json!({ "performed": true, "summary": "Completed the repository action." }),
    )
    .await;
    let numbers = |pulls: Vec<TaskPullRequest>| -> Vec<u64> {
        let mut numbers: Vec<_> = pulls.iter().map(|pull| pull.number).collect();
        numbers.sort_unstable();
        numbers
    };
    assert_eq!(numbers(pull_requests(&hub, &task).await), [901]);

    github.set_pull_request("acme/widgets", 901, Err("rate limited".into()));
    record_performed(
        &hub,
        &task,
        &lease,
        threadlane_hub::MARK_READY,
        json!({ "repository": "widgets", "numbers": [902] }),
        json!({ "performed": true, "summary": "marked #902 ready" }),
    )
    .await;
    assert_eq!(
        numbers(pull_requests(&hub, &task).await),
        [901, 902],
        "the new link is read and the known one is kept"
    );
}

/// A first page load keeps the pull requests it read when another read
/// fails, so a later failed read of a known one does not hide it (#746).
#[tokio::test]
async fn a_first_page_load_keeps_what_it_read_when_another_read_fails() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let (task, lease) = leased_task(&hub).await;
    github.set_pull_request("acme/widgets", 801, Ok(conflicted_draft(&"a".repeat(40))));
    github.set_pull_request("acme/widgets", 802, Err("rate limited".into()));
    record_performed(
        &hub,
        &task,
        &lease,
        threadlane_hub::MARK_READY,
        json!({ "repository": "widgets", "numbers": [801, 802] }),
        json!({ "performed": true, "summary": "marked #801, #802 ready" }),
    )
    .await;
    let numbers = |pulls: Vec<TaskPullRequest>| -> Vec<u64> {
        pulls.iter().map(|pull| pull.number).collect()
    };
    assert_eq!(numbers(pull_requests(&hub, &task).await), [801]);

    github.set_pull_request("acme/widgets", 801, Err("rate limited".into()));
    assert_eq!(
        numbers(pull_requests(&hub, &task).await),
        [801],
        "the known pull request stays while #802 is retried"
    );
    assert_eq!(
        github
            .calls()
            .iter()
            .filter(|call| matches!(call, GithubCall::PullRequestState { number: 802, .. }))
            .count(),
        2,
        "the failed read is retried"
    );
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
        github.calls()[2],
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

/// Runs `task` to its end: its progress file, `finish_task`, and the commit.
async fn finish(hub: &TestHub, task: &Task) {
    let (lease, run) = crate::common::begin(&*hub.link).await;
    for (id, tool, arguments) in [
        (
            "p",
            threadlane_hub::MEMORY_CREATE,
            json!({ "path": "project/tasks/T-0001.md", "content": "# T-0001\nDone." }),
        ),
        (
            "f",
            threadlane_hub::FINISH_TASK,
            json!({ "summary": "Done" }),
        ),
    ] {
        hub.link
            .call_tool(&lease, run.run_id, Some(id), tool, &arguments)
            .await
            .unwrap();
    }
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    hub.link.release(&lease).await.unwrap();
    assert_eq!(
        hub.db.task(task.id).await.unwrap().unwrap().status,
        TaskStatus::Done
    );
}

#[tokio::test]
async fn an_ended_tasks_worktrees_are_removed_when_it_ends_unless_they_hold_work() {
    let dir = tempfile::tempdir().unwrap();
    let clock = FakeClock::at_default_start();
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(clock.clone()))
        .await
        .unwrap();
    let core = HubCore::new(
        db,
        Arc::new(clock.clone()),
        Arc::new(NoHooks),
        HubSettings::default(),
    )
    .await
    .unwrap()
    .with_workspaces(dir.path().to_owned());
    let hub = TestHub::around(core, clock.clone()).await;
    hub.hello().await;
    register(&hub, &hub.general().await, "widgets", WIDGETS).await;
    let task = task_with_pull_request(&hub).await;
    let worktrees = dir.path().join("worktrees");
    let ended = worktrees.join("T-0001");
    // No task T-0099 exists here: its directory is not the hub's to remove.
    let unknown = worktrees.join("T-0099");
    for tree in [&ended, &unknown] {
        std::fs::create_dir_all(tree.join("widgets")).unwrap();
    }

    hub.core.sweep_once().await.unwrap();
    assert!(ended.exists(), "a running task keeps its worktrees");
    finish(&hub, &task).await;
    hub.core.sweep_once().await.unwrap();
    assert!(
        ended.join("widgets").exists(),
        "a directory that is not a git worktree may hold work: kept"
    );
    std::fs::remove_dir(ended.join("widgets")).unwrap();
    hub.core.sweep_once().await.unwrap();
    assert!(ended.exists(), "a kept task is not checked again at once");
    clock.advance(Duration::from_secs(3 * 24 * 60 * 60));
    hub.core.sweep_once().await.unwrap();
    assert!(!ended.exists(), "removed once nothing in it would be lost");
    assert!(unknown.exists());
    hub.core.sweep_once().await.unwrap();
}

#[tokio::test]
async fn activity_on_a_finished_tasks_pull_request_reaches_the_lead_with_its_details() {
    let github = FakeGithub::default();
    let (hub, _gadgets) = start(&github, Arc::new(NoHooks)).await;
    let general = hub.general().await;
    let task = task_with_pull_request(&hub).await;
    finish(&hub, &task).await;

    // Nothing is read for a finished task until GitHub reports activity.
    github.add_to(
        "acme/widgets",
        BRANCH,
        GithubItem {
            title: "test: failure".into(),
            ..check("check_run:9", "2026-01-01T00:00:01Z")
        },
    );
    github.add_to(
        "acme/widgets",
        BRANCH,
        GithubItem {
            title: "lint: success".into(),
            ..check("check_run:8", "2026-01-01T00:00:01Z")
        },
    );
    hub.core.poll_github_once().await.unwrap();
    assert!(github_events(&hub).await.is_empty());

    let at = "2026-01-01T00:00:02Z";
    let ci = GithubItem {
        kind: GithubKind::CheckResult,
        ..notified(at)
    };
    github.set_activity(
        &ci.id,
        vec![
            head("aaa", at),
            GithubItem {
                title: "test: failure".into(),
                ..check("check_run:9", at)
            },
            GithubItem {
                title: "lint: success".into(),
                ..check("check_run:8", at)
            },
        ],
    );
    github.notify(ci);
    let review = item("review:12", GithubKind::Review, WIDGETS_NAME, Some(42), at);
    github.add_to("acme/widgets", BRANCH, review.clone());
    let later = "2026-01-01T00:00:03Z";
    deliver(&github, vec![head("aaa", later), review]);
    hub.core.poll_github_once().await.unwrap();
    let events = github_events(&hub).await;
    assert_eq!(ids(&events), ["check_run:9", "review:12"]);
    assert!(
        events
            .iter()
            .all(|event| event.lane_id == general.lead_lane_id && event.requested)
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
    let state = GithubCall::PullRequestState {
        repository: "acme/widgets".into(),
        number: 42,
    };
    // The interface has reads only; nothing in it marks read or writes.
    assert_eq!(
        github.calls(),
        [
            GithubCall::Notifications { cursor: None },
            state.clone(),
            GithubCall::PullRequest {
                target: target.clone(),
                since: None
            },
            GithubCall::Notifications {
                cursor: Some("1".into())
            },
            state,
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

/// Gate one source operation without sleeping so deletion can be attempted while
/// that operation is provably still waiting.
#[derive(Debug, Default)]
struct PausedGithub {
    source: FakeGithub,
    pause_pull: bool,
    /// Pause reading a notification's activity on `acme/gadgets` instead.
    pause_activity: bool,
    entered: tokio::sync::Notify,
    released: tokio::sync::Notify,
}

impl PausedGithub {
    async fn pause(&self) {
        self.entered.notify_one();
        self.released.notified().await;
    }
}

#[async_trait::async_trait]
impl GithubSource for PausedGithub {
    async fn notifications(&self, cursor: Option<&str>) -> Result<Feed, GithubError> {
        if !self.pause_pull && !self.pause_activity {
            self.pause().await;
        }
        self.source.notifications(cursor).await
    }

    async fn pull_request(
        &self,
        target: &PullTarget,
        since: Option<&str>,
    ) -> Result<Vec<GithubItem>, GithubError> {
        if self.pause_pull && target.repository == "acme/gadgets" {
            self.pause().await;
        }
        self.source.pull_request(target, since).await
    }

    async fn activity(
        &self,
        notification: &GithubItem,
        items: &mut Vec<GithubItem>,
    ) -> Result<(), GithubError> {
        if self.pause_activity && notification.repository == "acme/gadgets" {
            self.pause().await;
        }
        self.source.activity(notification, items).await
    }
}

#[tokio::test]
async fn deletion_completes_during_notification_fetch_and_only_live_routes_receive_events() {
    let github = Arc::new(PausedGithub::default());
    let (hub, gadgets) = start(&github.source, Arc::new(NoHooks)).await;
    let when = "2026-11-17T08:00:00Z";
    github.source.notify(item(
        "deleted",
        GithubKind::Mention,
        "acme/gadgets",
        None,
        when,
    ));
    github.source.notify(item(
        "valid",
        GithubKind::Mention,
        "acme/widgets",
        None,
        when,
    ));
    let core = hub.core.clone().with_github(github.clone());
    let polling = tokio::spawn(async move { core.poll_github_once().await });
    assert_deleted_while_paused(&hub, &github, &gadgets.name).await;
    assert_eq!(polling.await.unwrap().unwrap().inserted, 1);
    assert_eq!(ids(&github_events(&hub).await), ["valid"]);
    assert!(
        hub.db
            .source_cursor("github:notifications")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn deletion_completes_during_pull_fetch_and_discarded_targets_do_not_advance() {
    let github = Arc::new(PausedGithub {
        pause_pull: true,
        ..PausedGithub::default()
    });
    let (hub, gadgets) = start(&github.source, Arc::new(NoHooks)).await;
    let kept = task_with_pull_request(&hub).await;
    let (task, lease) = leased_task_for(&hub, &gadgets).await;
    record_performed(
        &hub, &task, &lease, OPEN_PULL_REQUEST,
        json!({ "repository": "gadgets", "title": "Work in Gadgets" }),
        json!({ "performed": true, "repository": "gadgets", "branch": "topic", "pull_request": "https://github.com/acme/gadgets/pull/7" }),
    ).await;
    hub.db.release_lease(&lease).await.unwrap();
    let when = "2026-11-17T08:00:00Z";
    github
        .source
        .add_to("acme/widgets", BRANCH, check("valid", when));
    github.source.add_to(
        "acme/gadgets",
        "topic",
        GithubItem {
            branch: Some("topic".into()),
            ..item(
                "deleted",
                GithubKind::CheckResult,
                "acme/gadgets",
                Some(7),
                when,
            )
        },
    );
    let core = hub.core.clone().with_github(github.clone());
    let polling = tokio::spawn(async move { core.poll_github_once().await });
    assert_deleted_while_paused(&hub, &github, &gadgets.name).await;
    assert_eq!(polling.await.unwrap().unwrap().inserted, 1);
    let events = hub.db.pending_events(kept.lane_id).await.unwrap();
    assert!(ids(&events).contains(&"valid"));
    assert_eq!(
        hub.db
            .source_cursor("github:acme/gadgets@topic")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        hub.db
            .source_cursor(&format!("github:acme/widgets@{BRANCH}"))
            .await
            .unwrap()
            .as_deref(),
        Some(when),
    );
}

async fn assert_deleted_while_paused(hub: &TestHub, github: &PausedGithub, project: &str) {
    let within = Duration::from_secs(5);
    tokio::time::timeout(within, github.entered.notified())
        .await
        .unwrap();
    let deleted = tokio::time::timeout(
        within,
        hub.api.org(OrgRequest::DeleteProject {
            project: project.into(),
            confirm_name: project.into(),
        }),
    )
    .await;
    github.released.notify_one();
    deleted
        .expect("deletion completed while GitHub was blocked")
        .unwrap();
}

/// Notification `at` of thread 7 on `acme/widgets#42`, as the feed lists it.
fn notified(at: &str) -> GithubItem {
    GithubItem {
        author: None,
        ..item(
            &format!("notification:7:{at}"),
            GithubKind::Notification,
            WIDGETS_NAME,
            Some(42),
            at,
        )
    }
}

/// What thread update `at` reports first: the pull request's head.
fn head(sha: &str, at: &str) -> GithubItem {
    GithubItem {
        kind: GithubKind::PullRequestUpdate,
        title: format!("head {sha}"),
        body: "Add the settings page (open)".into(),
        ..notified(at)
    }
}

fn notified_review(state: &str, at: &str) -> GithubItem {
    GithubItem {
        title: state.into(),
        ..item("review:5", GithubKind::Review, WIDGETS_NAME, Some(42), at)
    }
}

fn deliver(github: &FakeGithub, activity: Vec<GithubItem>) {
    let notification = notified(&activity[0].at);
    github.set_activity(&notification.id, activity);
    github.notify(notification);
}

/// Something new in the feed that no lane hears of, so the feed lists every
/// unread thread again.
fn touch_feed(github: &FakeGithub, at: &str) {
    github.notify(item(at, GithubKind::Mention, "acme/elsewhere", None, at));
}

fn payload(event: &Event) -> GithubItem {
    serde_json::from_value(event.payload.clone()).unwrap()
}

async fn expect_nudge(hub: &TestHub) {
    let message = tokio::time::timeout(Duration::from_secs(1), hub.link.next_message())
        .await
        .expect("GitHub activity should wake the lead");
    assert_eq!(message, Some(HubMessage::Nudge));
}

#[tokio::test]
async fn a_pull_request_notification_says_what_happened_and_wakes_once_per_activity() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let general = hub.general().await;

    // A push: the new head, and the review that was already there.
    let t1 = "2026-11-17T08:00:00Z";
    deliver(
        &github,
        vec![head("aaa", t1), notified_review("approved", t1)],
    );
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 2);
    let events = github_events(&hub).await;
    assert!(
        events
            .iter()
            .all(|event| (event.lane_id, event.requested) == (general.lead_lane_id, false)),
        "no task owns #42, so its lead hears of it"
    );
    assert_eq!(payload(&events[0]).author.as_deref(), Some("octocat"));
    assert_eq!(payload(&events[0]).title, "approved", "the review state");
    assert_eq!(payload(&events[1]).title, "head aaa", "the new head");
    let pending = hub.db.pending_events(general.lead_lane_id).await.unwrap();
    assert_eq!(pending.len(), 2, "the lead has them to wake for");
    expect_nudge(&hub).await;

    // The same update delivered again is not read again and inserts nothing,
    // so it wakes nobody; what it would report now does not matter.
    github.set_activity(&notified(t1).id, vec![head("zzz", t1)]);
    // Recreate a crash after the last event insert but before saving its cursor.
    hub.db
        .set_source_cursor("github:lead:notification:7:", "", hub.clock.now())
        .await
        .unwrap();
    touch_feed(&github, "2026-11-17T08:00:30Z");
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 0);

    let repeat = "2026-11-17T08:01:00Z";
    deliver(
        &github,
        vec![head("aaa", repeat), notified_review("approved", t1)],
    );
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), hub.link.next_message())
            .await
            .is_err(),
        "redelivering the same activity gives no wake"
    );

    // Later distinct activity on the same pull request still arrives: a
    // comment, a second review request on the same head, and a check.
    let t2 = "2026-11-17T08:02:00Z";
    let comment = GithubItem {
        author: Some("stranger".into()),
        title: "comment".into(),
        ..item("comment:9", GithubKind::Comment, WIDGETS_NAME, Some(42), t2)
    };
    deliver(
        &github,
        vec![head("aaa", t2), notified_review("approved", t1), comment],
    );
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
    expect_nudge(&hub).await;
    let t3 = "2026-11-17T08:03:00Z";
    let request = GithubItem {
        kind: GithubKind::ReviewRequest,
        ..notified(t3)
    };
    github.set_activity(
        &request.id,
        vec![
            head("aaa", t3),
            GithubItem {
                id: "review_request:3".into(),
                ..request.clone()
            },
        ],
    );
    github.notify(request.clone());
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
    let t4 = "2026-11-17T08:04:00Z";
    let ci = GithubItem {
        title: "CI: failed".into(),
        ..item(
            &format!("notification:8:{t4}"),
            GithubKind::CheckResult,
            WIDGETS_NAME,
            Some(42),
            t4,
        )
    };
    github.notify(ci);
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
    let events = github_events(&hub).await;
    assert_eq!(
        ids(&events),
        [
            "notified:review:5:2026-11-17T08:00:00Z:approved",
            &format!("notification:7:{t1}"),
            "notified:comment:9:2026-11-17T08:02:00Z:comment",
            &format!("notified:review_request:3:{t3}:{}", request.title),
            &format!("notification:8:{t4}"),
        ]
    );
    assert_eq!(payload(&events[2]).author.as_deref(), Some("stranger"));
    assert_eq!(
        payload(&events[4]).title,
        "CI: failed",
        "the check and its result"
    );
    let pushed = "2026-11-17T08:05:00Z";
    deliver(&github, vec![head("bbb", pushed)]);
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
    let push = payload(github_events(&hub).await.last().unwrap());
    assert_eq!(push.kind, GithubKind::Push);
    assert_eq!(push.title, "head bbb");

    for (at, state) in [
        ("2026-11-17T08:06:00Z", "closed"),
        ("2026-11-17T08:07:00Z", "open"),
    ] {
        let update = GithubItem {
            body: format!("Add the settings page ({state})"),
            ..head("bbb", at)
        };
        deliver(&github, vec![update]);
        assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
        assert_eq!(
            payload(github_events(&hub).await.last().unwrap()).kind,
            GithubKind::PullRequestUpdate
        );
    }
}

#[tokio::test]
async fn mentions_and_review_requests_wake_only_for_distinct_activity_on_the_same_head() {
    for (reason, kind) in [
        (GithubKind::Mention, GithubKind::Comment),
        (GithubKind::ReviewRequest, GithubKind::ReviewRequest),
    ] {
        let github = FakeGithub::default();
        let (hub, _) = start(&github, Arc::new(NoHooks)).await;
        let first = "2026-11-17T08:00:00Z";
        for (at, id, happened, count) in [
            (first, "1", first, 2),
            ("2026-11-17T08:01:00Z", "1", first, 0),
            ("2026-11-17T08:02:00Z", "2", "2026-11-17T08:02:00Z", 1),
        ] {
            let notification = GithubItem {
                kind: reason,
                ..notified(at)
            };
            let detail = item(id, kind, WIDGETS_NAME, Some(42), happened);
            github.set_activity(&notification.id, vec![head("aaa", at), detail]);
            github.notify(notification);
            assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, count);
            if count == 0 {
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), hub.link.next_message())
                        .await
                        .is_err()
                );
            } else {
                expect_nudge(&hub).await;
            }
        }
    }
}

#[tokio::test]
async fn a_partial_lookup_is_retried_and_only_the_missing_comment_wakes_again() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let at = "2026-11-17T08:00:00Z";
    let notification = notified(at);
    let mut items = vec![head("aaa", at), notified_review("approved", at)];
    github.fail_activity(&notification.id, items.clone());
    github.notify(notification.clone());
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 2);
    expect_nudge(&hub).await;
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 0);
    items.push(item(
        "comment:6",
        GithubKind::Comment,
        WIDGETS_NAME,
        Some(42),
        at,
    ));
    github.set_activity(&notification.id, items);
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
    expect_nudge(&hub).await;
    assert_eq!(
        payload(github_events(&hub).await.last().unwrap()).kind,
        GithubKind::Comment
    );
    touch_feed(&github, "2026-11-17T08:05:00Z");
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 0);
}

#[tokio::test]
async fn a_task_given_the_pull_request_later_still_gets_the_review_its_lead_heard_of() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let t1 = "2026-11-17T08:00:00Z";
    deliver(
        &github,
        vec![head("aaa", t1), notified_review("changes requested", t1)],
    );
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 2);

    let task = task_with_pull_request(&hub).await;
    github.add_to(
        "acme/widgets",
        BRANCH,
        GithubItem {
            title: "changes requested".into(),
            branch: Some(BRANCH.into()),
            ..item("review:5", GithubKind::Review, "acme/widgets", Some(42), t1)
        },
    );
    hub.core.poll_github_once().await.unwrap();
    let events = hub.db.pending_events(task.lane_id).await.unwrap();
    assert!(ids(&events).contains(&"review:5"), "{:?}", ids(&events));
}

#[tokio::test]
async fn a_notification_whose_activity_cannot_be_read_still_arrives() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let t1 = "2026-11-17T08:00:00Z";
    github.fail_activity(&notified(t1).id, vec![]);
    github.notify(notified(t1));
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
    let events = github_events(&hub).await;
    let fallback = payload(&events[0]);
    assert_eq!(fallback.title, notified(t1).title);
    assert_eq!(fallback.kind, notified(t1).kind);
    github.set_activity(&notified(t1).id, vec![head("aaa", t1)]);
    assert_eq!(hub.core.poll_github_once().await.unwrap().inserted, 1);
    assert_eq!(payload(&github_events(&hub).await[1]).title, "head aaa");
}

#[tokio::test]
async fn deletion_completes_while_a_notification_activity_is_read() {
    let github = Arc::new(PausedGithub {
        pause_activity: true,
        ..PausedGithub::default()
    });
    let (hub, gadgets) = start(&github.source, Arc::new(NoHooks)).await;
    let when = "2026-11-17T08:00:00Z";
    github.source.notify(item(
        "deleted",
        GithubKind::ReviewRequest,
        "acme/gadgets",
        Some(3),
        when,
    ));
    github.source.notify(notified(when));
    let core = hub.core.clone().with_github(github.clone());
    let polling = tokio::spawn(async move { core.poll_github_once().await });
    assert_deleted_while_paused(&hub, &github, &gadgets.name).await;
    assert_eq!(polling.await.unwrap().unwrap().inserted, 1);
    assert_eq!(
        ids(&github_events(&hub).await),
        [notified(when).id.as_str()]
    );
}

#[tokio::test]
async fn the_lead_reads_an_issue_of_a_registered_repository_without_a_decision() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    github.set_issue(
        "acme/widgets",
        7,
        json!({ "title": "Crash", "comments": [{ "body": "Plan: fix it." }] }),
    );
    hub.send("m-1", "Plan issue 7").await;
    let (lease, run) = crate::common::begin(&*hub.link).await;
    assert_eq!(run.lane_kind, threadlane_core::model::LaneKind::Lead);
    let view = |id: &'static str, arguments: serde_json::Value| {
        let link = Arc::clone(&hub.link);
        async move {
            link.call_tool(
                &lease,
                run.run_id,
                Some(id),
                threadlane_hub::VIEW_ISSUE,
                &arguments,
            )
            .await
        }
    };
    let read = view("v1", json!({ "repository": "widgets", "number": 7 }))
        .await
        .unwrap();
    assert_eq!(read["title"], "Crash");
    assert_eq!(read["comments"][0]["body"], "Plan: fix it.");
    assert!(read["trust"].as_str().unwrap().starts_with("external"));
    let unknown = view("v2", json!({ "repository": "gadgets", "number": 7 })).await;
    assert!(
        matches!(
            unknown,
            Err(threadlane_core::tools::ToolError::InvalidArguments(_))
        ),
        "another project's repository is not named: {unknown:?}"
    );
    let missing = view("v3", json!({ "repository": "widgets", "number": 8 })).await;
    assert!(missing.is_err(), "{missing:?}");
    assert!(
        hub.db
            .lane_decisions(hub.general_lane().await.id)
            .await
            .unwrap()
            .is_empty(),
        "reading asks nothing"
    );
}

/// Pull request 42 of `acme/widgets` at `head`: a draft that conflicts, one
/// check failed, one passed and one still running.
fn conflicted_draft(head: &str) -> PullRequestState {
    let check = |name: &str, conclusion: Option<&str>| CheckRun {
        name: name.into(),
        conclusion: conclusion.map(str::to_owned),
    };
    PullRequestState {
        head_repository: "acme/widgets".into(),
        head_ref: BRANCH.into(),
        head_sha: head.into(),
        draft: true,
        mergeable_state: "dirty".into(),
        checks: vec![
            check("test", Some("failure")),
            check("lint", Some("success")),
            check("build", None),
        ],
        url: "https://github.com/acme/widgets/pull/42".into(),
        state: "open".into(),
        merged: false,
    }
}

async fn pull_requests(hub: &TestHub, task: &Task) -> Vec<TaskPullRequest> {
    match hub
        .api
        .overview(OverviewRequest::TaskPullRequests { task: task.id })
        .await
        .unwrap()
    {
        OverviewReply::TaskPullRequests { pulls } => pulls,
        other => panic!("{other:?}"),
    }
}

fn state_reads(github: &FakeGithub) -> usize {
    github
        .calls()
        .iter()
        .filter(|call| matches!(call, GithubCall::PullRequestState { .. }))
        .count()
}

#[tokio::test]
async fn a_tasks_pull_request_status_is_read_on_the_poll_and_served_from_the_cache() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let task = task_with_pull_request(&hub).await;
    github.set_pull_request("acme/widgets", 42, Ok(conflicted_draft(&"a".repeat(40))));
    hub.core.poll_github_once().await.unwrap();
    assert_eq!(state_reads(&github), 1);

    let read_at = hub.clock.now();
    let expected = TaskPullRequest {
        repository: "acme/widgets".into(),
        number: 42,
        url: "https://github.com/acme/widgets/pull/42".into(),
        status: PullRequestStatus::Draft,
        head_sha: "a".repeat(40),
        checks: vec![
            TaskCheckRun {
                name: "test".into(),
                status: CheckStatus::Failing,
            },
            TaskCheckRun {
                name: "lint".into(),
                status: CheckStatus::Passing,
            },
            TaskCheckRun {
                name: "build".into(),
                status: CheckStatus::Running,
            },
        ],
        mergeable_state: "dirty".into(),
        conflicted: true,
        refreshed_at: read_at,
    };
    assert_eq!(
        pull_requests(&hub, &task).await,
        std::slice::from_ref(&expected)
    );
    assert_eq!(
        pull_requests(&hub, &task).await,
        std::slice::from_ref(&expected)
    );
    assert_eq!(
        state_reads(&github),
        1,
        "asking reads the cache, not GitHub"
    );

    hub.clock.advance(Duration::from_secs(60));
    github.set_pull_request("acme/widgets", 42, Err("rate limited".into()));
    hub.core.poll_github_once().await.unwrap();
    assert_eq!(
        pull_requests(&hub, &task).await,
        [expected],
        "a failed read keeps what was cached"
    );

    github.set_pull_request(
        "acme/widgets",
        42,
        Ok(PullRequestState {
            draft: false,
            state: "closed".into(),
            merged: true,
            mergeable_state: "unknown".into(),
            ..conflicted_draft(&"a".repeat(40))
        }),
    );
    hub.core.poll_github_once().await.unwrap();
    let pulls = pull_requests(&hub, &task).await;
    assert_eq!(
        pulls.len(),
        1,
        "the same pull request is replaced, not added"
    );
    assert_eq!(pulls[0].status, PullRequestStatus::Merged);
    assert!(!pulls[0].conflicted);
    assert_eq!(pulls[0].refreshed_at, hub.clock.now());
}

#[tokio::test]
async fn a_task_not_cached_yet_is_read_once_and_a_task_without_a_pull_request_reads_nothing() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let task = task_with_pull_request(&hub).await;
    github.set_pull_request("acme/widgets", 42, Ok(conflicted_draft(&"a".repeat(40))));
    assert_eq!(pull_requests(&hub, &task).await.len(), 1);
    assert_eq!(pull_requests(&hub, &task).await.len(), 1);
    assert_eq!(state_reads(&github), 1);

    let (other, lease) = leased_task_for(&hub, &hub.general().await).await;
    hub.db.release_lease(&lease).await.unwrap();
    assert!(pull_requests(&hub, &other).await.is_empty());
    assert_eq!(state_reads(&github), 1);

    let missing = hub
        .api
        .overview(OverviewRequest::TaskPullRequests {
            task: threadlane_core::ids::TaskId(999),
        })
        .await;
    assert!(matches!(missing, Err(ApiError::NotFound(_))), "{missing:?}");
}

#[tokio::test]
async fn a_tasks_pull_request_files_are_read_once_per_head() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let task = task_with_pull_request(&hub).await;
    github.set_pull_request("acme/widgets", 42, Ok(conflicted_draft(&"a".repeat(40))));
    let file = PullRequestFile {
        path: "src/page.rs".into(),
        previous_path: None,
        status: "added".into(),
        additions: 2,
        deletions: 0,
        patch: Some("@@ -0,0 +1,2 @@\n+fn page() {}\n+".into()),
    };
    github.set_pull_request_files("acme/widgets", 42, vec![file.clone()]);
    let files = || async {
        match hub
            .api
            .overview(OverviewRequest::TaskPullFiles { task: task.id })
            .await
            .unwrap()
        {
            OverviewReply::TaskPullFiles { pulls } => pulls,
            other => panic!("{other:?}"),
        }
    };
    let file_reads = || {
        github
            .calls()
            .iter()
            .filter(|call| matches!(call, GithubCall::PullRequestFiles { .. }))
            .count()
    };
    let expected = PullRequestFiles {
        repository: "acme/widgets".into(),
        number: 42,
        head_sha: "a".repeat(40),
        files: vec![file],
    };
    assert_eq!(files().await, std::slice::from_ref(&expected));
    assert_eq!(files().await, [expected]);
    assert_eq!(file_reads(), 1);

    github.set_pull_request("acme/widgets", 42, Ok(conflicted_draft(&"b".repeat(40))));
    hub.core.poll_github_once().await.unwrap();
    assert_eq!(files().await[0].head_sha, "b".repeat(40));
    assert_eq!(file_reads(), 2, "a new head is read again");
}

#[tokio::test]
async fn a_failed_first_read_of_a_tasks_pull_requests_is_tried_again() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let task = task_with_pull_request(&hub).await;
    github.set_pull_request("acme/widgets", 42, Err("rate limited".into()));
    assert!(pull_requests(&hub, &task).await.is_empty());
    github.set_pull_request("acme/widgets", 42, Ok(conflicted_draft(&"a".repeat(40))));
    assert_eq!(pull_requests(&hub, &task).await.len(), 1, "read again");
    assert_eq!(pull_requests(&hub, &task).await.len(), 1);
    assert_eq!(state_reads(&github), 2, "then cached");
}

#[tokio::test]
async fn files_read_while_the_head_moved_are_not_cached_or_returned() {
    let github = FakeGithub::default();
    let (hub, _) = start(&github, Arc::new(NoHooks)).await;
    let task = task_with_pull_request(&hub).await;
    github.set_pull_request("acme/widgets", 42, Ok(conflicted_draft(&"a".repeat(40))));
    assert_eq!(pull_requests(&hub, &task).await[0].head_sha, "a".repeat(40));
    github.set_pull_request("acme/widgets", 42, Ok(conflicted_draft(&"b".repeat(40))));
    github.set_pull_request_files("acme/widgets", 42, Vec::new());
    let files = || {
        hub.api
            .overview(OverviewRequest::TaskPullFiles { task: task.id })
    };
    let moved = files().await;
    assert!(matches!(moved, Err(ApiError::Unreachable(_))), "{moved:?}");
    match files().await.unwrap() {
        OverviewReply::TaskPullFiles { pulls } => assert_eq!(pulls[0].head_sha, "b".repeat(40)),
        other => panic!("{other:?}"),
    }
}
