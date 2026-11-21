//! Milestone 9 stage A in the hub: standing rules, a tier 3 rule outside
//! Autonomous, decision expiry, ceilings and overrides through the client
//! interface (docs/milestone-9.md §2).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::client::{
    ApiError, DecisionReply, DecisionRequest, OrgReply, OrgRequest, StandingRuleSummary,
};
use threadlane_core::github::PullRequestState;
use threadlane_core::ids::{DecisionId, StandingRuleId};
use threadlane_core::link::Begin;
use threadlane_core::link::RunAssignment;
use threadlane_core::model::{
    Cell, Decision, DecisionStatus, EventKind, Lease, Mode, NewAgent, NewBinding, NewRepository,
    NewTask, OutboxKind, TaskOrigin, TaskStatus, Trust,
};
use threadlane_core::runtime::NoHooks;
use threadlane_core::runtime::steps::scheduler;
use threadlane_core::store::{
    DecisionStore, InboxStore, OrgStore, OutboxStore, TaskStore, TimerStore,
};
use threadlane_core::time::Clock;
use threadlane_core::tools::{Tier, ToolDefinition, ToolError};
use threadlane_hub::{CombinedTools, HubSettings, LEAD_NAME, MEMORY_CREATE};
use threadlane_store::{Bootstrap, HubDb, NO_RESPONSE};
use threadlane_testkit::fakes::{FakeGithub, FakeTools, GithubCall};
use threadlane_testkit::{CrashPlan, FakeClock};

use crate::common::{TestHub, begin};

/// Lets the space's projects go up to Autonomous.
async fn autonomous_space(hub: &TestHub) {
    let mut space = hub.db.space().await.unwrap();
    space.mode_ceiling = Mode::Autonomous;
    hub.db.update_space(&space).await.unwrap();
}

/// A run of General's lead on an event you did not send, so a protected
/// memory write asks (docs/milestone-5.md).
async fn unrequested_run(hub: &TestHub) -> (Lease, RunAssignment) {
    hub.insert(|event| event.trust = Trust::System).await;
    begin(&*hub.link).await
}

/// Writes the project's instructions file, or another protected one.
fn write(path: &str, content: &str) -> Json {
    json!({ "path": path, "content": content })
}

async fn call(
    hub: &TestHub,
    lease: &Lease,
    run: &RunAssignment,
    id: &str,
    tool: &str,
    arguments: &Json,
) -> Result<Json, ToolError> {
    hub.link
        .call_tool(lease, run.run_id, Some(id), tool, arguments)
        .await
}

async fn decision(hub: &TestHub, id: i64) -> Decision {
    hub.db.decision(DecisionId(id)).await.unwrap().unwrap()
}

async fn decide(hub: &TestHub, request: DecisionRequest) -> Result<DecisionReply, ApiError> {
    hub.api.decisions(request).await
}

async fn rules(hub: &TestHub) -> Vec<StandingRuleSummary> {
    match decide(hub, DecisionRequest::StandingRules { project: None }).await {
        Ok(DecisionReply::StandingRules { rules }) => rules,
        other => panic!("not rules: {other:?}"),
    }
}

/// Decision cards queued for you.
async fn cards(hub: &TestHub) -> usize {
    hub.db
        .deliverable(hub.clock.now(), 100)
        .await
        .unwrap()
        .iter()
        .filter(|message| message.kind == OutboxKind::Decision)
        .count()
}

#[tokio::test]
async fn a_rule_removes_the_next_identical_ask_but_not_another_target_and_a_revoked_one_asks_again()
{
    let hub = TestHub::start().await;
    hub.hello().await;
    let (lease, run) = unrequested_run(&hub).await;
    let instructions = "project/instructions.md";

    // Asked: the decision offers a rule for this file.
    let first = call(
        &hub,
        &lease,
        &run,
        "c-1",
        MEMORY_CREATE,
        &write(instructions, "A"),
    )
    .await;
    assert_eq!(first, Err(ToolError::WaitingOnYou));
    assert!(decision(&hub, 1).await.offers_standing_rule);
    let answered = decide(
        &hub,
        DecisionRequest::AnswerAlways {
            decision: DecisionId(1),
            option: "approve".into(),
            note: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        answered,
        DecisionReply::Answered { recorded: true, .. }
    ));
    let listed = rules(&hub).await;
    assert_eq!(listed.len(), 1);
    let rule = &listed[0];
    assert_eq!(
        (rule.agent.as_str(), rule.project.as_str()),
        (LEAD_NAME, "General")
    );
    assert_eq!(rule.rule.tool, MEMORY_CREATE);
    assert_eq!(rule.rule.pattern, json!({ "path": instructions }));
    assert_eq!(rule.rule.tier, Tier::Local);
    assert_eq!(rule.rule.created_from_decision, Some(DecisionId(1)));
    assert_eq!(rule.rule.uses, 0);

    // The same file again, other content: the rule answers, no card.
    let second = call(
        &hub,
        &lease,
        &run,
        "c-2",
        MEMORY_CREATE,
        &write(instructions, "B"),
    )
    .await;
    assert_eq!(
        second,
        Err(ToolError::AuthorizedAndQueued),
        "the run ends to perform it"
    );
    let by_rule = decision(&hub, 2).await;
    assert!(by_rule.approved(), "{by_rule:?}");
    assert_eq!(by_rule.expires_at, None);
    assert_eq!(cards(&hub).await, 1, "only the first was asked");
    assert_eq!(rules(&hub).await[0].rule.uses, 1);
    let answer = hub
        .db
        .pending_events(lease.lane_id)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.decision_ref == Some(DecisionId(2)))
        .expect("the rule's answer resumes the session");
    assert!(
        answer.payload["text"]
            .as_str()
            .unwrap()
            .starts_with("Standing rule 1 allowed D-0002"),
        "{}",
        answer.payload
    );

    // Another target asks.
    let other = call(
        &hub,
        &lease,
        &run,
        "c-3",
        MEMORY_CREATE,
        &write("space/instructions.md", "C"),
    )
    .await;
    assert_eq!(other, Err(ToolError::WaitingOnYou));
    assert_eq!(decision(&hub, 3).await.status, DecisionStatus::Pending);

    // Revoked: the same file asks again.
    let revoked = decide(
        &hub,
        DecisionRequest::RevokeRule {
            rule: StandingRuleId(1),
        },
    )
    .await
    .unwrap();
    let DecisionReply::StandingRule { rule } = revoked else {
        panic!("not a rule: {revoked:?}");
    };
    assert!(rule.rule.revoked_at.is_some());
    let again = call(
        &hub,
        &lease,
        &run,
        "c-4",
        MEMORY_CREATE,
        &write(instructions, "D"),
    )
    .await;
    assert_eq!(again, Err(ToolError::WaitingOnYou));
    assert_eq!(decision(&hub, 4).await.status, DecisionStatus::Pending);
    assert_eq!(cards(&hub).await, 3);
    assert_eq!(rules(&hub).await[0].rule.uses, 1);
}

#[tokio::test]
async fn always_is_refused_where_no_rule_could_apply() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let (lease, run) = unrequested_run(&hub).await;
    let asked = call(
        &hub,
        &lease,
        &run,
        "c-1",
        MEMORY_CREATE,
        &write("project/instructions.md", "A"),
    )
    .await;
    assert_eq!(asked, Err(ToolError::WaitingOnYou));
    let always = |option: &str| DecisionRequest::AnswerAlways {
        decision: DecisionId(1),
        option: option.into(),
        note: None,
    };
    assert!(
        matches!(
            decide(&hub, always("decline")).await,
            Err(ApiError::Invalid(_))
        ),
        "always only approves"
    );
    // An agent's routine offers no rule.
    let routine = json!({
        "instruction": "Check the build.",
        "rule": "daily",
        "hour": 9,
        "minute": 0,
        "zone": "UTC",
    });
    let asked = call(
        &hub,
        &lease,
        &run,
        "c-2",
        threadlane_hub::CREATE_SCHEDULE,
        &routine,
    )
    .await;
    assert_eq!(asked, Err(ToolError::WaitingOnYou));
    assert!(!decision(&hub, 2).await.offers_standing_rule);
    let refused = decide(
        &hub,
        DecisionRequest::AnswerAlways {
            decision: DecisionId(2),
            option: "approve".into(),
            note: None,
        },
    )
    .await;
    assert!(matches!(refused, Err(ApiError::Invalid(_))), "{refused:?}");
    assert_eq!(decision(&hub, 2).await.status, DecisionStatus::Pending);
    assert!(rules(&hub).await.is_empty());
}

#[tokio::test]
async fn a_tier_three_rule_has_no_effect_outside_autonomous() {
    let base = TestHub::start().await;
    autonomous_space(&base).await;
    let fake = FakeTools::new();
    fake.register(
        ToolDefinition {
            name: "release".into(),
            description: "Publish a release.".into(),
            input_schema: json!({ "type": "object" }),
            tier: Tier::SharedHardToUndo,
            safe_to_repeat: false,
        },
        Ok(json!({ "merged": true })),
    );
    let tools = CombinedTools::new(vec![base.core.tools(), Arc::new(fake.clone())]);
    let hub = TestHub::around(
        base.core.clone().with_tools(Arc::new(tools)),
        base.clock.clone(),
    )
    .await;
    hub.hello().await;
    hub.send("m-1", "merge it").await;
    let (lease, run) = begin(&*hub.link).await;
    let merge = json!({ "repository": "app" });

    // On request: asked, with no rule offered, since it would never apply.
    assert_eq!(
        call(&hub, &lease, &run, "c-0", "release", &merge).await,
        Err(ToolError::WaitingOnYou)
    );
    assert!(!decision(&hub, 1).await.offers_standing_rule);
    // Autonomous: asked, with a rule.
    hub.api
        .org(OrgRequest::SetMode {
            project: None,
            agent: LEAD_NAME.into(),
            mode: Mode::Autonomous,
        })
        .await
        .unwrap();
    assert_eq!(
        call(&hub, &lease, &run, "c-1", "release", &merge).await,
        Err(ToolError::WaitingOnYou)
    );
    assert!(decision(&hub, 2).await.offers_standing_rule);
    decide(
        &hub,
        DecisionRequest::AnswerAlways {
            decision: DecisionId(2),
            option: "approve".into(),
            note: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(rules(&hub).await[0].rule.tier, Tier::SharedHardToUndo);

    // Still asked On request and Assisted; in Autonomous it is reviewed,
    // which falls back to a card when no reviewer is available.
    for (n, mode) in [Mode::OnRequest, Mode::Assisted, Mode::Autonomous]
        .into_iter()
        .enumerate()
    {
        hub.api
            .org(OrgRequest::SetMode {
                project: None,
                agent: LEAD_NAME.into(),
                mode,
            })
            .await
            .unwrap();
        let id = format!("c-{}", n + 2);
        assert_eq!(
            call(&hub, &lease, &run, &id, "release", &merge).await,
            Err(ToolError::WaitingOnYou),
            "{mode}"
        );
        let asked = decision(&hub, i64::try_from(n).unwrap() + 3).await;
        assert_eq!(asked.status, DecisionStatus::Pending, "{mode}");
    }
    assert_eq!(rules(&hub).await[0].rule.uses, 0);
    assert!(fake.calls().is_empty());
}

/// A pending decision in General's lead lane, asked one hour before it
/// expires.
async fn asked_with_expiry(hub: &TestHub) -> Decision {
    hub.hello().await;
    let (lease, run) = unrequested_run(hub).await;
    let asked = call(
        hub,
        &lease,
        &run,
        "c-1",
        MEMORY_CREATE,
        &write("project/instructions.md", "A"),
    )
    .await;
    assert_eq!(asked, Err(ToolError::WaitingOnYou));
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    decision(hub, 1).await
}

fn hourly() -> HubSettings {
    HubSettings {
        decision_expiry: Duration::from_secs(3600),
        ..HubSettings::default()
    }
}

/// Whether the decision is expired as declined with "no response", with
/// exactly one answer event.
async fn assert_expired_once(hub: &TestHub, what: &str) {
    let expired = decision(hub, 1).await;
    assert_eq!(expired.status, DecisionStatus::Expired, "{what}");
    assert!(!expired.approved(), "{what}");
    assert_eq!(
        expired.chosen().map(|option| option.label.as_str()),
        Some("decline"),
        "{what}"
    );
    assert_eq!(
        expired.answer.as_ref().unwrap()["note"],
        json!(NO_RESPONSE),
        "{what}"
    );
    let lane = hub.general().await.lead_lane_id;
    let answers: Vec<_> = hub
        .db
        .pending_events(lane)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == EventKind::DecisionAnswer)
        .collect();
    assert_eq!(answers.len(), 1, "{what}: one answer event");
    assert!(
        answers[0].payload["text"]
            .as_str()
            .unwrap()
            .contains("expired with no response"),
        "{what}: {}",
        answers[0].payload
    );
}

#[tokio::test]
async fn a_decision_expires_as_declined_once() {
    let hub = TestHub::with(hourly(), Arc::new(NoHooks)).await;
    let asked = asked_with_expiry(&hub).await;
    assert_eq!(
        asked.expires_at,
        Some(asked.created_at + Duration::from_secs(3600))
    );
    let general = hub.general().await;
    assert!(
        hub.db.project_timers(general.id).await.unwrap().is_empty(),
        "the expiry is the system's, not a routine of the project"
    );
    hub.clock.advance(Duration::from_secs(3599));
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0, "not yet");
    assert_eq!(decision(&hub, 1).await.status, DecisionStatus::Pending);
    hub.clock.advance(Duration::from_secs(1));
    assert_eq!(hub.core.schedule_once().await.unwrap(), 1);
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0, "once");
    assert_expired_once(&hub, "expired").await;

    // An answer after it expired is ignored.
    let late = decide(
        &hub,
        DecisionRequest::Answer {
            decision: DecisionId(1),
            option: "approve".into(),
            note: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        late,
        DecisionReply::Answered {
            recorded: false,
            ..
        }
    ));
    assert_expired_once(&hub, "after a late answer").await;
}

#[tokio::test]
async fn an_answered_decision_does_not_expire() {
    let hub = TestHub::with(hourly(), Arc::new(NoHooks)).await;
    hub.dreaming_off(None).await;
    asked_with_expiry(&hub).await;
    decide(
        &hub,
        DecisionRequest::Answer {
            decision: DecisionId(1),
            option: "approve".into(),
            note: None,
        },
    )
    .await
    .unwrap();
    hub.clock.advance(Duration::from_secs(7200));
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    assert!(decision(&hub, 1).await.approved());
}

#[tokio::test]
async fn a_kill_at_each_scheduler_step_still_expires_a_decision_once() {
    for &step in scheduler::ALL {
        let plan = CrashPlan::kill_at(step);
        let hub = TestHub::with(hourly(), Arc::new(plan.clone())).await;
        asked_with_expiry(&hub).await;
        hub.clock.advance(Duration::from_secs(3600));
        assert!(hub.core.schedule_once().await.is_err(), "killed at {step}");
        assert_eq!(plan.killed(), Some(step));

        let restarted = hub.restart(Arc::new(NoHooks)).await;
        restarted.core.schedule_once().await.unwrap();
        restarted.core.schedule_once().await.unwrap();
        assert_expired_once(&restarted, &format!("killed at {step}")).await;
    }
}

async fn org(hub: &TestHub, request: OrgRequest) -> Result<OrgReply, ApiError> {
    hub.api.org(request).await
}

fn set_mode(mode: Mode) -> OrgRequest {
    OrgRequest::SetMode {
        project: None,
        agent: LEAD_NAME.into(),
        mode,
    }
}

fn set_override(tier: Tier, cell: Option<Cell>) -> OrgRequest {
    OrgRequest::SetOverride {
        project: None,
        agent: LEAD_NAME.into(),
        tier,
        cell,
    }
}

/// The error message of a refused request.
fn refused(reply: Result<OrgReply, ApiError>) -> String {
    match reply {
        Err(ApiError::Invalid(message)) => message,
        other => panic!("not refused: {other:?}"),
    }
}

#[tokio::test]
async fn a_mode_above_the_ceiling_is_refused_with_the_ceiling_named() {
    let hub = TestHub::start().await;
    // The space starts at On request.
    let message = refused(org(&hub, set_mode(Mode::Assisted)).await);
    assert!(message.contains("on_request"), "{message}");

    autonomous_space(&hub).await;
    assert!(org(&hub, set_mode(Mode::Autonomous)).await.is_ok());
    assert_eq!(
        org(
            &hub,
            OrgRequest::SetCeiling {
                project: None,
                mode: Mode::Assisted,
            },
        )
        .await,
        Ok(OrgReply::Ceiling {
            project: "General".into(),
            ceiling: Mode::Assisted,
        })
    );
    // The binding keeps its stored mode and acts at the ceiling; a new one
    // above it is refused.
    let lead = hub.db.lead_binding(hub.general().await.id).await.unwrap();
    assert_eq!(lead.unwrap().mode, Mode::Autonomous);
    let message = refused(org(&hub, set_mode(Mode::Autonomous)).await);
    assert!(
        message.contains("\"General\"") && message.contains("assisted"),
        "{message}"
    );
    assert!(org(&hub, set_mode(Mode::Assisted)).await.is_ok());

    // A project cannot lift itself above the space.
    let mut space = hub.db.space().await.unwrap();
    space.mode_ceiling = Mode::OnRequest;
    hub.db.update_space(&space).await.unwrap();
    let message = refused(
        org(
            &hub,
            OrgRequest::SetCeiling {
                project: None,
                mode: Mode::Assisted,
            },
        )
        .await,
    );
    assert!(message.contains("space"), "{message}");
}

#[tokio::test]
async fn an_override_is_stricter_freely_and_looser_only_to_the_ceiling() {
    let hub = TestHub::start().await;
    autonomous_space(&hub).await;
    // Stricter: reads are blocked for the lead.
    assert_eq!(
        org(&hub, set_override(Tier::Read, Some(Cell::Block))).await,
        Ok(OrgReply::Overrides {
            agent: LEAD_NAME.into(),
            overrides: [(Tier::Read, Cell::Block)].into(),
        })
    );
    // Looser than the ceiling's column allows: refused.
    let message = refused(
        org(
            &hub,
            set_override(Tier::SharedReversible, Some(Cell::Allow)),
        )
        .await,
    );
    assert!(
        message.contains("autonomous") && message.contains("review"),
        "{message}"
    );
    assert!(
        org(
            &hub,
            set_override(Tier::SharedReversible, Some(Cell::Review))
        )
        .await
        .is_ok()
    );

    // The gate reads it: a read is now blocked.
    hub.hello().await;
    hub.send("m-1", "list the tasks").await;
    let (lease, run) = begin(&*hub.link).await;
    let listed = call(
        &hub,
        &lease,
        &run,
        "c-1",
        threadlane_hub::LIST_TASKS,
        &json!({}),
    )
    .await;
    assert!(matches!(listed, Err(ToolError::Blocked(_))), "{listed:?}");

    // Cleared, it is allowed again.
    assert_eq!(
        org(&hub, set_override(Tier::Read, None)).await,
        Ok(OrgReply::Overrides {
            agent: LEAD_NAME.into(),
            overrides: [(Tier::SharedReversible, Cell::Review)].into(),
        })
    );
    let listed = call(
        &hub,
        &lease,
        &run,
        "c-2",
        threadlane_hub::LIST_TASKS,
        &json!({}),
    )
    .await;
    assert!(listed.is_ok(), "{listed:?}");
}

/// The head the Reviewer approved in [`own_merge`].
fn approved_head() -> String {
    "a".repeat(40)
}

/// Pull request 42 of `acme/widgets` from `branch`, at `head`, mergeable,
/// with one check that ended as `check`.
fn widgets_pull(branch: &str, head: &str, check: &str) -> PullRequestState {
    PullRequestState {
        head_repository: "acme/widgets".into(),
        head_ref: branch.into(),
        head_sha: head.into(),
        draft: false,
        mergeable_state: "clean".into(),
        checks: vec![Some(check.into())],
    }
}

/// A task of `origin` in `widgets` on GitHub, worked by an Assisted
/// Engineer, whose submission at [`approved_head`] the Reviewer approved
/// while you asked for more, so the Engineer runs again. GitHub answers
/// `pull` for pull request 42. Returns the hub, GitHub, and the Engineer's
/// lease and run after the approval.
async fn own_merge(
    origin: TaskOrigin,
    pull: PullRequestState,
) -> (TestHub, FakeGithub, Lease, RunAssignment) {
    let github = FakeGithub::default();
    let clock = FakeClock::at_default_start();
    let db = HubDb::open_in_memory(&Bootstrap::default(), Arc::new(clock.clone()))
        .await
        .unwrap();
    let core = threadlane_hub::HubCore::new(
        db,
        Arc::new(clock.clone()),
        Arc::new(NoHooks),
        HubSettings {
            reviewer: Some("Reviewer".into()),
            ..HubSettings::default()
        },
    )
    .await
    .unwrap()
    .with_github(Arc::new(github.clone()));
    let hub = TestHub::around(core, clock).await;
    hub.hello().await;
    autonomous_space(&hub).await;
    let project = hub.general().await;
    let engineer = hub
        .db
        .create_agent(&NewAgent::named("Engineer", "engineer"))
        .await
        .unwrap();
    hub.db
        .bind(&NewBinding {
            project_id: project.id,
            agent_id: engineer.id,
            mode: Mode::Assisted,
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    hub.db
        .create_agent(&NewAgent::named("Reviewer", "reviewer"))
        .await
        .unwrap();
    let widgets = hub
        .db
        .create_repository(&NewRepository {
            project_id: project.id,
            name: "widgets".into(),
            remote_url: "https://github.com/acme/widgets.git".into(),
            default_branch: "main".into(),
            protected_branches: vec![],
        })
        .await
        .unwrap();
    let task = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: project.id,
                agent_id: engineer.id,
                title: "Fix widgets".into(),
                goal: "Fix the widgets and merge them".into(),
                repositories: vec![widgets.id],
                parent_task_id: None,
                depends_on: vec![],
                origin,
                status: TaskStatus::Queued,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
            },
        )
        .await
        .unwrap();
    github.set_pull_request("acme/widgets", 42, Ok(pull));

    let (lease, implementation) = begin(&*hub.link).await;
    call(
        &hub,
        &lease,
        &implementation,
        "progress",
        MEMORY_CREATE,
        &json!({ "path": format!("project/tasks/{}.md", task.id), "content": "# Fix\nDone." }),
    )
    .await
    .unwrap();
    call(
        &hub,
        &lease,
        &implementation,
        "submit",
        threadlane_hub::FINISH_TASK,
        &json!({"summary":"Fixed","merge":[{"repository":"widgets","branch":"main","base":"main","conflicts":[],"revision":approved_head()}]}),
    )
    .await
    .unwrap();
    hub.link
        .complete(&lease, implementation.run_id, None)
        .await
        .unwrap();
    let Begin::Run(review) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("the review never started")
    };
    call(
        &hub,
        &lease,
        &review,
        "approval",
        "finish_review",
        &json!({"verdict":"approved","feedback":"Looks right","checks":"rerun"}),
    )
    .await
    .unwrap();
    // You asked for more meanwhile, so the approval sends the task back.
    hub.insert(|event| {
        event.lane_id = task.lane_id;
        event.kind = EventKind::UserMessage;
        event.trust = Trust::User;
        event.payload = json!({ "text": "Merge it once it is approved" });
    })
    .await;
    hub.link
        .complete(&lease, review.run_id, None)
        .await
        .unwrap();
    let Begin::Run(run) = hub.link.begin_run(&lease).await.unwrap() else {
        panic!("the Engineer never ran again")
    };
    assert_eq!(run.task_id, Some(task.id));
    (hub, github, lease, *run)
}

/// How many times GitHub was asked for a pull request's state.
fn pull_reads(github: &FakeGithub) -> usize {
    github
        .calls()
        .iter()
        .filter(|call| matches!(call, GithubCall::PullRequestState { .. }))
        .count()
}

#[tokio::test]
async fn a_requested_task_merges_its_own_green_approved_pull_request_without_asking() {
    let branch = threadlane_core::workspace::task_branch(threadlane_core::ids::TaskId(1));
    let (hub, github, lease, run) = own_merge(
        TaskOrigin::User,
        widgets_pull(&branch, &approved_head(), "success"),
    )
    .await;
    let merge = json!({"repository": "widgets", "number": 42});
    let merged = call(&hub, &lease, &run, "merge", threadlane_hub::MERGE, &merge).await;
    assert_eq!(merged, Err(ToolError::AuthorizedAndQueued));
    let decision = hub
        .db
        .lane_decisions(lease.lane_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(decision.tool, threadlane_hub::MERGE);
    assert!(decision.approved());
    assert_eq!(decision.answer.as_ref().unwrap()["by_default"], true);
    assert_eq!(cards(&hub).await, 0, "nothing reaches you");
    assert_eq!(pull_reads(&github), 1);
    // A repeated call replays its outcome and reads GitHub no more.
    let again = call(&hub, &lease, &run, "merge", threadlane_hub::MERGE, &merge).await;
    assert_eq!(again, Err(ToolError::AuthorizedAndQueued));
    assert_eq!(pull_reads(&github), 1);
}

#[tokio::test]
async fn an_own_merge_asks_when_a_check_fails_or_another_head_was_approved() {
    let branch = threadlane_core::workspace::task_branch(threadlane_core::ids::TaskId(1));
    for pull in [
        widgets_pull(&branch, &approved_head(), "failure"),
        widgets_pull(&branch, &"b".repeat(40), "success"),
        PullRequestState {
            mergeable_state: "dirty".into(),
            ..widgets_pull(&branch, &approved_head(), "success")
        },
    ] {
        let (hub, github, lease, run) = own_merge(TaskOrigin::User, pull.clone()).await;
        let merged = call(
            &hub,
            &lease,
            &run,
            "merge",
            threadlane_hub::MERGE,
            &json!({"repository": "widgets", "number": 42}),
        )
        .await;
        assert_eq!(merged, Err(ToolError::WaitingOnYou), "{pull:?}");
        assert_eq!(cards(&hub).await, 1, "{pull:?}");
        assert_eq!(pull_reads(&github), 1, "{pull:?}");
    }
}

#[tokio::test]
async fn another_branchs_pull_request_or_work_you_did_not_request_asks_as_before() {
    let branch = threadlane_core::workspace::task_branch(threadlane_core::ids::TaskId(1));
    let fork = PullRequestState {
        head_repository: "stranger/widgets".into(),
        ..widgets_pull(&branch, &approved_head(), "success")
    };
    for (origin, pull, reads) in [
        (
            TaskOrigin::User,
            widgets_pull("threadlane/T-0002", &approved_head(), "success"),
            1,
        ),
        (TaskOrigin::User, fork, 1),
        (
            TaskOrigin::Agent,
            widgets_pull(&branch, &approved_head(), "success"),
            0,
        ),
    ] {
        let (hub, github, lease, run) = own_merge(origin, pull.clone()).await;
        let merged = call(
            &hub,
            &lease,
            &run,
            "merge",
            threadlane_hub::MERGE,
            &json!({"repository": "widgets", "number": 42}),
        )
        .await;
        assert_eq!(merged, Err(ToolError::WaitingOnYou), "{origin:?} {pull:?}");
        assert_eq!(cards(&hub).await, 1, "{origin:?} {pull:?}");
        assert_eq!(pull_reads(&github), reads, "{origin:?} {pull:?}");
    }
}

#[tokio::test]
async fn an_unreadable_pull_request_asks_as_before() {
    let (hub, github, lease, run) = own_merge(TaskOrigin::User, PullRequestState::default()).await;
    github.set_pull_request("acme/widgets", 42, Err("rate limited".into()));
    let merged = call(
        &hub,
        &lease,
        &run,
        "merge",
        threadlane_hub::MERGE,
        &json!({"repository": "widgets", "number": 42}),
    )
    .await;
    assert_eq!(merged, Err(ToolError::WaitingOnYou));
    assert_eq!(cards(&hub).await, 1);
}
