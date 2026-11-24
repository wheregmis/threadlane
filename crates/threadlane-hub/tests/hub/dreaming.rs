//! Dreaming through the client, scheduler, lead lease and tool gateway.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::client::{
    ApiError, DecisionReply, DecisionRequest, DreamingStatus, MemoryFinding, MemoryReply,
    MemoryRequest, OperationsReply, OperationsRequest, OrgRequest, TimerRequest, UserMessage,
};
use threadlane_core::link::{Begin, HubLink, RunAssignment};
use threadlane_core::model::{
    DecisionKind, EventKind, EventStatus, Lease, OutboxKind, Recurrence, Rule, RunOutcome,
    SessionStatus,
};
use threadlane_core::runtime::{
    NoHooks,
    steps::{begin_run, dreaming, scheduler, tool_call},
};
use threadlane_core::store::{
    DecisionStore, InboxStore, MemoryStore, OrgStore, SessionStore, TaskStore, TimerStore,
};
use threadlane_core::time::Clock;
use threadlane_core::tools::ToolError;
use threadlane_hub::{
    HubSettings, MEMORY_CREATE, MEMORY_DELETE, MEMORY_EDIT, MEMORY_RENAME, MEMORY_VIEW,
};
use threadlane_testkit::CrashPlan;

use crate::common::{PAST_LEASE, TestHub, begin};

const HOUR: Duration = Duration::from_secs(3600);
const SESSIONS: &str = "project_sessions";
const DECISIONS: &str = "project_decisions";

fn interval() -> Recurrence {
    Recurrence {
        rule: Rule::Interval { minutes: 60 },
        zone: "UTC".to_owned(),
    }
}

async fn status(hub: &TestHub, project: Option<String>) -> DreamingStatus {
    let MemoryReply::Dreaming { status } = hub
        .api
        .memory(MemoryRequest::Dreaming { project })
        .await
        .unwrap()
    else {
        panic!("expected dreaming status");
    };
    status
}

async fn set(hub: &TestHub, cadence: Option<Recurrence>) {
    hub.api
        .memory(MemoryRequest::SetDreaming {
            project: None,
            cadence,
        })
        .await
        .unwrap();
}

async fn findings(hub: &TestHub) -> Vec<MemoryFinding> {
    let OperationsReply::MemoryCheck { findings } = hub
        .api
        .operations(OperationsRequest::CheckMemory { project: None })
        .await
        .unwrap()
    else {
        panic!("expected checker findings");
    };
    findings
}

async fn call(
    link: &dyn HubLink,
    lease: &Lease,
    run: &RunAssignment,
    tool: &str,
    args: Json,
) -> Result<Json, ToolError> {
    link.call_tool(lease, run.run_id, None, tool, &args).await
}

async fn complete(hub: &TestHub, lease: &Lease, run: &RunAssignment) -> RunOutcome {
    let outcome = hub
        .link
        .complete(lease, run.run_id, Some("Reviewed project memory."))
        .await
        .unwrap();
    hub.link.release(lease).await.unwrap();
    outcome
}

#[tokio::test]
async fn an_unset_project_dreams_by_default_and_an_explicit_off_stays_off() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let default = Some(Recurrence::default_dreaming("UTC").rule);
    let rule = |status: DreamingStatus| status.cadence.map(|cadence| cadence.rule);
    assert_eq!(rule(status(&hub, None).await), default, "General is unset");
    set(&hub, None).await;
    hub.api
        .org(OrgRequest::CreateProject {
            name: "Other".into(),
            description: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(rule(status(&hub, Some("Other".into())).await), default);
    assert_eq!(status(&hub, None).await.cadence, None, "a new project");
    let hub = hub.restart(Arc::new(NoHooks)).await;
    assert_eq!(status(&hub, None).await.cadence, None, "a restart");
    assert_eq!(rule(status(&hub, Some("Other".into())).await), default);
}

#[tokio::test]
async fn settings_are_project_scoped_validate_cadence_and_own_the_system_timer() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let project = hub.general().await;
    set(&hub, None).await;
    assert_eq!(status(&hub, None).await, DreamingStatus::default());
    hub.clock.advance(10 * HOUR);
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    assert!(hub.link.request_work().await.unwrap().is_none());
    let bad = Recurrence {
        rule: Rule::Interval { minutes: 1 },
        zone: "UTC".into(),
    };
    assert!(matches!(
        hub.api
            .memory(MemoryRequest::SetDreaming {
                project: None,
                cadence: Some(bad)
            })
            .await,
        Err(ApiError::Invalid(_))
    ));
    assert!(matches!(
        hub.api
            .operations(OperationsRequest::CheckMemory {
                project: Some("unknown".into())
            })
            .await,
        Err(ApiError::NotFound(_))
    ));

    let daily = Recurrence {
        rule: Rule::Daily {
            hour: 11,
            minute: 30,
        },
        zone: "UTC".into(),
    };
    set(&hub, Some(daily.clone())).await;
    assert_eq!(status(&hub, None).await.cadence, Some(daily.clone()));
    let (id, _) = hub.db.dreaming_timer(project.id).await.unwrap().unwrap();
    assert_eq!(
        hub.db.timer(id).await.unwrap().unwrap().due_at,
        hub.clock.now()
    );
    assert_eq!(hub.core.schedule_once().await.unwrap(), 1);
    let (lease, first) = begin(&*hub.link).await;
    assert!(first.dreaming);
    complete(&hub, &lease, &first).await;
    let due = hub.db.timer(id).await.unwrap().unwrap().due_at;
    assert_eq!(due, hub.clock.now() + Duration::from_secs(5400));
    hub.clock.advance(HOUR);
    set(&hub, Some(daily)).await;
    assert_eq!(
        hub.db.dreaming_timer(project.id).await.unwrap().unwrap().0,
        id
    );
    assert_eq!(hub.db.timer(id).await.unwrap().unwrap().due_at, due);
    assert!(
        hub.api
            .timers(TimerRequest::Pause { timer: id })
            .await
            .is_err()
    );
    assert!(
        hub.api
            .timers(TimerRequest::Cancel { timer: id })
            .await
            .is_err()
    );
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    hub.clock.advance(Duration::from_secs(1800));
    assert_eq!(hub.core.schedule_once().await.unwrap(), 1);
    set(&hub, None).await;
    assert!(
        hub.link.request_work().await.unwrap().is_none(),
        "off removes queued occurrences"
    );
    hub.clock.advance(48 * HOUR);
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    let disabled = status(&hub, None).await;
    assert!(disabled.cadence.is_none());
    assert!(disabled.last.is_some());
}

#[tokio::test]
async fn dreams_learn_from_project_choices_and_give_the_lead_current_evidence() {
    let hub = TestHub::start().await;
    hub.hello().await;
    set(&hub, None).await;
    hub.api
        .org(OrgRequest::CreateProject {
            name: "Other".into(),
            description: String::new(),
        })
        .await
        .unwrap();

    for (index, project, question, answer, note) in [
        (1, None, "Which report format?", "Table", None),
        (2, None, "Which report format?", "Table", None),
        (
            3,
            None,
            "Which report format?",
            "Table",
            Some("For weekly reports"),
        ),
        (4, Some("Other"), "Other project preference", "Prose", None),
    ] {
        hub.api
            .send_message(&UserMessage {
                project: project.map(str::to_owned),
                text: "Prepare a weekly report".into(),
                images: Vec::new(),
                client_message_id: format!("report-{index}"),
                task: None,
            })
            .await
            .unwrap();
        let (lease, lead) = begin(&*hub.link).await;
        assert_eq!(
            call(
                &*hub.link,
                &lease,
                &lead,
                "ask_user",
                json!({
                    "question": question, "options": ["Table", "Prose"]
                })
            )
            .await,
            Err(ToolError::WaitingOnYou)
        );
        complete(&hub, &lease, &lead).await;
        let decisions = hub.db.pending_decisions(lease.lane_id).await.unwrap();
        hub.api
            .decisions(DecisionRequest::Answer {
                decision: decisions[0].id,
                option: answer.into(),
                note: note.map(str::to_owned),
            })
            .await
            .unwrap();
        let (lease, resumed) = begin(&*hub.link).await;
        complete(&hub, &lease, &resumed).await;
    }

    set(&hub, Some(interval())).await;
    assert_eq!(hub.core.schedule_once().await.unwrap(), 1);
    let (lease, dream) = begin(&*hub.link).await;
    assert!(dream.dreaming);
    let result = call(&*hub.link, &lease, &dream, DECISIONS, json!({}))
        .await
        .unwrap();
    let choices = result["recent_user_choices"].as_array().unwrap();
    assert_eq!(choices.len(), 3);
    assert!(choices.iter().all(|choice| choice["chosen"] == "Table"));
    assert!(
        choices
            .iter()
            .all(|choice| choice["question"] == "Which report format?")
    );
    assert_eq!(choices[0]["note"], "For weekly reports");
    call(
        &*hub.link,
        &lease,
        &dream,
        MEMORY_CREATE,
        json!({
            "path": "project/notes/user-preferences.md",
            "content": "# User preferences\nRecurring choices for this project.\n\nUse a table for weekly reports."
        }),
    )
    .await
    .unwrap();
    complete(&hub, &lease, &dream).await;

    hub.send("next-report", "Prepare this week's report").await;
    let (lease, lead) = begin(&*hub.link).await;
    assert!(lead.prompt.contains("Dreaming decision history"));
    assert!(lead.prompt.contains("Use a table for weekly reports."));
    assert!(lead.prompt.contains("For weekly reports"));
    assert!(lead.appended_instructions.contains("all tool approvals"));
    assert!(
        call(&*hub.link, &lease, &lead, DECISIONS, json!({}))
            .await
            .is_err()
    );
    complete(&hub, &lease, &lead).await;

    // The resumed session already holds the unchanged history.
    hub.send("same-session", "And next week's").await;
    let (lease, lead) = begin(&*hub.link).await;
    assert!(!lead.prompt.contains("Dreaming decision history"));
    complete(&hub, &lease, &lead).await;

    // After a restart the hub no longer remembers what it sent, so the resumed session is
    // still told the history is withdrawn, once.
    let hub = hub.restart(Arc::new(NoHooks)).await;
    set(&hub, None).await;
    hub.send("dreaming-off", "Prepare this week's report").await;
    let (lease, lead) = begin(&*hub.link).await;
    assert!(
        lead.prompt
            .contains("Dreaming decision history sent earlier in this session is withdrawn")
    );
    assert!(!lead.prompt.contains("Use a table for weekly reports."));
    assert!(
        !lead
            .appended_instructions
            .contains("Use a table for weekly reports.")
    );
    complete(&hub, &lease, &lead).await;
    hub.send("still-off", "And next week's").await;
    let (lease, lead) = begin(&*hub.link).await;
    assert!(!lead.prompt.contains("Dreaming decision history"));
    complete(&hub, &lease, &lead).await;
}

#[tokio::test]
async fn dream_filters_tools_scope_session_history_and_effects() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let project = hub.general().await;
    hub.api
        .org(OrgRequest::CreateProject {
            name: "Other".into(),
            description: String::new(),
        })
        .await
        .unwrap();
    let other = hub
        .db
        .projects()
        .await
        .unwrap()
        .into_iter()
        .find(|project| project.name == "Other")
        .unwrap();
    hub.dreaming_off(Some("Other")).await;
    hub.api
        .send_message(&UserMessage {
            project: Some("Other".into()),
            text: "Other project request".into(),
            images: Vec::new(),
            client_message_id: "other".into(),
            task: None,
        })
        .await
        .unwrap();
    let (lease, other_run) = begin(&*hub.link).await;
    assert_eq!(other_run.project_id, other.id);
    call(
        &*hub.link,
        &lease,
        &other_run,
        "send_message",
        json!({"text":"Other project update"}),
    )
    .await
    .unwrap();
    complete(&hub, &lease, &other_run).await;
    hub.db
        .write_by_owner(
            "space/notes/secret.md",
            "# Space\nSecret from space",
            hub.clock.now(),
        )
        .await
        .unwrap();
    let foreign = format!("projects/{}/notes/secret.md", other.id.get());
    hub.db
        .write_by_owner(
            &foreign,
            "# Other\nSecret from another project",
            hub.clock.now(),
        )
        .await
        .unwrap();
    hub.send("prior", "Give one update").await;
    let (lease, ordinary) = begin(&*hub.link).await;
    assert!(ordinary.tools.iter().all(|tool| tool.name != SESSIONS));
    assert!(
        call(&*hub.link, &lease, &ordinary, SESSIONS, json!({}))
            .await
            .is_err()
    );
    call(
        &*hub.link,
        &lease,
        &ordinary,
        "send_message",
        json!({"text":"Delivered update"}),
    )
    .await
    .unwrap();
    complete(&hub, &lease, &ordinary).await;
    hub.core.dispatch_once().await.unwrap();

    set(&hub, Some(interval())).await;
    hub.clock.advance(5 * HOUR);
    let timer = hub.db.dreaming_timer(project.id).await.unwrap().unwrap().0;
    assert_eq!(
        hub.core.schedule_once().await.unwrap(),
        1,
        "one event after downtime"
    );
    assert_eq!(hub.core.schedule_once().await.unwrap(), 0);
    let timer = hub.db.timer(timer).await.unwrap().unwrap();
    assert_eq!(timer.due_at, hub.clock.now() + HOUR);
    assert_eq!(timer.fire_count, 1);
    assert_eq!(
        status(&hub, Some("Other".into())).await,
        DreamingStatus::default()
    );
    let user_event = hub
        .send("during", "Handle a separate user request")
        .await
        .event_id;
    let (lease, dream) = begin(&*hub.link).await;
    assert!(dream.dreaming && dream.remove_builtin_tools);
    assert_ne!(dream.session_id, ordinary.session_id);
    assert_eq!(dream.events.len(), 1);
    assert!(dream.perform.is_empty());
    assert!(
        hub.link
            .folded_in(&lease, dream.run_id, &[user_event])
            .await
            .is_err()
    );
    assert!(
        hub.link
            .begin_perform(&lease, dream.run_id, threadlane_core::ids::DecisionId(999))
            .await
            .unwrap_err()
            .to_string()
            .contains("a dream cannot perform")
    );

    assert!(dream.prompt.contains("Curate this project's memory"));
    assert!(dream.appended_instructions.is_empty());
    let names: std::collections::BTreeSet<_> =
        dream.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(
        names,
        [
            SESSIONS,
            DECISIONS,
            threadlane_core::forge::REPORT_LIMITATION,
            MEMORY_VIEW,
            MEMORY_CREATE,
            MEMORY_EDIT,
            MEMORY_DELETE,
            MEMORY_RENAME
        ]
        .into()
    );
    for tool in [
        "send_message",
        "create_task",
        "set_follow_up",
        "list_tasks",
        "ask_user",
        "comment",
        "custom",
        "Bash",
    ] {
        assert!(
            matches!(
                call(&*hub.link, &lease, &dream, tool, json!({})).await,
                Err(ToolError::Blocked(_))
            ),
            "{tool}"
        );
    }
    for path in [
        "space",
        "space/notes/secret.md",
        "project/../space/notes/secret.md",
        &foreign,
    ] {
        assert!(
            call(
                &*hub.link,
                &lease,
                &dream,
                MEMORY_VIEW,
                json!({"path":path})
            )
            .await
            .is_err(),
            "{path}"
        );
        assert!(
            call(
                &*hub.link,
                &lease,
                &dream,
                MEMORY_CREATE,
                json!({"path":path,"content":"# Overwrite\nBad"})
            )
            .await
            .is_err(),
            "{path}"
        );
        assert!(
            call(
                &*hub.link,
                &lease,
                &dream,
                MEMORY_RENAME,
                json!({"from":"project/notes/shadow.md","to":path})
            )
            .await
            .is_err(),
            "{path}"
        );
    }
    assert!(
        call(
            &*hub.link,
            &lease,
            &dream,
            SESSIONS,
            json!({"project":"Other"})
        )
        .await
        .is_err()
    );
    let history = call(&*hub.link, &lease, &dream, SESSIONS, json!({}))
        .await
        .unwrap();
    assert_eq!(history["runs"].as_array().unwrap().len(), 1);
    assert_eq!(history["runs"][0]["rationale"], "Reviewed project memory.");
    assert_eq!(history["runs"][0]["outcome"], "sent");
    assert_eq!(
        history["runs"][0]["sent_messages"][0]["body"]["text"],
        "Delivered update"
    );
    assert_eq!(
        history["runs"][0]["owner_messages"][0]["text"],
        "Give one update"
    );
    let total = history["total"].as_u64().unwrap();
    assert_eq!(history["total_runs"], 1);
    assert_eq!(history.get("next_offset"), None);
    let first = call(&*hub.link, &lease, &dream, SESSIONS, json!({"limit": 1}))
        .await
        .unwrap();
    assert_eq!(first["runs"].as_array().unwrap().len(), 1);
    assert!(first["tasks"].as_array().unwrap().is_empty());
    assert_eq!(first.get("next_offset").is_some(), total > 1);
    let past = call(
        &*hub.link,
        &lease,
        &dream,
        SESSIONS,
        json!({"offset": total}),
    )
    .await
    .unwrap();
    assert!(past["runs"].as_array().unwrap().is_empty());
    assert!(past["tasks"].as_array().unwrap().is_empty());
    assert_eq!(
        hub.db
            .session(dream.session_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        SessionStatus::Closed
    );
    assert_eq!(complete(&hub, &lease, &dream).await, RunOutcome::Silent);
    hub.core.dispatch_once().await.unwrap();
    assert!(
        hub.feed()
            .await
            .iter()
            .all(|item| item.message.run_id != Some(dream.run_id)
                || item.message.kind != OutboxKind::Message)
    );
    assert!(hub.db.tasks(project.id).await.unwrap().is_empty());
    let (lease, user) = begin(&*hub.link).await;
    assert!(!user.dreaming);
    assert_ne!(user.session_id, dream.session_id);
    assert!(user.prompt.contains("Handle a separate user request"));
    complete(&hub, &lease, &user).await;

    hub.clock.advance(HOUR);
    assert_eq!(hub.core.schedule_once().await.unwrap(), 1);
    let (lease, dream) = begin(&*hub.link).await;
    let history = call(&*hub.link, &lease, &dream, SESSIONS, json!({}))
        .await
        .unwrap();
    assert_eq!(history["runs"].as_array().unwrap().len(), 1);
    assert_eq!(history["runs"][0]["run_id"], user.run_id.get());
    complete(&hub, &lease, &dream).await;
}

#[tokio::test]
async fn dream_reports_go_to_upstream_once_after_approval() {
    use threadlane_core::forge::{REPORT_LIMITATION, RepositoryAction};
    use threadlane_core::link::Perform;

    let hub = TestHub::start().await;
    hub.hello().await;
    let project = hub.general().await;
    let agent = hub.db.bindings(project.id).await.unwrap()[0].agent_id;
    hub.db
        .create_task(
            None,
            &threadlane_core::model::NewTask {
                project_id: project.id,
                agent_id: agent,
                title: "Historical task".into(),
                goal: "Proposed before the dream".into(),
                repositories: vec![],
                parent_task_id: None,
                depends_on: vec![],
                origin: threadlane_core::model::TaskOrigin::User,
                status: threadlane_core::model::TaskStatus::Proposed,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
                playbooks: Vec::new(),
            },
        )
        .await
        .unwrap();
    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let history = call(&*hub.link, &lease, &dream, SESSIONS, json!({}))
        .await
        .unwrap();
    assert_eq!(history["tasks"][0]["title"], "Historical task");
    let report = json!({"title":"Explain blocked tasks", "situation":"A blocked task gave no reason.", "suggestion":"Show the blocking dependency."});
    let mut redirected = report.clone();
    redirected["repository"] = json!("elsewhere");
    assert!(matches!(
        call(&*hub.link, &lease, &dream, REPORT_LIMITATION, redirected).await,
        Err(ToolError::InvalidArguments(_))
    ));
    assert_eq!(
        call(
            &*hub.link,
            &lease,
            &dream,
            REPORT_LIMITATION,
            report.clone()
        )
        .await,
        Err(ToolError::WaitingOnYou)
    );
    complete(&hub, &lease, &dream).await;
    let DecisionReply::Pending { decisions } =
        hub.api.decisions(DecisionRequest::Pending).await.unwrap()
    else {
        panic!("expected pending issue")
    };
    let decision = &decisions[0];
    assert!(decision.context.contains("Show the blocking dependency."));
    hub.api
        .decisions(DecisionRequest::Answer {
            decision: decision.id,
            option: "approve".into(),
            note: None,
        })
        .await
        .unwrap();
    let (lease, followup) = begin(&*hub.link).await;
    assert_eq!(followup.perform, [decision.id]);
    let Perform::Repository {
        remote,
        action: RepositoryAction::CreateIssue { title, body },
    } = hub
        .link
        .begin_perform(&lease, followup.run_id, decision.id)
        .await
        .unwrap()
    else {
        panic!("expected an issue")
    };
    assert_eq!(remote, "https://github.com/wheregmis/threadlane");
    assert_eq!(title, "Explain blocked tasks");
    assert!(body.contains("A blocked task gave no reason."));
    hub.link
        .finish_perform(
            &lease,
            followup.run_id,
            decision.id,
            &json!({"performed":true}),
        )
        .await
        .unwrap();
    complete(&hub, &lease, &followup).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let history = call(&*hub.link, &lease, &dream, SESSIONS, json!({}))
        .await
        .unwrap();
    assert_eq!(history["previous_reports"][0]["title"], title);
    assert!(matches!(
        call(&*hub.link, &lease, &dream, REPORT_LIMITATION, report).await,
        Err(ToolError::Blocked(_))
    ));
}

#[tokio::test]
async fn a_dream_report_is_reviewed_and_one_the_classifier_allows_is_filed_without_a_card() {
    use threadlane_core::forge::{REPORT_LIMITATION, RepositoryAction};
    use threadlane_core::link::Perform;
    use threadlane_core::model::BackendKind;
    use threadlane_core::store::OutboxStore;
    use threadlane_testkit::ScriptedBackend;

    let base = TestHub::start().await;
    let reviewer = ScriptedBackend::new(BackendKind::Claude, Arc::new(base.clock.clone()));
    let hub = TestHub::around(
        base.core
            .clone()
            .with_backends(vec![Arc::new(reviewer.clone())]),
        base.clock.clone(),
    )
    .await;
    hub.hello().await;
    hub.general().await;
    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let report = |title: &str| json!({"title": title, "situation": "A blocked task gave no reason.", "suggestion": "Show the blocking dependency."});

    // Asking you first still asks, without the classifier; with no
    // classifier answer, a report falls back to its card.
    let mut asking = report("Ask before filing");
    asking["require_approval"] = json!(true);
    assert_eq!(
        call(&*hub.link, &lease, &dream, REPORT_LIMITATION, asking).await,
        Err(ToolError::WaitingOnYou)
    );
    assert_eq!(
        call(
            &*hub.link,
            &lease,
            &dream,
            REPORT_LIMITATION,
            report("No answer")
        )
        .await,
        Err(ToolError::WaitingOnYou)
    );
    // The classifier allows the next: answered, and no card.
    reviewer.push_answer(Ok(
        json!({"verdict":"allow","reason":"A concrete Threadlane limitation."}),
    ));
    assert_eq!(
        call(
            &*hub.link,
            &lease,
            &dream,
            REPORT_LIMITATION,
            report("Explain blocked tasks")
        )
        .await,
        Err(ToolError::WaitingOnYou)
    );
    complete(&hub, &lease, &dream).await;
    let DecisionReply::Pending { decisions } =
        hub.api.decisions(DecisionRequest::Pending).await.unwrap()
    else {
        panic!("expected pending reports")
    };
    let titles: Vec<&str> = decisions
        .iter()
        .map(|d| d.arguments["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Ask before filing", "No answer"]);
    assert!(
        decisions.iter().all(|d| !d.urgent),
        "reports wait under Later"
    );
    let cards = hub
        .db
        .deliverable(hub.clock.now(), 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|message| message.kind == OutboxKind::Decision)
        .count();
    assert_eq!(cards, 2, "the allowed report has no card");
    let allowed = hub
        .db
        .lane_decisions(decisions[0].lane_id)
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.arguments["title"] == "Explain blocked tasks")
        .unwrap();
    assert!(allowed.approved());
    assert!(!allowed.urgent);

    // Filed the way a report you approved is: by the run that follows.
    let (lease, followup) = begin(&*hub.link).await;
    assert!(!followup.dreaming);
    assert_eq!(followup.perform, [allowed.id]);
    let Perform::Repository {
        remote,
        action: RepositoryAction::CreateIssue { title, .. },
    } = hub
        .link
        .begin_perform(&lease, followup.run_id, allowed.id)
        .await
        .unwrap()
    else {
        panic!("expected an issue")
    };
    assert_eq!(remote, "https://github.com/wheregmis/threadlane");
    assert_eq!(title, "Explain blocked tasks");
}

#[tokio::test]
async fn a_report_that_failed_to_file_is_retried_by_id_under_its_approval() {
    use threadlane_core::forge::{REPORT_LIMITATION, RepositoryAction};
    use threadlane_core::link::Perform;

    let hub = TestHub::start().await;
    hub.hello().await;
    hub.general().await;
    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let report = json!({"title":"Explain blocked tasks", "situation":"A blocked task gave no reason.", "suggestion":"Show the blocking dependency."});
    assert_eq!(
        call(&*hub.link, &lease, &dream, REPORT_LIMITATION, report).await,
        Err(ToolError::WaitingOnYou)
    );
    complete(&hub, &lease, &dream).await;
    let DecisionReply::Pending { decisions } =
        hub.api.decisions(DecisionRequest::Pending).await.unwrap()
    else {
        panic!("expected pending issue")
    };
    let first = decisions[0].id;
    hub.api
        .decisions(DecisionRequest::Answer {
            decision: first,
            option: "approve".into(),
            note: None,
        })
        .await
        .unwrap();
    let (lease, followup) = begin(&*hub.link).await;
    hub.link
        .begin_perform(&lease, followup.run_id, first)
        .await
        .unwrap();
    hub.link
        .finish_perform(
            &lease,
            followup.run_id,
            first,
            &json!({"performed":false,"error":"network unreachable"}),
        )
        .await
        .unwrap();
    complete(&hub, &lease, &followup).await;

    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let history = call(&*hub.link, &lease, &dream, SESSIONS, json!({}))
        .await
        .unwrap();
    assert_eq!(history["previous_reports"][0]["id"], first.to_string());
    assert!(matches!(
        call(
            &*hub.link,
            &lease,
            &dream,
            REPORT_LIMITATION,
            json!({"retry":"D-9999"})
        )
        .await,
        Err(ToolError::Blocked(_))
    ));
    assert_eq!(
        call(
            &*hub.link,
            &lease,
            &dream,
            REPORT_LIMITATION,
            json!({"retry": first.to_string()})
        )
        .await,
        Err(ToolError::AuthorizedAndQueued)
    );
    complete(&hub, &lease, &dream).await;
    let DecisionReply::Pending { decisions } =
        hub.api.decisions(DecisionRequest::Pending).await.unwrap()
    else {
        panic!("expected a decision list")
    };
    assert!(decisions.is_empty(), "the retry asked again: {decisions:?}");
    let (lease, followup) = begin(&*hub.link).await;
    let [retry] = followup.perform[..] else {
        panic!("expected one retry, got {:?}", followup.perform)
    };
    assert_ne!(retry, first);
    let Perform::Repository {
        action: RepositoryAction::CreateIssue { title, body },
        ..
    } = hub
        .link
        .begin_perform(&lease, followup.run_id, retry)
        .await
        .unwrap()
    else {
        panic!("expected an issue")
    };
    assert_eq!(title, "Explain blocked tasks");
    assert!(body.contains("Show the blocking dependency."));
}

#[tokio::test]
async fn instruction_changes_are_advisory_and_the_answer_does_not_wake_a_lead() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let path = format!("projects/{}/instructions.md", hub.general().await.id.get());
    hub.db
        .write_by_owner(&path, "Original", hub.clock.now())
        .await
        .unwrap();
    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    assert_eq!(
        call(
            &*hub.link,
            &lease,
            &dream,
            MEMORY_CREATE,
            json!({"path":"project/instructions.md","content":"Suggested"})
        )
        .await,
        Err(ToolError::WaitingOnYou)
    );
    assert_eq!(complete(&hub, &lease, &dream).await, RunOutcome::Deferred);
    let DecisionReply::Pending { decisions } =
        hub.api.decisions(DecisionRequest::Pending).await.unwrap()
    else {
        panic!("pending");
    };
    let proposal = &decisions[0];
    assert_eq!(proposal.kind, DecisionKind::Proposal);
    assert!(proposal.is_instruction_proposal());
    assert!(proposal.options.iter().all(|option| !option.approves()));
    for option in ["approve", "Dismiss"] {
        let result = hub
            .api
            .decisions(DecisionRequest::Answer {
                decision: proposal.id,
                option: option.into(),
                note: None,
            })
            .await;
        assert_eq!(result.is_ok(), option == "Dismiss");
    }
    assert_eq!(
        hub.db.read(&path).await.unwrap().unwrap().content,
        "Original"
    );
    assert_eq!(hub.db.history(&path).await.unwrap().len(), 1);
    assert!(hub.link.request_work().await.unwrap().is_none());
    assert!(status(&hub, None).await.last.unwrap().changes.is_empty());
}

#[tokio::test]
async fn scripted_dream_reduces_duplicates_and_missing_references_and_reports_versions() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let prefix = format!("projects/{}/notes", hub.general().await.id.get());
    for (file, content) in [
        (
            "retain",
            "# Restore\nUse the verified release artifact first.",
        ),
        (
            "duplicate",
            "# Recovery\nUSE the verified release artifact first!",
        ),
        ("near", "# Copy\nUse the verified release artifact second."),
        (
            "stale",
            "# FAQ\nSee T-0999 and [more](missing.md). Keep the retention policy.",
        ),
    ] {
        hub.db
            .write_by_owner(&format!("{prefix}/{file}.md"), content, hub.clock.now())
            .await
            .unwrap();
    }
    assert_eq!(findings(&hub).await.len(), 5);
    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    call(
        &*hub.link,
        &lease,
        &dream,
        MEMORY_DELETE,
        json!({"path":"project/notes/duplicate.md","expected_version":1}),
    )
    .await
    .unwrap();
    call(
        &*hub.link,
        &lease,
        &dream,
        MEMORY_EDIT,
        json!({
            "path":"project/notes/stale.md","expected_version":1,
            "old_text":"See T-0999 and [more](missing.md). ",
            "new_text":""
        }),
    )
    .await
    .unwrap();
    complete(&hub, &lease, &dream).await;
    assert_eq!(
        findings(&hub).await.len(),
        1,
        "one genuinely different order remains a near-match"
    );
    assert!(
        hub.db
            .read(&format!("{prefix}/near.md"))
            .await
            .unwrap()
            .unwrap()
            .content
            .contains("second")
    );
    assert_eq!(
        hub.db
            .read(&format!("{prefix}/stale.md"))
            .await
            .unwrap()
            .unwrap()
            .content,
        "# FAQ\nKeep the retention policy."
    );
    set(&hub, None).await;
    let summary = status(&hub, None).await.last.unwrap();
    assert_eq!(summary.at, hub.clock.now());
    for (name, deleted) in [("duplicate", true), ("stale", false)] {
        assert!(
            summary
                .changes
                .iter()
                .any(|change| change.path == format!("project/notes/{name}.md")
                    && change.version == 2
                    && change.deleted == deleted)
        );
    }
}

#[tokio::test]
async fn dreaming_recovers_at_setting_scheduler_run_and_tool_boundaries() {
    for step in [
        dreaming::BEFORE_SETTING,
        dreaming::AFTER_SETTING,
        scheduler::AFTER_SCAN,
        scheduler::AFTER_FIRE,
        scheduler::AFTER_WAKE,
        begin_run::AFTER_START_RUN,
        begin_run::AFTER_ASSEMBLE_CONTEXT,
        tool_call::AFTER_BEGIN,
        tool_call::AFTER_EFFECT,
        tool_call::AFTER_FINISH,
        dreaming::BEFORE_MARKER,
        dreaming::AFTER_MARKER,
    ] {
        let plan = CrashPlan::kill_at(step);
        let hub = TestHub::with(HubSettings::default(), Arc::new(plan.clone())).await;
        hub.hello().await;
        let project = hub.general().await;
        let args = json!({"path":"project/notes/test.md","content":"# Test\nDurable details"});
        let attempt = async {
            hub.api
                .memory(MemoryRequest::SetDreaming {
                    project: None,
                    cadence: Some(interval()),
                })
                .await
                .map_err(|e| e.to_string())?;
            hub.clock.advance(HOUR);
            hub.core.schedule_once().await.map_err(|e| e.to_string())?;
            let lease = hub
                .link
                .request_work()
                .await
                .map_err(|e| e.to_string())?
                .unwrap();
            let Begin::Run(run) = hub
                .link
                .begin_run(&lease)
                .await
                .map_err(|e| e.to_string())?
            else {
                panic!("dream not assigned")
            };
            call(&*hub.link, &lease, &run, MEMORY_CREATE, args.clone())
                .await
                .map_err(|e| e.to_string())?;
            hub.link
                .complete(&lease, run.run_id, None)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        };
        assert!(attempt.await.is_err(), "{step:?}");
        assert_eq!(plan.killed(), Some(step));

        let hub = hub.restart(Arc::new(NoHooks)).await;
        let enabled = status(&hub, None).await.cadence == Some(interval());
        assert_eq!(enabled, step != dreaming::BEFORE_SETTING);
        if !enabled {
            set(&hub, Some(interval())).await;
        }
        hub.clock.advance(
            if [dreaming::BEFORE_SETTING, dreaming::AFTER_SETTING].contains(&step) {
                HOUR
            } else {
                PAST_LEASE
            },
        );
        hub.core.sweep_once().await.unwrap();
        hub.core.schedule_once().await.unwrap();
        if step != dreaming::AFTER_MARKER {
            assert!(status(&hub, None).await.last.is_none(), "{step:?}");
            let (lease, dream) = begin(&*hub.link).await;
            call(&*hub.link, &lease, &dream, MEMORY_CREATE, args.clone())
                .await
                .unwrap();
            complete(&hub, &lease, &dream).await;
        }
        let events = hub.db.pending_events(project.lead_lane_id).await.unwrap();
        assert!(events.is_empty(), "{step:?}: {events:?}");
        let event = hub.event(threadlane_core::ids::EventId(1)).await;
        assert_eq!(
            (event.kind, event.status),
            (EventKind::Dream, EventStatus::Done),
            "{step:?}"
        );
        assert_eq!(hub.committed_runs_with(event.id).await, 1, "{step:?}");
        let history = hub
            .db
            .history(&format!("projects/{}/notes/test.md", project.id.get()))
            .await
            .unwrap();
        assert_eq!(history.len(), 1, "{step:?}");
        assert!(
            status(&hub, None)
                .await
                .last
                .unwrap()
                .changes
                .iter()
                .any(|change| change.path == "project/notes/test.md")
        );
    }
}

#[tokio::test]
async fn dream_refuses_stale_changes_after_an_owner_restores_a_note() {
    let hub = TestHub::start().await;
    hub.hello().await;
    let file = format!("projects/{}/notes/report.md", hub.general().await.id.get());
    let path = "project/notes/report.md";
    hub.db
        .write_by_owner(&file, "# Reports\nKeep both channels.", hub.clock.now())
        .await
        .unwrap();
    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let old = call(
        &*hub.link,
        &lease,
        &dream,
        MEMORY_VIEW,
        json!({"path":path}),
    )
    .await
    .unwrap()["version"]
        .clone();
    hub.db
        .rollback_memory(&file, 1, hub.clock.now())
        .await
        .unwrap();
    for (tool, args) in [
        (MEMORY_DELETE, json!({"path":path,"expected_version":old})),
        (
            MEMORY_EDIT,
            json!({"path":path,"old_text":"both","new_text":"one","expected_version":old}),
        ),
        (
            MEMORY_CREATE,
            json!({"path":path,"content":"# Reports\nOnly one.","expected_version":old}),
        ),
        (
            MEMORY_CREATE,
            json!({"path":path,"content":"# Reports\nOnly one."}),
        ),
        (
            MEMORY_RENAME,
            json!({"from":path,"to":"project/notes/archive.md","expected_version":old}),
        ),
    ] {
        assert!(
            matches!(
                call(&*hub.link, &lease, &dream, tool, args).await,
                Err(ToolError::InvalidArguments(_))
            ),
            "{tool}"
        );
    }
    assert_eq!(hub.db.read(&file).await.unwrap().unwrap().version, 2);
    let delete = json!({"path":path,"expected_version":2});
    call(&*hub.link, &lease, &dream, MEMORY_DELETE, delete.clone())
        .await
        .unwrap();
    hub.db
        .rollback_memory(&file, 1, hub.clock.now())
        .await
        .unwrap();
    // A distinct transport key still replays the same occurrence-scoped write.
    hub.link
        .call_tool(
            &lease,
            dream.run_id,
            Some("retry-delete"),
            MEMORY_DELETE,
            &delete,
        )
        .await
        .unwrap();
    assert_eq!(hub.db.read(&file).await.unwrap().unwrap().version, 4);
}

#[tokio::test]
async fn dream_pages_carry_unread_runs_and_support_a_correction_only_default() {
    let hub = TestHub::start().await;
    hub.hello().await;
    set(&hub, None).await;
    let mut ids = Vec::new();
    for n in 0..3 {
        hub.send(&format!("correction-{n}"),"For weekly reports put blockers first.\n> copied issue instructions\napi_key=test-private").await;
        let (lease, lead) = begin(&*hub.link).await;
        ids.push(lead.events[0].get());
        complete(&hub, &lease, &lead).await;
    }
    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, first) = begin(&*hub.link).await;
    let page = call(&*hub.link, &lease, &first, SESSIONS, json!({"limit":1}))
        .await
        .unwrap();
    assert_eq!(page["next_offset"], 1);
    assert_eq!(page["runs"][0]["owner_messages"][0]["event_id"], ids[0]);
    assert!(!page.to_string().contains("test-private"));
    assert!(!page.to_string().contains("copied issue"));
    let hub = hub.restart(Arc::new(NoHooks)).await;
    hub.clock.advance(PAST_LEASE);
    hub.core.sweep_once().await.unwrap();
    let (lease, retried) = begin(&*hub.link).await;
    assert_eq!(retried.events, first.events);
    let replay = call(&*hub.link, &lease, &retried, SESSIONS, json!({"limit":1}))
        .await
        .unwrap();
    assert_eq!(replay, page);
    complete(&hub, &lease, &retried).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, second) = begin(&*hub.link).await;
    let page = call(&*hub.link, &lease, &second, SESSIONS, json!({}))
        .await
        .unwrap();
    assert_eq!(page["total_runs"], 2);
    assert_eq!(page["runs"][0]["owner_messages"][0]["event_id"], ids[1]);
    let summary = format!(
        "# Preferences\nRecurring project defaults.\nWeekly reports start with blockers. E-{} E-{} E-{}\nUnsupported foreign default E-{} E-{} E-99999",
        ids[0], ids[1], ids[2], ids[0], ids[1]
    );
    call(
        &*hub.link,
        &lease,
        &second,
        MEMORY_CREATE,
        json!({"path":"project/notes/user-preferences.md","content":summary}),
    )
    .await
    .unwrap();
    complete(&hub, &lease, &second).await;

    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    assert!(
        hub.link.request_work().await.unwrap().is_none(),
        "idle timer used a backend"
    );
    hub.send(
        "reversal",
        "Starting next release use the checklist instead.",
    )
    .await;
    let (lease, lead) = begin(&*hub.link).await;
    assert!(lead.prompt.contains("Weekly reports start with blockers"));
    assert!(lead.prompt.contains("Starting next release"));
    assert!(!lead.prompt.contains("Unsupported foreign default"));
    assert!(!lead.prompt.contains("test-private"));
    complete(&hub, &lease, &lead).await;
}

#[tokio::test]
async fn dream_keeps_defaults_suspended_until_the_hidden_correction_is_reviewed() {
    let hub = TestHub::start().await;
    hub.hello().await;
    set(&hub, None).await;
    let project = hub.general().await.id;
    hub.send("reversal", "Stop using the previous report format.")
        .await;
    let (lease, reversal) = begin(&*hub.link).await;
    complete(&hub, &lease, &reversal).await;
    for n in 0..20 {
        hub.send(&format!("later-{n}"), "Thanks.").await;
    }
    let (lease, later) = begin(&*hub.link).await;
    complete(&hub, &lease, &later).await;

    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, first) = begin(&*hub.link).await;
    call(&*hub.link, &lease, &first, SESSIONS, json!({"offset":1}))
        .await
        .unwrap();
    complete(&hub, &lease, &first).await;
    assert!(hub.db.preferences_need_review(project).await.unwrap());

    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, second) = begin(&*hub.link).await;
    let page = call(&*hub.link, &lease, &second, SESSIONS, json!({"limit":1}))
        .await
        .unwrap();
    assert_eq!(page["runs"][0]["run_id"], reversal.run_id.get());
    assert!(hub.db.preferences_need_review(project).await.unwrap());
    complete(&hub, &lease, &second).await;
    assert!(!hub.db.preferences_need_review(project).await.unwrap());
}

#[tokio::test]
async fn dream_freezes_membership_and_pages_more_than_twenty_decisions() {
    let hub = TestHub::start().await;
    hub.hello().await;
    set(&hub, None).await;
    for index in 0..23 {
        hub.send(&format!("question-{index}"), "Prepare a weekly report.")
            .await;
        let (lease, lead) = begin(&*hub.link).await;
        assert_eq!(
            call(
                &*hub.link,
                &lease,
                &lead,
                "ask_user",
                json!({"question":"What format?","options":["Table","Prose"]})
            )
            .await,
            Err(ToolError::WaitingOnYou)
        );
        complete(&hub, &lease, &lead).await;
        let d = hub.db.pending_decisions(lease.lane_id).await.unwrap();
        hub.api
            .decisions(DecisionRequest::Answer {
                decision: d[0].id,
                option: "Table".into(),
                note: None,
            })
            .await
            .unwrap();
        let (lease, resumed) = begin(&*hub.link).await;
        complete(&hub, &lease, &resumed).await;
    }
    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, first) = begin(&*hub.link).await;
    let mut offset = 0;
    // The oldest three questions and their answer runs lie outside the Lead's recent window.
    while offset < 6 {
        let page = call(
            &*hub.link,
            &lease,
            &first,
            SESSIONS,
            json!({"offset":offset,"limit":6-offset}),
        )
        .await
        .unwrap();
        offset = page["next_offset"].as_u64().unwrap();
    }
    complete(&hub, &lease, &first).await;
    let project = hub.general().await;
    assert!(
        hub.db.preferences_need_review(project.id).await.unwrap(),
        "older choices were unread"
    );
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let mut ids = std::collections::BTreeSet::new();
    let mut offset = 0;
    loop {
        let page = call(
            &*hub.link,
            &lease,
            &dream,
            DECISIONS,
            json!({"limit":8,"offset":offset}),
        )
        .await
        .unwrap();
        assert_eq!(page["total"], 23);
        for row in page["recent_user_choices"].as_array().unwrap() {
            assert!(
                ids.insert(row["decision_id"].as_i64().unwrap()),
                "duplicate page member"
            );
        }
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        offset = next;
    }
    assert_eq!(ids.len(), 23);
    let page = call(&*hub.link, &lease, &dream, SESSIONS, json!({"limit":1}))
        .await
        .unwrap();
    let total = page["total"].as_u64().unwrap();
    let agent = hub.db.bindings(project.id).await.unwrap()[0].agent_id;
    hub.db
        .create_task(
            None,
            &threadlane_core::model::NewTask {
                project_id: project.id,
                agent_id: agent,
                title: "Arrived during review".into(),
                goal: "Later".into(),
                repositories: vec![],
                parent_task_id: None,
                depends_on: vec![],
                origin: threadlane_core::model::TaskOrigin::User,
                status: threadlane_core::model::TaskStatus::Proposed,
                priority: 0,
                created_by_call: None,
                created_at: hub.clock.now(),
                playbooks: Vec::new(),
            },
        )
        .await
        .unwrap();
    let past = call(
        &*hub.link,
        &lease,
        &dream,
        SESSIONS,
        json!({"offset":total}),
    )
    .await
    .unwrap();
    assert_eq!(past["total"], total);
    assert!(past["tasks"].as_array().unwrap().is_empty());
    complete(&hub, &lease, &dream).await;
    assert!(!hub.db.preferences_need_review(project.id).await.unwrap());
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, next) = begin(&*hub.link).await;
    let later = call(
        &*hub.link,
        &lease,
        &next,
        SESSIONS,
        json!({"offset":total-1}),
    )
    .await
    .unwrap();
    assert_eq!(later["tasks"][0]["title"], "Arrived during review");
}

/// Asks the owner in `run` and answers, then lets the resumed run finish.
async fn ask_and_answer(hub: &TestHub, lease: &Lease, run: &RunAssignment, question: &str) {
    assert_eq!(
        call(
            &*hub.link,
            lease,
            run,
            "ask_user",
            json!({"question":question,"options":["Table","Prose"]})
        )
        .await,
        Err(ToolError::WaitingOnYou)
    );
    complete(hub, lease, run).await;
    let pending = hub.db.pending_decisions(lease.lane_id).await.unwrap();
    hub.api
        .decisions(DecisionRequest::Answer {
            decision: pending[0].id,
            option: "Table".into(),
            note: Some(format!("Note for {question}")),
        })
        .await
        .unwrap();
    let (lease, resumed) = begin(&*hub.link).await;
    assert_eq!(resumed.lane_kind, run.lane_kind, "the asking lane resumes");
    complete(hub, &lease, &resumed).await;
}

/// Pages through choices in a new dream, at most `max_pages` pages.
async fn dream_choices(hub: &TestHub, limit: u64, max_pages: usize) -> Vec<Json> {
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    assert!(dream.dreaming);
    let mut pages = Vec::new();
    let mut offset = 0;
    loop {
        let page = call(
            &*hub.link,
            &lease,
            &dream,
            DECISIONS,
            json!({"limit":limit,"offset":offset}),
        )
        .await
        .unwrap();
        let next = page["next_offset"].as_u64();
        assert_eq!(page["more_remain"], next.is_some(), "{page}");
        pages.push(page);
        match next {
            Some(next) if pages.len() < max_pages => offset = next,
            _ => break,
        }
    }
    complete(hub, &lease, &dream).await;
    pages
}

#[tokio::test]
async fn reviewed_choices_page_newest_first_across_lead_and_task_runs() {
    let hub = TestHub::start().await;
    hub.hello().await;
    set(&hub, None).await;
    let project = hub.general().await;
    let agent = hub
        .db
        .create_agent(&threadlane_core::model::NewAgent::named(
            "Engineer", "engineer",
        ))
        .await
        .unwrap();
    hub.db
        .bind(&threadlane_core::model::NewBinding {
            project_id: project.id,
            agent_id: agent.id,
            mode: Some(threadlane_core::model::Mode::OnRequest),
            is_lead: false,
            max_parallel: 1,
        })
        .await
        .unwrap();
    for index in 0..3 {
        hub.send(&format!("old-{index}"), "Prepare a weekly report.")
            .await;
        let (lease, lead) = begin(&*hub.link).await;
        ask_and_answer(&hub, &lease, &lead, &format!("Old {index}")).await;
    }
    set(&hub, Some(interval())).await;
    let first = dream_choices(&hub, 50, 1).await;
    assert_eq!(first[0]["total"], 3);

    // Newer answers, from a task run and then a lead run.
    hub.send("task", "Build the report page.").await;
    let (lease, lead) = begin(&*hub.link).await;
    call(
        &*hub.link,
        &lease,
        &lead,
        "create_task",
        json!({"title":"page","goal":"page is done","agent":"Engineer"}),
    )
    .await
    .unwrap();
    complete(&hub, &lease, &lead).await;
    let (lease, task_run) = begin(&*hub.link).await;
    assert!(task_run.task_id.is_some(), "a task run asks");
    ask_and_answer(&hub, &lease, &task_run, "Newer from task").await;
    hub.send("lead", "One more report.").await;
    let (lease, lead) = begin(&*hub.link).await;
    ask_and_answer(&hub, &lease, &lead, "Newest from lead").await;
    // This dream reads only the unread page, so the old choices keep their earlier review.
    let second = dream_choices(&hub, 2, 1).await;
    assert_eq!(second[0]["more_remain"], true);

    // Every choice is reviewed now; the newest still come first.
    let pages = dream_choices(&hub, 2, usize::MAX).await;
    let questions: Vec<_> = pages
        .iter()
        .flat_map(|page| page["recent_user_choices"].as_array().unwrap().clone())
        .map(|choice| {
            assert_eq!(choice["chosen"], "Table");
            choice["question"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(
        questions,
        [
            "Newest from lead",
            "Newer from task",
            "Old 2",
            "Old 1",
            "Old 0"
        ]
    );
    assert_eq!(pages[0]["total"], 5);
    assert_eq!(pages[0]["more_remain"], true);
    assert_eq!(pages[2]["more_remain"], false);
    assert_eq!(
        pages[0]["recent_user_choices"][0]["note"],
        "Note for Newest from lead"
    );
}

#[tokio::test]
async fn sessions_keep_cut_tasks_structured_and_can_list_only_recent_ones() {
    use threadlane_core::model::{NewTask, TaskOrigin, TaskStatus};

    let hub = TestHub::start().await;
    hub.hello().await;
    set(&hub, None).await;
    let project = hub.general().await;
    let agent = hub.db.bindings(project.id).await.unwrap()[0].agent_id;
    let new_task = |title: &str, status| NewTask {
        project_id: project.id,
        agent_id: agent,
        title: title.into(),
        goal: "Reviewed by the dream".into(),
        repositories: vec![],
        parent_task_id: None,
        depends_on: vec![],
        origin: TaskOrigin::User,
        status,
        priority: 0,
        created_by_call: None,
        created_at: hub.clock.now(),
        playbooks: Vec::new(),
    };
    let big = hub
        .db
        .create_task(None, &new_task("Big", TaskStatus::Queued))
        .await
        .unwrap();
    let (lease, run) = begin(&*hub.link).await;
    assert_eq!(run.task_id, Some(big.id));
    call(
        &*hub.link,
        &lease,
        &run,
        MEMORY_CREATE,
        json!({"path": format!("project/tasks/{}.md", big.id), "content": "# Big\nDone."}),
    )
    .await
    .unwrap();
    let summary = format!("Shipped the big change.\n{}", "x".repeat(30_000));
    call(
        &*hub.link,
        &lease,
        &run,
        "finish_task",
        json!({"summary": summary}),
    )
    .await
    .unwrap();
    complete(&hub, &lease, &run).await;
    // The lead hears the result first.
    let (lease, lead) = begin(&*hub.link).await;
    assert_eq!(lead.task_id, None);
    complete(&hub, &lease, &lead).await;

    hub.clock.advance(Duration::from_secs(60));
    let cutoff = threadlane_hub::format_time(hub.clock.now());
    // Off a whole second, so a truncated updated_at would repeat below.
    hub.clock.advance(Duration::from_millis(60_500));
    hub.db
        .create_task(None, &new_task("Recent", TaskStatus::Proposed))
        .await
        .unwrap();

    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let all = call(&*hub.link, &lease, &dream, SESSIONS, json!({}))
        .await
        .unwrap();
    assert!(all.get("previous_reports").is_some(), "{all}");
    let tasks = all["tasks"].as_array().unwrap();
    let cut = tasks.iter().find(|t| t["task_id"] == "T-0001").unwrap();
    assert_eq!(cut["status"], "done");
    assert_eq!(cut["waiting_reason"], Json::Null);
    let line = cut["summary"].as_str().unwrap();
    assert!(
        line.starts_with("Shipped the big change. xxx") && line.len() < 300,
        "{line}"
    );
    assert!(cut["updated_at"].as_str().unwrap() < cutoff.as_str());
    assert!(cut.get("result").is_none(), "{cut}");

    let full = call(
        &*hub.link,
        &lease,
        &dream,
        SESSIONS,
        json!({"task_id": "T-0001"}),
    )
    .await
    .unwrap();
    assert_eq!(full["result"]["summary"], summary);
    assert!(
        full["summary"]
            .as_str()
            .unwrap()
            .starts_with(line.trim_end_matches('…'))
    );
    assert!(full.get("goal").is_some() && cut.get("goal").is_none());

    let recent = call(
        &*hub.link,
        &lease,
        &dream,
        SESSIONS,
        json!({"updated_after": cutoff, "offset": 0}),
    )
    .await
    .unwrap();
    let titles: Vec<&Json> = recent["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["title"])
        .collect();
    assert_eq!(titles, [&json!("Recent")]);
    let stamp = recent["tasks"][0]["updated_at"].as_str().unwrap();
    assert!(stamp.ends_with(".500Z"), "{stamp}");
    let after = call(
        &*hub.link,
        &lease,
        &dream,
        SESSIONS,
        json!({"updated_after": stamp, "offset": 0}),
    )
    .await
    .unwrap();
    assert_eq!(after["tasks"], json!([]), "{after}");

    let later = call(&*hub.link, &lease, &dream, SESSIONS, json!({"offset": 1}))
        .await
        .unwrap();
    assert!(later.get("previous_reports").is_none(), "{later}");
    for bad in [
        json!({"updated_after": "yesterday"}),
        json!({"task_id": "T-0099"}),
    ] {
        assert!(matches!(
            call(&*hub.link, &lease, &dream, SESSIONS, bad).await,
            Err(ToolError::InvalidArguments(_))
        ));
    }
    complete(&hub, &lease, &dream).await;
}

#[tokio::test]
async fn sessions_page_every_task_past_the_old_cap_of_two_hundred() {
    use threadlane_core::model::{NewTask, TaskOrigin, TaskStatus};

    let hub = TestHub::start().await;
    hub.hello().await;
    set(&hub, None).await;
    let project = hub.general().await;
    let agent = hub.db.bindings(project.id).await.unwrap()[0].agent_id;
    for index in 0..205 {
        hub.db
            .create_task(
                None,
                &NewTask {
                    project_id: project.id,
                    agent_id: agent,
                    title: format!("Task {index}"),
                    goal: "Reviewed by the dream".into(),
                    repositories: vec![],
                    parent_task_id: None,
                    depends_on: vec![],
                    origin: TaskOrigin::User,
                    status: TaskStatus::Proposed,
                    priority: 0,
                    created_by_call: None,
                    created_at: hub.clock.now(),
                    playbooks: Vec::new(),
                },
            )
            .await
            .unwrap();
    }
    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let mut ids = std::collections::BTreeSet::new();
    let mut offset = 0;
    loop {
        let page = call(
            &*hub.link,
            &lease,
            &dream,
            SESSIONS,
            json!({"offset": offset, "limit": 200}),
        )
        .await
        .unwrap();
        let total_runs = page["total_runs"].as_u64().unwrap();
        assert_eq!(page["total"].as_u64().unwrap(), total_runs + 205);
        for task in page["tasks"].as_array().unwrap() {
            assert!(ids.insert(task["task_id"].as_str().unwrap().to_owned()));
        }
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        offset = next;
    }
    assert_eq!(ids.len(), 205);
}

#[tokio::test]
async fn sessions_give_the_reason_each_failed_or_abandoned_run_ended() {
    use threadlane_core::link::RunFailure;
    use threadlane_core::model::{NewTask, TaskOrigin, TaskStatus};

    let hub = TestHub::start().await;
    hub.hello().await;
    set(&hub, None).await;
    let project = hub.general().await;
    let agent = hub.db.bindings(project.id).await.unwrap()[0].agent_id;
    let task = hub
        .db
        .create_task(
            None,
            &NewTask {
                project_id: project.id,
                agent_id: agent,
                title: "Flaky".into(),
                goal: "Fails then is abandoned".into(),
                repositories: vec![],
                parent_task_id: None,
                depends_on: vec![],
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
    let name = hub
        .db
        .agents()
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.id == agent)
        .unwrap()
        .name;
    // A backend diagnostic carrying a credential must never reach Dreaming.
    const SECRET: &str = "sk-live-SYNTHETIC0123456789";
    let (lease, failed) = begin(&*hub.link).await;
    assert_eq!(failed.task_id, Some(task.id));
    let message = format!("request failed\nAuthorization: Bearer {SECRET}");
    hub.link
        .fail(&lease, failed.run_id, &RunFailure::Error { message })
        .await
        .unwrap();
    hub.link.release(&lease).await.unwrap();
    hub.clock.advance(Duration::from_secs(11));
    // Twenty more failures that count no attempt, so the oldest is the 22nd.
    // The newest of them lost its backend executable before it started.
    let mut missing = None;
    for i in 0..20 {
        let (lease, run) = begin(&*hub.link).await;
        assert_eq!(run.task_id, Some(task.id));
        let failure = if i == 19 {
            missing = Some(run.run_id);
            RunFailure::Error {
                message: "the backend could not start the run: backend not installed".into(),
            }
        } else {
            RunFailure::SessionMissing
        };
        hub.link.fail(&lease, run.run_id, &failure).await.unwrap();
        hub.link.release(&lease).await.unwrap();
    }
    // That error counts an attempt, so wait out its retry delay.
    hub.clock.advance(Duration::from_secs(30));
    let missing = missing.unwrap();
    let (_, abandoned) = begin(&*hub.link).await;
    assert_eq!(abandoned.task_id, Some(task.id));
    hub.clock.advance(PAST_LEASE);
    hub.core.sweep_once().await.unwrap();
    while let Some(lease) = hub.link.request_work().await.unwrap() {
        let Begin::Run(run) = hub.link.begin_run(&lease).await.unwrap() else {
            hub.link.release(&lease).await.unwrap();
            break;
        };
        complete(&hub, &lease, &run).await;
    }

    set(&hub, Some(interval())).await;
    hub.clock.advance(HOUR);
    hub.core.schedule_once().await.unwrap();
    let (lease, dream) = begin(&*hub.link).await;
    let page = call(&*hub.link, &lease, &dream, SESSIONS, json!({}))
        .await
        .unwrap();
    assert!(!page.to_string().contains(SECRET), "{page}");
    let run = |id: threadlane_core::ids::RunId| {
        page["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["run_id"] == id.get())
            .unwrap_or_else(|| panic!("no run {id} in {page}"))
            .clone()
    };
    let listed = run(failed.run_id);
    assert_eq!(listed["status"], "failed");
    assert_eq!(listed["end_code"], "error", "{listed}");
    assert_eq!(
        listed["end_reason"],
        "the run failed; the backend's diagnostic is withheld"
    );
    assert_eq!(listed["task_id"], task.id.to_string());
    assert_eq!(listed["agent"], name.as_str());
    let gone = run(abandoned.run_id);
    assert_eq!(gone["status"], "abandoned", "{gone}");
    assert_eq!(gone["end_code"], "lease_ended");
    assert_eq!(gone["agent"], name.as_str());
    assert_eq!(run(missing)["end_code"], "backend_not_installed");
    let committed = page["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["status"] == "committed")
        .expect("a committed run");
    assert!(committed.get("end_reason").is_none(), "{committed}");
    assert!(committed.get("agent").is_none(), "{committed}");

    let detail = |offset: usize| {
        let (link, lease, dream) = (&hub.link, &lease, &dream);
        async move {
            call(
                &**link,
                lease,
                dream,
                SESSIONS,
                json!({"task_id": task.id.to_string(), "offset": offset, "limit": 20}),
            )
            .await
            .unwrap()
        }
    };
    let first = detail(0).await;
    assert!(!first.to_string().contains(SECRET), "{first}");
    let runs = first["failed_runs"].as_array().unwrap();
    assert_eq!(runs.len(), 20, "{first}");
    assert_eq!(first["failed_runs_total"], 22);
    assert_eq!(first["failed_runs_next_offset"], 20);
    assert_eq!(runs[0]["run_id"], abandoned.run_id.get());
    assert_eq!(runs[0]["end_code"], "lease_ended");
    assert_eq!(runs[1]["run_id"], missing.get());
    assert_eq!(runs[1]["end_code"], "backend_not_installed");
    assert_eq!(runs[2]["end_code"], "session_missing");
    let rest = detail(20).await;
    assert!(!rest.to_string().contains(SECRET), "{rest}");
    let runs = rest["failed_runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2, "{rest}");
    assert!(rest.get("failed_runs_next_offset").is_none(), "{rest}");
    assert_eq!(runs[1]["run_id"], failed.run_id.get());
    assert_eq!(runs[1]["end_code"], "error");
    assert_eq!(runs[1]["agent"], name.as_str());
    complete(&hub, &lease, &dream).await;
}
