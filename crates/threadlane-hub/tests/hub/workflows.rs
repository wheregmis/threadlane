//! Public client requests against the real hub and its in-memory Turso store.
use super::common::{TestHub, begin};
use serde_json::json;
use threadlane_core::client::{
    OrgRequest, OverviewReply, OverviewRequest, TaskReply, TaskRequest, TimerRequest,
};
use threadlane_core::model::{Recurrence, Rule, TimerStatus};
use threadlane_core::store::{OrgStore, TaskStore, TimerStore};
use threadlane_core::work::ReviewRecipe;
use threadlane_hub::CREATE_TASK;

fn schedule() -> Recurrence {
    Recurrence {
        rule: Rule::Daily { hour: 9, minute: 0 },
        zone: "America/Toronto".into(),
    }
}

#[tokio::test]
async fn paused_team_runs_once_with_exact_history_and_resumes_on_its_original_cadence() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let project = hub.general().await;
    let preset = TaskRequest::InstallReviewTeam {
        project: project.name.clone(),
        engineer: "Builder".into(),
        reviewer: "Critic".into(),
        recurrence: schedule(),
    };
    let TaskReply::ReviewTeam { timer } = hub.api.tasks(preset.clone()).await.unwrap() else {
        panic!("expected routine")
    };
    assert_eq!(timer.status, TimerStatus::Paused);
    assert_eq!(
        hub.api.tasks(preset).await.unwrap(),
        TaskReply::ReviewTeam {
            timer: timer.clone()
        }
    );
    let manual = TimerRequest::RunOnce {
        timer: timer.id,
        client_request_id: "preview".into(),
    };
    hub.api.timers(manual.clone()).await.unwrap();
    hub.api.timers(manual).await.unwrap();
    let after = hub.db.timer(timer.id).await.unwrap().unwrap();
    assert_eq!(
        (after.due_at, after.status, after.fire_count),
        (timer.due_at, TimerStatus::Paused, 1)
    );
    let (lease, run) = begin(&*hub.link).await;
    let child = hub
        .link
        .call_tool(
            &lease,
            run.run_id,
            Some("delegate"),
            CREATE_TASK,
            &json!({"title":"Review","goal":"Review the change","agent":"Critic"}),
        )
        .await
        .unwrap();
    hub.link.complete(&lease, run.run_id, None).await.unwrap();
    let OverviewReply::RoutineHistory { rows } = hub
        .api
        .overview(OverviewRequest::RoutineHistory {
            timer: timer.id,
            before: None,
        })
        .await
        .unwrap()
    else {
        panic!("history")
    };
    assert_eq!(rows.len(), 1);
    assert!(rows[0].manual);
    assert!(rows[0].started.is_some());
    assert_eq!(
        rows[0].tasks[0].to_string(),
        child["task_id"].as_str().unwrap()
    );
    hub.api
        .timers(TimerRequest::Resume { timer: timer.id })
        .await
        .unwrap();
    let OverviewReply::Agenda { recent, upcoming } = hub
        .api
        .overview(OverviewRequest::Agenda { tomorrow_in: None })
        .await
        .unwrap()
    else {
        panic!("agenda")
    };
    assert_eq!(recent, rows);
    assert_eq!(upcoming[0].due, timer.due_at);
    assert!(upcoming.len() >= 7);
    let OverviewReply::SpaceUsage { groups } = hub
        .api
        .overview(OverviewRequest::SpaceUsage { days: 7 })
        .await
        .unwrap()
    else {
        panic!("usage")
    };
    assert_eq!(groups.iter().map(|g| g.runs).sum::<u64>(), 1);
    assert_eq!(groups[0].cost_reports, 0);
}

#[tokio::test]
async fn saved_workflow_creates_one_pair_and_rolls_back_if_a_role_is_invalid() {
    let hub = TestHub::start().await;
    let project = hub.general().await;
    let recipe = ReviewRecipe {
        review: "Check the acceptance criteria.".into(),
        ..Default::default()
    };
    let save = TaskRequest::SaveReviewRecipe { recipe };
    let TaskReply::ReviewRecipe { recipe } = hub.api.tasks(save.clone()).await.unwrap() else {
        panic!("recipe")
    };
    assert!(hub.api.tasks(save).await.is_err());
    let preset = |reviewer: &str| TaskRequest::InstallReviewTeam {
        project: project.name.clone(),
        engineer: "Builder".into(),
        reviewer: reviewer.into(),
        recurrence: schedule(),
    };
    let lead = hub.db.agents().await.unwrap()[0].name.clone();
    assert!(hub.api.tasks(preset(&lead)).await.is_err());
    assert!(
        !hub.db
            .agents()
            .await
            .unwrap()
            .iter()
            .any(|a| a.name == "Builder")
    );
    hub.api.tasks(preset("Critic")).await.unwrap();
    hub.api
        .org(OrgRequest::AddRepository {
            project: Some(project.name.clone()),
            name: "demo".into(),
            remote_url: "https://github.com/example/demo.git".into(),
            default_branch: Some("main".into()),
        })
        .await
        .unwrap();
    let start = |reviewer: &str| TaskRequest::StartReviewWorkflow {
        project: project.name.clone(),
        goal: "Improve loading".into(),
        engineer: "Builder".into(),
        reviewer: reviewer.into(),
        repository: "demo".into(),
        version: recipe.version,
        client_request_id: "button".into(),
    };
    assert!(hub.api.tasks(start("Missing")).await.is_err());
    assert!(hub.db.tasks(project.id).await.unwrap().is_empty());
    let result = hub.api.tasks(start("Critic")).await.unwrap();
    assert_eq!(result, hub.api.tasks(start("Critic")).await.unwrap());
    let mut tasks = hub.db.tasks(project.id).await.unwrap();
    tasks.sort_by_key(|t| t.id);
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[1].depends_on, [tasks[0].id]);
    assert!(tasks[1].goal.contains("Check the acceptance criteria."));
}
