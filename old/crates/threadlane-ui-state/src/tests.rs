use super::*;
use crate::projection::{
    compute_full_session_projection, compute_latest_run_completion, compute_session_messages,
    compute_session_messages_for_hydration,
};
use crate::test_support::{
    activate_test_session, generated_reported_session_path, reported_session_shape_state,
};
use std::process::Command;
use std::sync::{Arc, Mutex};
use threadlane_coding_agent::controller::SessionRuntimeStatus;
use threadlane_protocol::AgentMessage;
use threadlane_coding_agent::harness::CodingSessionHarness;
use threadlane_runtime::harness::{
    OperationIntent, OperationOutcome, ProviderOutcome, Record, SessionStore, TraceString,
};

#[test]
fn close_work_refresh_is_required_only_for_undisclosed_identities() {
    assert!(!close_work_needs_refresh(
        &["a".into()],
        &["a".into(), "b".into()]
    ));
    assert!(!close_work_needs_refresh(&[], &["a".into()]));
    assert!(close_work_needs_refresh(
        &["a".into(), "new".into()],
        &["a".into()]
    ));
}
#[test]
fn active_close_work_includes_only_live_automation_runs() {
    use threadlane_automation::RunStatus;
    assert_eq!(active_automation_status(RunStatus::Starting), Some("Starting"));
    assert_eq!(active_automation_status(RunStatus::Running), Some("Running"));
    assert_eq!(active_automation_status(RunStatus::WaitingPermission), Some("Needs permission"));
    assert_eq!(active_automation_status(RunStatus::WaitingAnswer), Some("Needs an answer"));
    assert_eq!(active_automation_status(RunStatus::Queued), None);
    assert_eq!(active_automation_status(RunStatus::Succeeded), None);

    let root = tempfile::tempdir().unwrap();
    let project = root.path().to_path_buf();
    let session_file = project.join(".threadlane/sessions/active.jsonl");
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    let session = test_session("active", &session_file);
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: vec![session.clone()],
        is_expanded: true,
    });
    let runtime = state.ensure_session_runtime(
        session.runtime_work_dir.clone(),
        session_file.clone(),
    );
    runtime.begin_generation().unwrap();

    let definition = threadlane_automation::Definition {
        id: "test".into(),
        revision: 1,
        name: "Test".into(),
        prompt: "Test".into(),
        project: project.clone(),
        model: "model".into(),
        effort: "medium".into(),
        worktree: false,
        schedule: threadlane_automation::Schedule::Manual,
        enabled: false,
        notify_all: false,
        anchor: 0,
        next_at: None,
        failures: 0,
        paused_reason: None,
    };
    let make_run = |id: &str, status: RunStatus, session_file: Option<PathBuf>| {
        threadlane_automation::Run {
            id: id.into(),
            definition: definition.clone(),
            scheduled_for: None,
            created_at: 0,
            finished_at: None,
            status,
            session_id: id.into(),
            session_file,
            error: None,
            reviewed: false,
        }
    };
    let mut projection = crate::automation::Projection::default();
    projection.snapshot.runs.extend([
        make_run("duplicate", RunStatus::Running, Some(session_file.clone())),
        make_run("standalone", RunStatus::WaitingAnswer, None),
        make_run("queued", RunStatus::Queued, None),
    ]);
    state.apply_automation_projection(projection);

    let work = state.active_close_work();
    let identity = session_file.display().to_string();
    assert_eq!(work.len(), 2);
    assert_eq!(
        work.iter().filter(|item| item.identity == identity).count(),
        1
    );
    assert!(work.iter().any(|item| {
        item.identity == "automation:standalone" && item.status == "Needs an answer"
    }));
    assert!(!work.iter().any(|item| item.identity == "automation:queued"));
}
#[test]
fn automation_projection_refreshes_each_changed_project_once() {
    let mut state = AppState::load_from_registry(vec![]);
    let (tx, rx) = mpsc::channel();
    state.session_refresh_tx = tx;
    let definition = threadlane_automation::Definition {
        id: "test".into(), revision: 1, name: "Test".into(), prompt: "Test".into(),
        project: PathBuf::from("/project-a"), model: "model".into(), effort: "medium".into(),
        worktree: false, schedule: threadlane_automation::Schedule::Manual,
        enabled: false, notify_all: false, anchor: 0, next_at: None, failures: 0,
        paused_reason: None,
    };
    let mut projection = crate::automation::Projection::default();
    for n in 0..4 {
        let mut definition = definition.clone();
        if n == 3 { definition.project = PathBuf::from("/project-b"); }
        projection.snapshot.runs.push(threadlane_automation::Run {
            id: n.to_string(), definition, scheduled_for: None, created_at: n,
            finished_at: Some(n), status: threadlane_automation::RunStatus::Succeeded,
            session_id: format!("automation_{n}"), session_file: None, error: None, reviewed: false,
        });
    }
    state.apply_automation_projection(projection.clone());
    let projects: Vec<_> = rx.try_iter().map(|(_, path)| path).collect();
    assert_eq!(projects.len(), 2);
    assert_eq!(projects.into_iter().collect::<HashSet<_>>(), HashSet::from([PathBuf::from("/project-a"), PathBuf::from("/project-b")]));
    state.apply_automation_projection(projection.clone());
    assert!(rx.try_recv().is_err());
    projection.snapshot.runs[0].session_file = Some(PathBuf::from("/chat.jsonl"));
    projection.snapshot.runs[1].status = threadlane_automation::RunStatus::Failed;
    state.apply_automation_projection(projection);
    assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![(0, PathBuf::from("/project-a"))]);
}

/// Remote events refresh the automation view and requests without triggering local discovery.
#[test]
fn remote_automation_projection_refreshes_client_and_view() {
    use threadlane_protocol::automation::{AutomationProjection, Definition, Schedule};

    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    let (tx, rx) = mpsc::channel();
    state.session_refresh_tx = tx;
    let mut projection = AutomationProjection {
        permissions: HashMap::from([(
            "automation-session".into(),
            permission_request("permission"),
        )]),
        questions: HashMap::from([("automation-session".into(), question_request("question"))]),
        question_queues: Some(HashMap::from([(
            "automation-session".into(),
            vec![question_request("question")],
        )])),
        active_session_id: Some("automation-session".into()),
        error: Some("Remote store warning".into()),
        ..Default::default()
    };
    projection.snapshot.revision = 17;
    projection.snapshot.definitions.push(Definition {
        id: "remote-automation".into(),
        revision: 1,
        name: "Remote automation".into(),
        prompt: "Check remote project".into(),
        project: PathBuf::from("/remote/project"),
        model: "model".into(),
        effort: "medium".into(),
        worktree: false,
        schedule: Schedule::Manual,
        enabled: false,
        notify_all: false,
        anchor: 0,
        next_at: None,
        failures: 0,
        paused_reason: None,
    });
    projection.snapshot.runs.push(threadlane_automation::Run {
        id: "remote-run".into(),
        definition: projection.snapshot.definitions[0].clone(),
        scheduled_for: None,
        created_at: 0,
        finished_at: None,
        status: threadlane_automation::RunStatus::WaitingAnswer,
        session_id: "automation-session".into(),
        session_file: Some(PathBuf::from(
            "/remote/project/.threadlane/sessions/automation-session.jsonl",
        )),
        error: None,
        reviewed: false,
    });

    assert!(
        state.drain_chat_stream(vec![SessionEvent::AutomationChanged {
            projection: projection.clone(),
        }])
    );
    assert_eq!(state.client.automation, projection);
    assert_eq!(state.automations.snapshot, projection.snapshot);
    assert_eq!(state.automations.permissions, projection.permissions);
    assert_eq!(state.automations.questions, projection.questions);
    assert_eq!(
        Some(&state.automations.question_queues),
        projection.question_queues.as_ref()
    );
    assert_eq!(state.automations.error, projection.error);
    assert_eq!(state.pending_permissions, projection.permissions);
    assert_eq!(state.pending_questions, projection.questions);
    assert!(state.automations.active_runtime.is_none());
    assert!(state.automation_service.is_none());
    assert!(
        rx.try_recv().is_err(),
        "remote paths must not trigger local discovery"
    );
}

/// Cross-client answers advance complete queues and preserve unrelated permissions.
#[test]
fn remote_automation_projection_resolves_requests_and_promotes_next_question() {
    use threadlane_protocol::automation::AutomationProjection;

    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.active_session_id = Some("automation-session".into());
    let projection = AutomationProjection {
        permissions: HashMap::from([(
            "automation-session".into(),
            permission_request("permission"),
        )]),
        questions: HashMap::from([(
            "automation-session".into(),
            question_request("queued-question"),
        )]),
        question_queues: Some(HashMap::from([(
            "automation-session".into(),
            vec![
                question_request("question"),
                question_request("queued-question"),
            ],
        )])),
        ..Default::default()
    };
    assert!(state.drain_chat_stream(vec![SessionEvent::AutomationChanged { projection }]));
    state
        .pending_permissions
        .insert("unrelated-session".into(), permission_request("unrelated"));

    // The first question is answered elsewhere while the latest one stays
    // unchanged. The complete queue is the only indication of that answer.
    let mut partial = state.client.automation.clone();
    partial
        .question_queues
        .as_mut()
        .unwrap()
        .get_mut("automation-session")
        .unwrap()
        .remove(0);
    assert!(
        state.drain_chat_stream(vec![SessionEvent::AutomationChanged {
            projection: partial,
        }])
    );
    assert_eq!(
        state.pending_questions["automation-session"].id,
        "queued-question"
    );
    assert!(!state.queued_questions.contains_key("automation-session"));
    assert!(state.drain_chat_stream(vec![SessionEvent::Agent {
        session_id: "automation-session".into(),
        event: AgentEvent::QuestionRequested {
            request: question_request("next-question"),
        },
    }]));
    assert_eq!(
        state.queued_questions["automation-session"],
        vec![question_request("next-question")]
    );

    // Another client resolves the permission and visible question. The
    // complete projection retains only the later question.
    let resolved = AutomationProjection {
        questions: HashMap::from([(
            "automation-session".into(),
            question_request("next-question"),
        )]),
        question_queues: Some(HashMap::from([(
            "automation-session".into(),
            vec![question_request("next-question")],
        )])),
        ..Default::default()
    };
    assert!(
        state.drain_chat_stream(vec![SessionEvent::AutomationChanged {
            projection: resolved.clone(),
        }])
    );
    assert!(!state.pending_permissions.contains_key("automation-session"));
    assert_eq!(
        state.pending_permissions["unrelated-session"].id,
        "unrelated"
    );
    assert_eq!(
        state.pending_questions["automation-session"].id,
        "next-question"
    );
    assert!(!state.queued_questions.contains_key("automation-session"));
    assert!(state.automations.permissions.is_empty());
    assert_eq!(state.automations.questions, resolved.questions);
    assert_eq!(state.client.automation, resolved);

    assert!(
        state.drain_chat_stream(vec![SessionEvent::AutomationChanged {
            projection: AutomationProjection {
                question_queues: Some(HashMap::new()),
                ..Default::default()
            },
        }])
    );
    assert!(!state.pending_questions.contains_key("automation-session"));
    assert!(!state.queued_questions.contains_key("automation-session"));
    assert!(state.automations.questions.is_empty());
}

/// The local watch uses the same FIFO reconciliation as remote projections.
#[test]
fn local_automation_projection_preserves_question_queue_order() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.active_session_id = Some("automation-session".into());
    for id in ["first-question", "second-question"] {
        assert!(state.drain_chat_stream(vec![SessionEvent::Agent {
            session_id: "automation-session".into(),
            event: AgentEvent::QuestionRequested {
                request: question_request(id),
            },
        }]));
    }
    let projection = crate::automation::Projection {
        questions: HashMap::from([(
            "automation-session".into(),
            question_request("second-question"),
        )]),
        question_queues: HashMap::from([(
            "automation-session".into(),
            vec![
                question_request("first-question"),
                question_request("second-question"),
            ],
        )]),
        ..Default::default()
    };
    state.apply_automation_projection(projection.clone());
    state.apply_automation_projection(projection);
    assert_eq!(
        state.pending_questions["automation-session"].id,
        "first-question"
    );
    assert_eq!(
        state.queued_questions["automation-session"],
        vec![question_request("second-question")]
    );

    state.apply_automation_projection(crate::automation::Projection::default());
    assert!(!state.pending_questions.contains_key("automation-session"));
    assert!(!state.queued_questions.contains_key("automation-session"));
}

/// First snapshots handle both missed resolutions and partial reconnect replay.
#[test]
fn remote_automation_first_snapshot_reconciles_coalesced_agent_questions() {
    use threadlane_protocol::automation::AutomationProjection;

    for (agent_ids, projected_ids) in [
        (vec!["first", "second"], vec!["second"]),
        (vec!["second"], vec!["first", "second"]),
    ] {
        let mut state = AppState::load_from_registry(Vec::new());
        state.daemon_remote = true;
        state.active_session_id = Some("automation-session".into());
        for id in agent_ids {
            assert!(state.drain_chat_stream(vec![SessionEvent::Agent {
                session_id: "automation-session".into(),
                event: AgentEvent::QuestionRequested {
                    request: question_request(id)
                },
            }]));
        }
        let requests: Vec<_> = projected_ids
            .iter()
            .map(|id| question_request(id))
            .collect();
        let projection = AutomationProjection {
            questions: HashMap::from([(
                "automation-session".into(),
                requests.last().unwrap().clone(),
            )]),
            question_queues: Some(HashMap::from([(
                "automation-session".into(),
                requests.clone(),
            )])),
            ..Default::default()
        };
        assert!(state.drain_chat_stream(vec![SessionEvent::AutomationChanged { projection }]));
        assert_eq!(state.pending_questions["automation-session"], requests[0]);
        let queued = state
            .queued_questions
            .get("automation-session")
            .cloned()
            .unwrap_or_default();
        assert_eq!(queued, requests[1..]);
    }
}

/// Local wire copies cannot replace the service watch projection.
#[test]
fn local_automation_watch_remains_authoritative_over_wire_events() {
    let mut state = AppState::load_from_registry(Vec::new());
    let mut projection = crate::automation::Projection {
        permissions: HashMap::from([(
            "automation-session".into(),
            permission_request("permission"),
        )]),
        questions: HashMap::from([("automation-session".into(), question_request("question"))]),
        question_queues: HashMap::from([(
            "automation-session".into(),
            vec![question_request("question")],
        )]),
        ..Default::default()
    };
    projection.snapshot.revision = 5;
    state.apply_automation_projection(projection.clone());
    let client_projection = state.client.automation.clone();

    assert!(
        !state.drain_chat_stream(vec![SessionEvent::AutomationChanged {
            projection: Default::default(),
        }])
    );
    assert_eq!(state.client.automation, client_projection);
    assert_eq!(state.automations.snapshot, projection.snapshot);
    assert_eq!(state.pending_permissions, projection.permissions);
    assert_eq!(state.pending_questions, projection.questions);
}

/// Remote desktops cannot overwrite daemon state with a local service watch.
#[test]
fn remote_automation_does_not_subscribe_to_or_apply_local_projections() {
    use threadlane_protocol::automation::AutomationProjection;

    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    let projection = AutomationProjection {
        questions: HashMap::from([(
            "automation-session".into(),
            question_request("remote-question"),
        )]),
        error: Some("Remote store warning".into()),
        ..Default::default()
    };
    assert!(
        state.drain_chat_stream(vec![SessionEvent::AutomationChanged {
            projection: projection.clone(),
        }])
    );

    assert!(state.start_automations().is_none());
    assert!(state.automation_service.is_none());
    state.apply_automation_projection(crate::automation::Projection::default());
    assert_eq!(state.client.automation, projection);
    assert_eq!(state.automations.questions, projection.questions);
    assert_eq!(state.automations.error, projection.error);
    assert_eq!(state.pending_questions, projection.questions);
}

#[test]
fn fusion_project_model_overrides_old_fast_role() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = threadlane_project::subagent_settings::SubagentSettings::default();
    settings.fast_model = Some("gpt-5.6-luna".into());
    settings.orchestrator_mode = threadlane_protocol::OrchestratorMode::Fusion;
    threadlane_project::subagent_settings::save(dir.path(), &settings).unwrap();
    let mut roles = threadlane_runtime::ModelRoles::default();
    roles.fast = Some("gpt-5.6-terra".into());
    let options = crate::projection::coding_agent_options(
        dir.path().to_path_buf(),
        dir.path().join("session.jsonl"),
        "gpt-6-astra".into(),
        roles,
        threadlane_protocol::browser::BrowserBridge::unavailable(),
    );
    assert_eq!(
        options.agent_config.unwrap().model_roles.fast.as_deref(),
        Some("gpt-5.6-luna")
    );
}

#[test]
fn run_timing_uses_durable_identity_and_survives_selection_and_stale_hydration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timing.jsonl");
    let mut store = JsonlStore::open(&path).unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            wall_time_ms: Some(10_000),
            source_leaf_id: None,
            intent: OperationIntent::Run,
        })
        .unwrap();
    let running = compute_full_session_projection(&path)
        .unwrap()
        .run_timing
        .unwrap();
    assert_eq!(running.elapsed_seconds(17_999, true), Some(7));
    assert_eq!(running.elapsed_seconds(9_000, true), None);
    assert_eq!(running.elapsed_seconds(17_000, false), None);
    store
        .append_record(Record::AbortObserved {
            id: "abort".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 2,
            wall_time_ms: Some(22_000),
            run_id: "run".into(),
            attempt: None,
            observation: threadlane_runtime::harness::AbortObservation::SignalSent,
            initiator: threadlane_runtime::harness::AbortInitiator::User,
            target: threadlane_runtime::harness::AbortTarget::ActiveRun,
            acknowledged: true,
            detail: None,
        })
        .unwrap();
    let stopped = compute_full_session_projection(&path)
        .unwrap()
        .run_timing
        .unwrap();
    assert_eq!(stopped.elapsed_seconds(99_000, false), Some(12));
    store
        .append_record(Record::OperationFinished {
            id: "finished".into(),
            seq: 3,
            lane: "main".into(),
            timestamp: 3,
            wall_time_ms: Some(99_000),
            run_id: "run".into(),
            outcome: OperationOutcome::Aborted,
            error: None,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "compaction".into(),
            seq: 4,
            lane: "main".into(),
            timestamp: 4,
            wall_time_ms: Some(100_000),
            source_leaf_id: None,
            intent: OperationIntent::Compaction,
        })
        .unwrap();
    let finished = compute_full_session_projection(&path)
        .unwrap()
        .run_timing
        .unwrap();
    assert_eq!(finished.elapsed_seconds(99_000, false), Some(12));
    assert_eq!(finished.elapsed_seconds(99_000, true), None);
    let mut legacy = finished.clone();
    legacy.started_at_ms = None;
    assert_eq!(legacy.elapsed_seconds(99_000, false), None);
    legacy = finished.clone();
    legacy.finished_at_ms = None;
    assert_eq!(legacy.elapsed_seconds(99_000, false), None);

    let mut state = AppState::load_from_registry(Vec::new());
    state.active_work_dir = Some(dir.path().to_path_buf());
    state.active_session_id = Some("one".into());
    let key = state.active_session_projection_key().unwrap();
    state.apply_run_timing(&key, Some(finished.clone()));
    state.apply_run_timing(&key, Some(running));
    assert_eq!(state.active_run_elapsed_seconds(), Some(12));
    state.active_session_id = Some("two".into());
    assert_eq!(state.active_run_elapsed_seconds(), None);
    state.active_session_id = Some("one".into());
    assert_eq!(state.active_run_elapsed_seconds(), Some(12));
    state.run_timings.get_mut(&key).unwrap().suppressed = true;
    state.apply_run_timing(&key, Some(finished.clone()));
    assert_eq!(state.active_run_elapsed_seconds(), None);
    let mut next = finished;
    next.start_seq = 4;
    next.source_seq = 5;
    state.apply_run_timing(&key, Some(next));
    assert_eq!(state.active_run_elapsed_seconds(), Some(12));
}

#[test]
fn filesystem_root_is_not_an_attachable_project() {
    assert!(!super::is_attachable_project_root(Path::new("/")));
    assert!(super::is_attachable_project_root(Path::new("/project")));
}

#[test]
fn model_selection_resets_unsupported_reasoning_and_rejects_hidden_efforts() {
    use threadlane_protocol::ReasoningEffort;
    let mut state = AppState::load_from_registry(Vec::new());
    state.available_models = vec![threadlane_daemon::catalog::ModelOption {
        id: "gpt-4o".into(),
        label: "GPT-4o".into(),
        provider: threadlane_daemon::catalog::ModelProvider::OpenAi,
    }];
    state.selected_model = "previous-model".into();
    state.reasoning_effort = ReasoningEffort::High;
    state.set_selected_model("gpt-4o".into());
    assert_eq!(state.reasoning_effort, ReasoningEffort::Off);
    state.set_reasoning_effort(ReasoningEffort::High);
    assert_eq!(state.reasoning_effort, ReasoningEffort::Off);
}

#[test]
fn active_git_work_dir_uses_the_active_session_checkout_when_available() {
    let local_project = PathBuf::from("/projects/local");
    let worktree = PathBuf::from("/projects/local/.threadlane/worktrees/session");
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects = vec![ProjectInfo {
        name: "Local".into(),
        work_dir: local_project.clone(),
        sessions: Vec::new(),
        is_expanded: true,
    }];
    state.active_work_dir = Some(local_project.clone());

    assert_eq!(state.active_git_work_dir(), Some(local_project.clone()));

    state.projects[0].sessions.push(SessionInfo {
        id: "session".into(),
        title: "Session".into(),
        work_dir: local_project.clone(),
        runtime_work_dir: worktree.clone(),
        session_file: local_project.join(".threadlane/sessions/session.jsonl"),
        updated_at: 0,
        health: SessionHealth::Healthy,
        git_branch: Some("feature/session".into()),
        github_issue: None,
        is_worktree: true,
        worktree_available: true,
        completion_summary: SessionCompletionSummary::Unknown,
    });
    state.active_session_id = Some("session".into());

    assert_eq!(state.active_git_work_dir(), Some(worktree.clone()));
    assert!(!state.active_worktree_unavailable());

    state.projects[0].sessions[0].worktree_available = false;

    assert_eq!(state.active_git_work_dir(), None);
    assert!(state.active_worktree_unavailable());

    state.projects[0].sessions.clear();
    state.active_session_id = Some("missing-session".into());

    assert_eq!(state.active_git_work_dir(), None);
    // No session record exists, so there is no worktree to recreate: the
    // terminal falls back instead of offering recovery that would fail with
    // "Active session was not found".
    assert!(!state.active_worktree_unavailable());
}

#[test]
fn stale_session_refresh_results_are_discarded() {
    let project = PathBuf::from("/projects/local");
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects = vec![ProjectInfo {
        name: "Local".into(),
        work_dir: project.clone(),
        sessions: Vec::new(),
        is_expanded: true,
    }];
    state.active_work_dir = Some(project.clone());
    state.active_session_id = Some("session".into());

    // A result captured before a worktree recreation (older generation) must
    // not overwrite current sessions.
    state.session_refresh_generation = 1;
    assert!(!state.apply_session_refresh(project.clone(), Vec::new(), 0));
    assert!(state.active_session_id.as_deref() == Some("session"));
    assert!(state.apply_session_refresh(project.clone(), Vec::new(), 1));
    // The refreshed list omits the selected session, so the selection clears
    // instead of pointing at a record that no longer exists.
    assert!(state.active_session_id.is_none());
}


#[test]
fn recreate_active_worktree_restores_the_recorded_session_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let project = init_test_repo(&dir);
    std::fs::write(project.join("base.txt"), "base\n").unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-qm", "initial"]);
    run_git(&project, &["branch", "worktree/session"]);

    let session_id = "session";
    let worktree = project.join(".threadlane/worktrees/session");
    let session_file = project.join(".threadlane/sessions/session.jsonl");
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    let mut store = JsonlStore::open(&session_file).unwrap();
    store.append_fact("main", "is_worktree", "true", None).unwrap();
    store
        .append_fact("main", "worktree_path", &worktree.to_string_lossy(), None)
        .unwrap();
    store
        .append_fact("main", "git_branch", "worktree/session", None)
        .unwrap();
    drop(store);

    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: discover_sessions_in_project(&project),
        is_expanded: true,
    });
    state.active_work_dir = Some(project.clone());
    state.active_session_id = Some(session_id.into());

    assert!(state.active_worktree_unavailable());
    state.recreate_active_worktree().unwrap();

    assert_eq!(state.active_git_work_dir(), Some(worktree.clone()));
    assert!(worktree.is_dir());
    assert_eq!(
        threadlane_git::current_branch(&worktree).unwrap().as_deref(),
        Some("worktree/session")
    );
    assert!(!state.active_worktree_unavailable());
}

#[test]
fn opening_a_file_targets_the_active_session_checkout() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let worktree = temp.path().join("worktree");
    std::fs::create_dir_all(project.join(".threadlane/sessions")).unwrap();
    std::fs::create_dir_all(worktree.join("src")).unwrap();
    std::fs::write(worktree.join("src/lib.rs"), "pub fn worktree() {}\n").unwrap();
    let project = project.canonicalize().unwrap();
    let worktree = worktree.canonicalize().unwrap();
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects = vec![ProjectInfo {
        name: "Project".into(),
        work_dir: project.clone(),
        sessions: vec![SessionInfo {
            id: "session".into(),
            title: "Session".into(),
            work_dir: project.clone(),
            runtime_work_dir: worktree.clone(),
            session_file: project.join(".threadlane/sessions/session.jsonl"),
            updated_at: 0,
            health: SessionHealth::Healthy,
            git_branch: None,
            github_issue: None,
            is_worktree: true,
            worktree_available: true,
            completion_summary: SessionCompletionSummary::Unknown,
        }],
        is_expanded: true,
    }];
    state.active_work_dir = Some(project);
    state.active_session_id = Some("session".into());

    state.request_open_file("src/lib.rs".into());

    assert_eq!(
        state.requested_editor_target,
        Some(RequestedEditorTarget::File {
            project: worktree.clone(),
            path: "src/lib.rs".into(),
            line: None,
        })
    );
    state.request_open_file_at_line("src/lib.rs".into(), Some(7));
    assert_eq!(
        state.requested_editor_target,
        Some(RequestedEditorTarget::File {
            project: worktree,
            path: "src/lib.rs".into(),
            line: Some(7),
        })
    );
}

fn take_stream_events(state: &mut AppState, limit: usize) -> Vec<SessionEvent> {
    let receiver = state.stream_rx.as_mut().unwrap();
    std::iter::from_fn(|| receiver.try_recv().ok())
        .take(limit)
        .collect()
}

#[derive(Default)]
struct ModelSelectionProvider(Mutex<Vec<(String, Option<String>)>>);

#[async_trait::async_trait]
impl threadlane_protocol::ProviderPort for ModelSelectionProvider {
    async fn stream_request(
        &self,
        request: threadlane_protocol::RuntimeRequest,
        events: tokio::sync::mpsc::Sender<threadlane_protocol::RuntimeStreamEvent>,
    ) {
        self.0
            .lock()
            .unwrap()
            .push((request.model, request.reasoning_effort));
        events
            .send(threadlane_protocol::RuntimeStreamEvent::ContentToken(
                "done".into(),
            ))
            .await
            .unwrap();
        events
            .send(threadlane_protocol::RuntimeStreamEvent::Finished {
                tool_calls: vec![],
                usage: Default::default(),
            })
            .await
            .unwrap();
    }

    async fn fetch_deferred(
        &self,
        _model: &str,
        _handle_id: &str,
    ) -> Result<threadlane_protocol::DeferredResponse, String> {
        Ok(threadlane_protocol::DeferredResponse::Pending)
    }

    async fn cancel_deferred(&self, _model: &str, _handle_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn provider_kind(&self, _model: &str) -> &'static str {
        "test"
    }
}

#[tokio::test]
async fn model_and_reasoning_pickers_persist_before_rebuild_and_next_request() {
    let selected = "opencode-go/minimax-m2.7";
    for has_runtime in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let session_file = temp.path().join("session.jsonl");
        let options = || threadlane_coding_agent::CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: temp.path().into(),
            session_file: Some(session_file.clone()),
            system_prompt: Default::default(),
            agent_config: None,
            coding_config: None,
            browser: threadlane_protocol::browser::BrowserBridge::unavailable(),
        };
        let mut original = threadlane_coding_agent::CodingAgent::new(options());
        original.set_fact("model", "gpt-4o").unwrap();
        drop(original);
        let mut state = AppState::load_from_registry(Vec::new());
        activate_test_session(&mut state, "session", &session_file);
        // Before hydration the picker can still show another session's
        // selection. Clicking it must update this session's stored model.
        state.selected_model = if has_runtime { "gpt-4o" } else { selected }.into();
        state.available_models = vec![threadlane_daemon::catalog::ModelOption {
            id: selected.into(),
            label: "MiniMax M2.7".into(),
            provider: threadlane_daemon::catalog::ModelProvider::OpenCode,
        }];
        if has_runtime {
            state.active_session_runtime().unwrap();
        }

        state.set_selected_model(selected.into());
        state.set_reasoning_effort(ReasoningEffort::High);

        assert_eq!(state.selected_model, selected);
        assert_eq!(state.daemon_core.runtime_for_file(&session_file).unwrap().model(), selected);
        assert_eq!(
            state.daemon_core.runtime_for_file(&session_file).unwrap().reasoning_effort(),
            ReasoningEffort::High
        );
        assert_eq!(
            JsonlStore::open_read_only(&session_file).unwrap().facts()["model"],
            selected
        );
        assert_eq!(
            JsonlStore::open_read_only(&session_file).unwrap().facts()["reasoning_effort"],
            "High"
        );
        drop(state);

        // Reload with the old default, as startup does, then drive the real
        // CodingAgent/harness path with only the network transport replaced.
        let provider = Arc::new(ModelSelectionProvider::default());
        let mut restored =
            threadlane_coding_agent::controller::test_support::coding_agent_with_provider(
                options(),
                provider.clone(),
            );
        let result = restored.handle_input_with_images("continue", vec![]).await;
        assert!(result.is_none(), "generation failed: {result:?}");
        assert_eq!(
            *provider.0.lock().unwrap(),
            [(selected.to_string(), Some("high".into()))]
        );
        let store = JsonlStore::open_read_only(&session_file).unwrap();
        assert!(store.records().iter().any(|record| matches!(
            record,
            Record::ProviderRequestStarted { model, .. } if model.as_str() == selected
        )));
    }
}

#[test]
fn model_picker_preserves_current_selection_while_runtime_is_busy() {
    let temp = tempfile::tempdir().unwrap();
    let session_file = temp.path().join("session.jsonl");
    let mut state = AppState::load_from_registry(Vec::new());
    activate_test_session(&mut state, "session", &session_file);
    state.selected_model = "gpt-4o".into();
    state.available_models = vec![threadlane_daemon::catalog::ModelOption {
        id: "opencode-go/minimax-m2.7".into(),
        label: "MiniMax M2.7".into(),
        provider: threadlane_daemon::catalog::ModelProvider::OpenCode,
    }];
    let (runtime, _) = state.active_session_runtime().unwrap();
    runtime
        .agent
        .try_lock()
        .unwrap()
        .set_fact("model", "gpt-4o")
        .unwrap();
    runtime.begin_generation().unwrap();

    state.set_selected_model("opencode-go/minimax-m2.7".into());

    assert_eq!(state.selected_model, "gpt-4o");
    assert_eq!(
        state.session_status.as_deref(),
        Some("Stop the current turn before changing models")
    );
    runtime.finish_generation(None);
    let _settings = runtime.agent.try_lock().unwrap();
    state.set_selected_model("opencode-go/minimax-m2.7".into());
    assert_eq!(state.selected_model, "gpt-4o");
    assert!(
        state
            .session_status
            .as_deref()
            .unwrap()
            .contains("settings are still loading")
    );
    assert!(Arc::ptr_eq(
        &state.daemon_core.runtime_for_file(&session_file).unwrap(),
        &runtime
    ));
    assert_eq!(
        JsonlStore::open_read_only(&session_file).unwrap().facts()["model"],
        "gpt-4o"
    );
    drop(_settings);
    state.set_selected_model("opencode-go/minimax-m2.7".into());
    assert_eq!(state.selected_model, "opencode-go/minimax-m2.7");
    assert!(state.session_status.is_none());
}

#[test]
fn new_task_acp_pick_is_remembered_until_first_turn() {
    // New task has no session/runtime, so the picker cannot talk to an
    // agent yet. The choice must wait as pending instead of failing with
    // "Open a session..." and leaving the default (DeepSeek) selected.
    let mut state = AppState::load_from_registry(Vec::new());
    state.active_work_dir = Some(PathBuf::from("/project"));
    state.active_session_id = None;
    state.is_new_task = true;
    state.selected_model = "acp/opencode2".into();

    state.set_acp_config_option("model".into(), "muse".into());

    assert_eq!(
        state
            .pending_acp_config
            .get("opencode2")
            .and_then(|m| m.get("model"))
            .map(String::as_str),
        Some("muse")
    );
    assert!(state.session_status.is_none());
    // Cleared exactly once when the first turn takes it.
    let taken = state.take_pending_acp_config("opencode2");
    assert_eq!(taken, vec![("model".to_string(), "muse".to_string())]);
    assert!(!state.pending_acp_config.contains_key("opencode2"));
}

#[test]
fn model_picker_ignores_acp_replies_from_replaced_or_inactive_runtimes() {
    let temp = tempfile::tempdir().unwrap();
    let file_a = temp.path().join("a/session.jsonl");
    let file_b = temp.path().join("b/session.jsonl");
    let mut state = AppState::load_from_registry(Vec::new());
    activate_test_session(&mut state, "session", &file_a);
    state.selected_model = "acp/test".into();
    let (old, _) = state.active_session_runtime().unwrap();
    state.daemon_core.drop_runtime(&file_a);
    let (current, _) = state.active_session_runtime().unwrap();
    let reply = |runtime: &Arc<SessionRuntime>, label: &str, error: Option<&str>| {
        SessionEvent::AcpConfigOptions {
            session_id: "session".into(),
            session_file: runtime.session_file.clone(),
            runtime_instance: runtime.instance_id(),
            options: vec![
                serde_json::from_value(serde_json::json!({
                    "id": "model", "name": "Model", "category": "model",
                    "currentValue": "model", "options": [{ "value": "model", "name": label }]
                }))
                .unwrap(),
            ],
            error: error.map(str::to_string),
            failed_config: None,
        }
    };
    state.session_status = Some("Existing status".into());

    assert!(!state.drain_chat_stream(vec![reply(&old, "Stale model", Some("Stale error"))]));
    assert!(state.active_acp_config_options().is_empty());
    assert_eq!(state.session_status.as_deref(), Some("Existing status"));
    assert!(state.drain_chat_stream(vec![reply(&current, "Current model", None)]));
    assert_eq!(
        state.active_acp_model_label().as_deref(),
        Some("Current model")
    );
    assert!(state.drain_chat_stream(vec![reply(&current, "Wrong model", Some("Try again"))]));
    assert_eq!(
        state.active_acp_model_label().as_deref(),
        Some("Current model")
    );

    // Equal session ids in different projects still have distinct settings
    // and an inactive session's error cannot replace the active status.
    activate_test_session(&mut state, "session", &file_b);
    let (other, _) = state.active_session_runtime().unwrap();
    state.session_status = Some("Other session status".into());
    assert!(!state.drain_chat_stream(vec![reply(&current, "Wrong model", Some("Inactive error"))]));
    assert_eq!(
        state.session_status.as_deref(),
        Some("Other session status")
    );
    assert!(state.active_acp_config_options().is_empty());
    assert!(state.drain_chat_stream(vec![reply(&other, "Other model", None)]));
    assert_eq!(
        state.active_acp_model_label().as_deref(),
        Some("Other model")
    );
    state.selected_model = "gpt-4o".into();
    assert!(state.active_acp_model_label().is_none());
}

fn permission_request(id: &str) -> threadlane_protocol::PermissionRequest {
    threadlane_protocol::PermissionRequest {
        id: id.into(),
        capability: "network".into(),
        title: "Connect to api.example.test".into(),
        detail: "https://api.example.test".into(),
        scopes: vec![threadlane_protocol::PermissionScope::Once],
    }
}

fn question_request(id: &str) -> threadlane_protocol::QuestionRequest {
    threadlane_protocol::QuestionRequest {
        id: id.into(),
        questions: vec![threadlane_protocol::QuestionItem {
            id: "q1".into(),
            header: "Scope".into(),
            question: "Which scope should be used?".into(),
            options: vec!["small".into(), "full".into()],
            allow_custom: true,
        }],
    }
}

fn test_session(id: &str, session_file: &Path) -> SessionInfo {
    let work_dir = session_file
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .or_else(|| session_file.parent())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    SessionInfo {
        id: id.into(),
        title: id.into(),
        work_dir: work_dir.clone(),
        runtime_work_dir: work_dir,
        session_file: session_file.to_path_buf(),
        updated_at: 0,
        health: SessionHealth::Healthy,
        git_branch: None,
        github_issue: None,
        is_worktree: false,
        worktree_available: true,
        completion_summary: SessionCompletionSummary::Unknown,
    }
}

#[test]
fn queued_messages_survive_session_switch_and_remain_editable() {
    let root = tempfile::tempdir().unwrap();
    let work_dir = root.path().canonicalize().unwrap();
    let sessions_dir = work_dir.join(".threadlane/sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_file = sessions_dir.join("queue-session.jsonl");
    let other_file = sessions_dir.join("other-session.jsonl");
    let mut state = AppState::for_tests();
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: work_dir.clone(),
        sessions: vec![
            test_session("queue-session", &session_file),
            test_session("other-session", &other_file),
        ],
        is_expanded: true,
    });
    state.select_session(work_dir.clone(), "queue-session".into());
    let runtime = state.ensure_session_runtime(work_dir.clone(), session_file.clone());
    runtime.begin_generation().unwrap();
    // An active turn holds the agent while follow-ups wait in its durable queue.
    let _running_turn = runtime.agent.try_lock().unwrap();
    state.is_generating = true;
    let images = vec![ImageAttachment {
        display_name: "shot.png".into(),
        data_url: "data:image/png;base64,AA==".into(),
    }];
    for _ in 0..2 {
        state
            .stage_busy_message("same follow-up".into(), images.clone())
            .unwrap();
        state.queue_pending_message().unwrap();
    }
    let queued_ids: Vec<_> = state
        .messages
        .iter()
        .map(|message| message.id.clone())
        .collect();
    assert_eq!(queued_ids.len(), 2);
    assert_ne!(queued_ids[0], queued_ids[1]);
    assert_eq!(
        compute_session_messages_for_hydration(&session_file).unwrap().len(),
        2,
        "both inputs must be durable before switching"
    );

    for _ in 0..2 {
        state.select_session(work_dir.clone(), "other-session".into());
        state.apply_session_messages(
            "other-session",
            &other_file,
            compute_session_messages_for_hydration(&other_file).unwrap(),
            None,
        );
        assert!(
            state.messages.is_empty(),
            "queues must not leak into another session"
        );
        state.select_session(work_dir.clone(), "queue-session".into());
        assert!(state.is_generating);
        state.apply_session_messages(
            "queue-session",
            &session_file,
            compute_session_messages_for_hydration(&session_file).unwrap(),
            None,
        );
        assert_eq!(
            state
                .messages
                .iter()
                .map(|message| message.id.clone())
                .collect::<Vec<_>>(),
            queued_ids,
            "hydration must restore each queued occurrence with its actionable identity",
        );
    }

    let prefix = "queued-user-queue-session-";
    let (text, restored_images) = state
        .edit_queued_message(queued_ids[0].strip_prefix(prefix).unwrap())
        .unwrap();
    assert_eq!(text, "same follow-up");
    assert_eq!(restored_images, images);
    state.apply_session_messages(
        "queue-session",
        &session_file,
        compute_session_messages_for_hydration(&session_file).unwrap(),
        None,
    );
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0].id, queued_ids[1]);
    state
        .cancel_queued_message(queued_ids[1].strip_prefix(prefix).unwrap())
        .unwrap();
    state.apply_session_messages(
        "queue-session",
        &session_file,
        compute_session_messages_for_hydration(&session_file).unwrap(),
        None,
    );
    assert!(
        state.messages.is_empty(),
        "cancelled entries must not return on hydration"
    );
    state.scheduler_handles.remove(&session_file);
}

#[test]
fn inactive_permission_is_visible_before_session_selection() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.active_session_id = Some("foreground".into());
    let session = test_session("background", Path::new("/project/background.jsonl"));
    let request = permission_request("permission-1");

    let changed = state.drain_chat_stream(vec![SessionEvent::Agent {
        session_id: session.id.clone(),
        event: AgentEvent::PermissionRequested {
            request: request.clone(),
        },
    }]);

    assert!(changed);
    assert_eq!(state.pending_permissions.get(&session.id), Some(&request));
    assert_eq!(
        state.session_attention(&session),
        SessionAttention::NeedsYou
    );
    let deferred = &state.deferred_stream_events[&session.id];
    assert_eq!(deferred.len(), 1);
    assert!(matches!(
        &deferred[0],
        SessionEvent::Agent {
            session_id,
            event: AgentEvent::PermissionRequested { request: deferred },
        } if session_id == &session.id && deferred == &request
    ));
}

#[test]
fn inactive_finished_clears_live_permission_attention() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.active_session_id = Some("foreground".into());
    let session = test_session("background", Path::new("/project/background.jsonl"));
    let request = permission_request("permission-1");
    assert!(state.drain_chat_stream(vec![SessionEvent::Agent {
        session_id: session.id.clone(),
        event: AgentEvent::PermissionRequested { request },
    }]));

    let changed = state.drain_chat_stream(vec![SessionEvent::Finished {
        session_id: session.id.clone(),
        session_file: session.session_file.clone(),
    }]);

    assert!(changed);
    assert!(!state.pending_permissions.contains_key(&session.id));
    assert_eq!(state.session_attention(&session), SessionAttention::Idle);
    let deferred = &state.deferred_stream_events[&session.id];
    assert_eq!(deferred.len(), 2);
    assert!(matches!(deferred[0], SessionEvent::Agent { .. }));
    assert!(matches!(deferred[1], SessionEvent::Finished { .. }));
}

#[test]
fn scheduled_completion_updates_only_matching_active_session() {
    let mut state = AppState::load_from_registry(Vec::new());
    let active = test_session("active", Path::new("/project/active.jsonl"));
    let background = test_session("background", Path::new("/project/background.jsonl"));
    state.active_session_id = Some(active.id.clone());
    state.active_work_dir = Some(Path::new("/project").to_path_buf());
    state.is_generating = true;

    let background_file = state.session_file(Path::new("/project"), &background.id);
    let (scheduled_tx, scheduled_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .scheduler_results
        .insert(background_file.clone(), scheduled_rx);
    scheduled_tx
        .send(SchedulerSupervisorEvent::Completed(Some(Ok(
            "background complete".into(),
        ))))
        .unwrap();
    assert!(state.drain_chat_stream(Vec::new()));
    assert_eq!(
        state.session_attention(&background),
        SessionAttention::Ready
    );
    assert!(state.drain_chat_stream(vec![SessionEvent::Scheduled {
        session_id: background.id.clone(),
        session_file: state.session_file(Path::new("/project"), &background.id),
        result: Some(Err("background failure".into())),
    }]));
    assert_eq!(
        state.session_attention(&background),
        SessionAttention::NeedsYou
    );

    state.active_session_id = Some(background.id.clone());
    state.active_work_dir = Some(Path::new("/project").to_path_buf());
    assert!(state.drain_chat_stream(Vec::new()));
    assert!(state.messages.iter().any(|message| {
        message.role == MessageRole::Assistant && message.content == "background complete"
    }));
    state.active_session_id = Some(active.id.clone());
    let active_file = state.session_file(Path::new("/project"), &active.id);
    assert!(state.drain_chat_stream(vec![SessionEvent::Scheduled {
        session_id: active.id.clone(),
        session_file: active_file,
        result: Some(Err("scheduled failure".into())),
    }]));
    assert!(!state.is_generating);
    assert_eq!(state.session_status.as_deref(), Some("scheduled failure"));
    assert!(state.messages.iter().any(|message| {
        message.role == MessageRole::Error && message.content == "scheduled failure"
    }));
    let active_file = state.session_file(Path::new("/project"), &active.id);
    assert!(state.drain_chat_stream(vec![SessionEvent::Scheduled {
        session_id: active.id.clone(),
        session_file: active_file,
        result: None,
    }]));
    assert!(state.messages.iter().any(|message| {
        message.role == MessageRole::System && message.content == "Scheduled work completed"
    }));
}

#[test]
fn active_question_stays_pending_until_answered() {
    let mut state = AppState::load_from_registry(Vec::new());
    let session = test_session("active", Path::new("/project/active.jsonl"));
    state.active_session_id = Some(session.id.clone());
    state.active_work_dir = Some(Path::new("/project").to_path_buf());
    let request = question_request("question-1");

    let changed = state.drain_chat_stream(vec![SessionEvent::Agent {
        session_id: session.id.clone(),
        event: AgentEvent::QuestionRequested {
            request: request.clone(),
        },
    }]);

    assert!(changed);
    assert_eq!(state.pending_questions.get(&session.id), Some(&request));
    assert!(state.messages.iter().any(|message| {
        message.role == MessageRole::System
            && message.content.contains("Which scope should be used?")
    }));
    // The request must stay pending: the run blocks until the user
    // answers or explicitly dismisses. Never auto-resolve here.
    assert_eq!(
        state.session_attention(&session),
        SessionAttention::NeedsYou
    );
    // No runtime is attached in the test, so nothing resolves the request;
    // explicit dismiss still returns false without a runtime, and the
    // Finished arm always drops the pending entry.
    assert!(!state.resolve_active_question(&request.id));
    assert_eq!(state.pending_questions.get(&session.id), Some(&request));
    let changed = state.drain_chat_stream(vec![SessionEvent::Finished {
        session_id: session.id.clone(),
        session_file: session.session_file.clone(),
    }]);
    assert!(changed);
    assert!(!state.pending_questions.contains_key(&session.id));
}

#[test]
fn session_attention_obeys_blocking_working_ready_idle_precedence() {
    let error = SessionRuntimeStatus::Error("provider failed".into());
    let working = SessionRuntimeStatus::Working;

    assert_eq!(
        derive_session_attention(true, &SessionHealth::Working, Some(&working), true, true),
        SessionAttention::NeedsYou
    );
    assert_eq!(
        derive_session_attention(false, &SessionHealth::Warning, None, false, true),
        SessionAttention::NeedsYou
    );
    assert_eq!(
        derive_session_attention(false, &SessionHealth::Healthy, Some(&error), true, true),
        SessionAttention::NeedsYou
    );
    assert_eq!(
        derive_session_attention(
            false,
            &SessionHealth::Healthy,
            Some(&SessionRuntimeStatus::Interrupted),
            false,
            true,
        ),
        SessionAttention::NeedsYou
    );
    assert_eq!(
        derive_session_attention(false, &SessionHealth::Healthy, Some(&working), false, true),
        SessionAttention::Working
    );
    assert_eq!(
        derive_session_attention(false, &SessionHealth::Working, None, false, true),
        SessionAttention::Working
    );
    assert_eq!(
        derive_session_attention(false, &SessionHealth::Healthy, None, false, true),
        SessionAttention::Ready
    );
    assert_eq!(
        derive_session_attention(false, &SessionHealth::Healthy, None, false, false),
        SessionAttention::Idle
    );
}

#[test]
fn completed_pr_and_missing_worktree_are_not_attention_without_other_work() {
    let mut state = AppState::load_from_registry(Vec::new());
    let mut session = test_session("session", Path::new("/project/session.jsonl"));
    session.is_worktree = true;
    session.worktree_available = false;
    assert_eq!(state.session_attention(&session), SessionAttention::Idle);

    session.git_branch = Some("feature/session".into());
    let pr_key = (session.work_dir.clone(), "feature/session".into());
    for completed_state in ["MERGED", "CLOSED"] {
        state.git_prs.insert(
            pr_key.clone(),
            Some(threadlane_git::GitHubPrInfo {
                state: completed_state.into(),
                is_draft: true,
                ..Default::default()
            }),
        );
        assert_eq!(state.session_attention(&session), SessionAttention::Idle);
    }

    for active_state in ["OPEN", "DRAFT"] {
        state.git_prs.insert(
            pr_key.clone(),
            Some(threadlane_git::GitHubPrInfo {
                state: active_state.into(),
                is_draft: active_state == "DRAFT",
                ..Default::default()
            }),
        );
        assert_eq!(state.session_attention(&session), SessionAttention::Ready);
    }

    state.git_prs.insert(
        pr_key,
        Some(threadlane_git::GitHubPrInfo {
            state: "MERGED".into(),
            ..Default::default()
        }),
    );
    state.git_statuses.insert(
        session.runtime_work_dir.clone(),
        threadlane_git::GitStatus {
            has_changes: true,
            ..Default::default()
        },
    );
    assert_eq!(state.session_attention(&session), SessionAttention::Idle);
}

#[test]
fn project_git_status_marks_sessions_in_the_checkout_ready() {
    let mut state = AppState::load_from_registry(Vec::new());
    let session_file = Path::new("/project/.threadlane/sessions/current.jsonl");
    let active = test_session("current", session_file);
    let historical = test_session(
        "historical",
        Path::new("/project/.threadlane/sessions/old.jsonl"),
    );

    state.active_work_dir = Some(active.work_dir.clone());
    state.active_session_id = Some(active.id.clone());
    state.git_statuses.insert(
        active.runtime_work_dir.clone(),
        threadlane_git::GitStatus {
            has_changes: true,
            ..Default::default()
        },
    );

    assert_eq!(state.session_attention(&active), SessionAttention::Ready);
    assert_eq!(
        state.session_attention(&historical),
        SessionAttention::Ready
    );
}

#[test]
fn worktree_session_does_not_inherit_main_checkout_git_status() {
    let mut state = AppState::load_from_registry(Vec::new());
    let mut session = test_session(
        "worktree",
        Path::new("/project/.threadlane/sessions/worktree.jsonl"),
    );
    session.is_worktree = true;
    session.runtime_work_dir = PathBuf::from("/project/.threadlane/worktrees/worktree");

    state.git_statuses.insert(
        session.work_dir.clone(),
        threadlane_git::GitStatus {
            has_changes: true,
            ..Default::default()
        },
    );

    assert_eq!(state.session_attention(&session), SessionAttention::Idle);
}
#[test]
fn inactive_start_and_error_wake_attention_observers() {
    let dir = tempfile::tempdir().unwrap();
    let session_file = dir.path().join(".threadlane/sessions/background.jsonl");
    let session = test_session("background", &session_file);
    let mut state = AppState::load_from_registry(Vec::new());
    state.active_session_id = Some("foreground".into());
    let runtime = state.ensure_session_runtime(
        session.runtime_work_dir.clone(),
        session.session_file.clone(),
    );
    runtime.begin_generation().unwrap();

    assert!(state.drain_chat_stream(vec![SessionEvent::Agent {
        session_id: session.id.clone(),
        event: AgentEvent::AgentStart,
    }]));
    assert_eq!(state.session_attention(&session), SessionAttention::Working);

    runtime.finish_generation(Some("provider failed".into()));
    assert!(state.drain_chat_stream(vec![SessionEvent::Agent {
        session_id: session.id.clone(),
        event: AgentEvent::AgentError {
            error: "provider failed".into(),
        },
    }]));
    assert_eq!(
        state.session_attention(&session),
        SessionAttention::NeedsYou
    );
    assert_eq!(state.deferred_stream_events[&session.id].len(), 2);
}

#[test]
fn worktree_queue_and_stop_use_the_existing_session_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().canonicalize().unwrap();
    let worktree = project.join("worktree");
    let session_file = worktree.join(".threadlane/sessions/worktree-session.jsonl");
    let mut session = test_session("worktree-session", &session_file);
    session.work_dir = project.clone();
    session.is_worktree = true;
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: vec![session.clone()],
        is_expanded: true,
    });
    state.active_work_dir = Some(project);
    state.active_session_id = Some(session.id.clone());
    let runtime = state.ensure_session_runtime(worktree, session_file);
    runtime.begin_generation().unwrap();
    state.is_generating = true;
    state
        .stage_busy_message("Follow up in this worktree".into(), Vec::new())
        .unwrap();

    let (pending_runtime, _, text, _) = state.pending_runtime_message().unwrap();
    assert!(Arc::ptr_eq(&runtime, &pending_runtime));
    assert_eq!(text, "Follow up in this worktree");
    state.cancel_generation().unwrap();
    assert!(!state.is_generating);
    assert!(!runtime.is_generating());
    assert_eq!(runtime.status(), SessionRuntimeStatus::Ready);
    assert_eq!(state.session_attention(&session), SessionAttention::Idle);
    assert_eq!(
        state.session_status.as_deref(),
        Some("Generation cancelled")
    );
    let events = take_stream_events(&mut state, 16);
    state.drain_chat_stream(events);
    assert_eq!(state.session_attention(&session), SessionAttention::Idle);
}

#[test]
fn removed_session_clears_live_and_deferred_attention() {
    let dir = tempfile::tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let session_file = work_dir.join(".threadlane/sessions/background.jsonl");
    let session = test_session("background", &session_file);
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: work_dir.clone(),
        sessions: vec![session.clone()],
        is_expanded: true,
    });
    state
        .pending_permissions
        .insert(session.id.clone(), permission_request("permission-1"));
    state.deferred_stream_events.insert(
        session.id.clone(),
        vec![SessionEvent::Finished {
            session_id: session.id.clone(),
            session_file,
        }],
    );

    state.finish_session_removal(&work_dir, &session.id);

    assert!(!state.pending_permissions.contains_key(&session.id));
    assert!(!state.deferred_stream_events.contains_key(&session.id));
}

#[test]
fn removing_worktree_session_removes_checkout_and_metadata_stub() {
    let dir = tempfile::tempdir().unwrap();
    let project = init_test_repo(&dir);
    std::fs::write(project.join("base.txt"), "base\n").unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-qm", "initial"]);

    let session_id = "worktree-session";
    let worktree = project.join(".threadlane/worktrees").join(session_id);
    threadlane_git::create_worktree(&project, &worktree, "worktree/session").unwrap();
    let stub = project
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    let transcript = worktree
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&stub, "{}\n").unwrap();
    std::fs::write(&transcript, "{}\n").unwrap();

    let mut session = test_session(session_id, &transcript);
    session.work_dir = project.clone();
    session.runtime_work_dir = worktree.clone();
    session.is_worktree = true;
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: vec![session],
        is_expanded: true,
    });

    state
        .remove_session(project.clone(), session_id.into(), true)
        .unwrap();

    assert!(!worktree.exists());
    assert!(!stub.exists());
    assert!(
        threadlane_git::list_worktrees(&project)
            .unwrap()
            .iter()
            .all(|entry| entry.branch.as_deref() != Some("worktree/session"))
    );
}

#[test]
fn removing_worktree_session_retains_checkout_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let project = init_test_repo(&dir);
    std::fs::write(project.join("base.txt"), "base\n").unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-qm", "initial"]);

    let session_id = "worktree-session";
    let worktree = project.join(".threadlane/worktrees").join(session_id);
    threadlane_git::create_worktree(&project, &worktree, "worktree/session").unwrap();
    let stub = project
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    let transcript = worktree
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&stub, "{}\n").unwrap();
    std::fs::write(&transcript, "{}\n").unwrap();

    let mut session = test_session(session_id, &transcript);
    session.work_dir = project.clone();
    session.runtime_work_dir = worktree.clone();
    session.is_worktree = true;
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: vec![session],
        is_expanded: true,
    });

    state
        .remove_session(project.clone(), session_id.into(), false)
        .unwrap();

    assert!(worktree.exists());
    assert!(!stub.exists());
    assert!(
        threadlane_git::list_worktrees(&project)
            .unwrap()
            .iter()
            .any(|entry| entry.branch.as_deref() == Some("worktree/session"))
    );
}

#[test]
fn settling_worktree_session_removes_checkout_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let project = init_test_repo(&dir);
    std::fs::write(project.join("base.txt"), "base\n").unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-qm", "initial"]);

    let session_id = "worktree-session";
    let worktree = project.join(".threadlane/worktrees").join(session_id);
    threadlane_git::create_worktree(&project, &worktree, "worktree/session").unwrap();
    let stub = project
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    let transcript = worktree
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&stub, "{}\n").unwrap();
    std::fs::write(&transcript, "{}\n").unwrap();

    let mut session = test_session(session_id, &transcript);
    session.work_dir = project.clone();
    session.runtime_work_dir = worktree.clone();
    session.is_worktree = true;
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: vec![session],
        is_expanded: true,
    });

    state
        .settle_session(project.clone(), session_id.into(), true)
        .unwrap();

    let archive_file = project
        .join(".threadlane/sessions/archive")
        .join(format!("{session_id}.jsonl"));
    assert!(archive_file.exists());
    assert!(!worktree.exists());
    assert!(!stub.exists());
    assert!(
        threadlane_git::list_worktrees(&project)
            .unwrap()
            .iter()
            .all(|entry| entry.branch.as_deref() != Some("worktree/session"))
    );
}

#[test]
fn settling_worktree_session_retains_checkout_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let project = init_test_repo(&dir);
    std::fs::write(project.join("base.txt"), "base\n").unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-qm", "initial"]);

    let session_id = "worktree-session";
    let worktree = project.join(".threadlane/worktrees").join(session_id);
    threadlane_git::create_worktree(&project, &worktree, "worktree/session").unwrap();
    let stub = project
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    let transcript = worktree
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&stub, "{}\n").unwrap();
    std::fs::write(&transcript, "{}\n").unwrap();

    let mut session = test_session(session_id, &transcript);
    session.work_dir = project.clone();
    session.runtime_work_dir = worktree.clone();
    session.is_worktree = true;
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: vec![session],
        is_expanded: true,
    });

    state
        .settle_session(project.clone(), session_id.into(), false)
        .unwrap();

    let archive_file = project
        .join(".threadlane/sessions/archive")
        .join(format!("{session_id}.jsonl"));
    assert!(archive_file.exists());
    assert!(worktree.exists());
    assert!(!stub.exists());
    assert!(
        threadlane_git::list_worktrees(&project)
            .unwrap()
            .iter()
            .any(|entry| entry.branch.as_deref() == Some("worktree/session"))
    );
}

#[test]
fn session_discovery_restores_its_last_git_branch() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let work_dir = std::env::temp_dir().join(format!("threadlane-session-branch-{unique}"));
    let session_file = work_dir.join(".threadlane/sessions/session.jsonl");
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    let mut store = JsonlStore::open(&session_file).unwrap();
    store
        .append_fact("main", "git_branch", "feature/session", None)
        .unwrap();
    drop(store);

    let sessions = discover_sessions_in_project(&work_dir);

    assert_eq!(sessions[0].git_branch.as_deref(), Some("feature/session"));
    std::fs::remove_dir_all(work_dir).ok();
}

#[test]
fn session_discovery_keeps_canonical_project_and_runtime_worktree_separate() {
    let project_dir = tempfile::tempdir().unwrap();
    let session_file = project_dir
        .path()
        .join(".threadlane/sessions/session.jsonl");
    let runtime_work_dir = project_dir.path().join(".threadlane/worktrees/session");
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    let mut store = JsonlStore::open(&session_file).unwrap();
    store
        .append_fact("main", "is_worktree", "true", None)
        .unwrap();
    store
        .append_fact(
            "main",
            "worktree_path",
            &runtime_work_dir.to_string_lossy(),
            None,
        )
        .unwrap();
    drop(store);

    let sessions = discover_sessions_in_project(project_dir.path());
    let session = &sessions[0];
    assert_eq!(session.work_dir, project_dir.path().canonicalize().unwrap());
    assert_eq!(
        session.runtime_work_dir,
        session.work_dir.join(".threadlane/worktrees/session")
    );
    assert!(session.is_worktree);
    assert!(!session.worktree_available);

    std::fs::create_dir_all(&session.runtime_work_dir).unwrap();
    let sessions = discover_sessions_in_project(project_dir.path());
    assert!(sessions[0].worktree_available);
}

#[test]
fn github_issue_survives_worktree_transcript_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let session_id = "session-worktree";
    let root_session_file = project
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    let worktree = project.join(".threadlane/worktrees").join(session_id);
    let worktree_session_file = worktree
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    std::fs::create_dir_all(root_session_file.parent().unwrap()).unwrap();
    std::fs::create_dir_all(worktree_session_file.parent().unwrap()).unwrap();

    let mut stub = JsonlStore::open(&root_session_file).unwrap();
    stub.append_fact("main", "is_worktree", "true", None)
        .unwrap();
    stub.append_fact(
        "main",
        "worktree_path",
        worktree.to_string_lossy().as_ref(),
        None,
    )
    .unwrap();
    let issue = threadlane_git::GitHubIssueRef {
        host: "github.com".into(),
        owner: "threadlane".into(),
        repo: "threadlane".into(),
        number: 42,
        url: "https://github.com/threadlane/threadlane/issues/42".into(),
    };
    stub.append_fact(
        "main",
        "github_issue",
        &serde_json::to_string(&issue).unwrap(),
        None,
    )
    .unwrap();
    drop(stub);

    let mut transcript = JsonlStore::open(&worktree_session_file).unwrap();
    transcript
        .append_entry(threadlane_runtime::harness::Entry {
            id: "message-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: transcript.next_sequence(),
            timestamp: 1,
            message: AgentMessage::user("Persisted history", Vec::new()),
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    drop(transcript);

    let sessions = discover_sessions_in_project(&project);
    let mut cache = SessionDiscoveryCache::default();
    let _ = discover_sessions_in_project_cached(&project, &mut cache);
    let cached_sessions = discover_sessions_in_project_cached(&project, &mut cache);

    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].session_file,
        worktree_session_file.canonicalize().unwrap()
    );
    assert_eq!(sessions[0].work_dir, project.canonicalize().unwrap());
    assert_eq!(
        sessions[0].runtime_work_dir,
        worktree.canonicalize().unwrap()
    );
    assert_eq!(sessions[0].github_issue, Some(issue.clone()));
    assert_eq!(cached_sessions[0].github_issue, Some(issue));
    let messages = compute_session_messages(&sessions[0].session_file).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "Persisted history");
}

#[test]
fn issue_branch_name_slugs_titles_and_uses_the_session_suffix() {
    assert_eq!(
        AppState::issue_branch_name(123, "Fix flaky auth!", "abcdef"),
        "issue/123-fix-flaky-auth-abcdef"
    );
    assert_eq!(
        AppState::issue_branch_name(7, "___", "123456"),
        "issue/7-task-123456"
    );
}

fn run_git(work_dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(work_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Init a fixture repo with host-global git hooks neutralized: hooks that
/// fire on checkout (e.g. tooling that auto-inits sidecar dirs) create
/// untracked droppings that are environment noise, not session state, and
/// race worktree setup/removal assertions. Every test that creates worktrees
/// must build its repo through this helper.
fn init_test_repo(dir: &tempfile::TempDir) -> PathBuf {
    let project = dir.path().canonicalize().unwrap();
    run_git(&project, &["init", "-b", "main"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "Test"]);
    let empty_hooks = dir.path().join("empty-hooks");
    std::fs::create_dir_all(&empty_hooks).unwrap();
    run_git(
        &project,
        &["config", "core.hooksPath", empty_hooks.to_str().unwrap()],
    );
    project
}

fn issue_ref(number: u64) -> threadlane_git::GitHubIssueRef {
    threadlane_git::GitHubIssueRef {
        host: "github.com".into(),
        owner: "threadlane".into(),
        repo: "threadlane".into(),
        number,
        url: format!("https://github.com/threadlane/threadlane/issues/{number}"),
    }
}

fn issue_work_state(work_dir: &Path) -> AppState {
    let work_dir = work_dir.canonicalize().unwrap();
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects = vec![ProjectInfo {
        name: "Project".into(),
        work_dir: work_dir.clone(),
        sessions: Vec::new(),
        is_expanded: true,
    }];
    state.active_work_dir = Some(work_dir);
    state.active_session_id = None;
    state
}

#[test]
fn fork_session_targets_context_session_and_preserves_shared_checkout_after_reload() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().canonicalize().unwrap();
    let source_id = "source";
    let checkout = project.join(".threadlane/worktrees/source");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("uncommitted.txt"), "user work").unwrap();
    let source = canonical_session_file(&project, source_id);
    for (key, value) in [
        ("is_worktree", "true"),
        ("worktree_path", checkout.to_str().unwrap()),
        ("model", "openai/test"),
    ] {
        CodingSessionHarness::append_fact_to_path(&source, "main", key, value, None).unwrap();
    }
    // Discovery must fork the full local transcript, not the metadata stub.
    let local_source = canonical_session_file(&checkout, source_id);
    CodingSessionHarness::append_fact_to_path(&local_source, "main", "name", "Local history", None)
        .unwrap();
    let before = std::fs::read(&source).unwrap();
    let mut state = issue_work_state(&project);
    state.projects[0].sessions = discover_sessions_in_project(&project);
    state.active_session_id = Some("different-session".into());
    let fork_id = state
        .fork_session(project.clone(), source_id.into())
        .unwrap();
    assert_eq!(state.active_session_id.as_deref(), Some(fork_id.as_str()));
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let sessions = discover_sessions_in_project(&project);
    let fork = sessions
        .iter()
        .find(|session| session.id == fork_id)
        .unwrap();
    assert_eq!(fork.runtime_work_dir, checkout);
    assert!(fork.is_worktree && fork.worktree_available);
    assert_eq!(fork.title, "Local history (fork)");
    assert_eq!(
        discover_session_stubs_in_project(&project)
            .iter()
            .find(|session| session.id == fork_id)
            .unwrap()
            .runtime_work_dir,
        checkout
    );
    assert!(
        state
            .remove_session(project.clone(), source_id.into(), true)
            .unwrap_err()
            .contains("another session")
    );
    assert!(
        state
            .settle_session(project.clone(), fork_id.clone(), true)
            .unwrap_err()
            .contains("another session")
    );
    state
        .remove_session(project.clone(), source_id.into(), false)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(checkout.join("uncommitted.txt")).unwrap(),
        "user work"
    );
    assert_eq!(
        discover_sessions_in_project(&project)[0].runtime_work_dir,
        checkout
    );
    let second_fork = state.fork_session(project.clone(), fork_id).unwrap();
    assert_eq!(
        state.session_runtime_work_dir(&project, &second_fork),
        checkout
    );
}

#[test]
fn fork_session_rejects_missing_checkout_and_unknown_session_without_switching() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().canonicalize().unwrap();
    let source = canonical_session_file(&project, "missing");
    CodingSessionHarness::append_fact_to_path(&source, "main", "is_worktree", "true", None)
        .unwrap();
    let mut state = issue_work_state(&project);
    state.projects[0].sessions = discover_sessions_in_project(&project);
    assert!(
        state
            .fork_session(project.clone(), "missing".into())
            .is_err()
    );
    assert!(
        state
            .fork_session(project.clone(), "unknown".into())
            .is_err()
    );
    assert!(state.active_session_id.is_none());
    assert_eq!(discover_sessions_in_project(&project).len(), 1);
}

#[test]
fn fork_session_preparation_defers_io_and_selection_until_worker_completion() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().canonicalize().unwrap();
    let mut state = issue_work_state(&project);
    let source_id = state.create_new_session().unwrap();
    let source = canonical_session_file(&project, &source_id);
    let runtime = state.ensure_session_runtime(project.clone(), source);
    runtime.begin_generation().unwrap();
    assert!(
        state
            .fork_session(project.clone(), source_id.clone())
            .unwrap_err()
            .contains("Stop")
    );
    runtime.finish_generation(None);
    let work = state
        .prepare_session_fork(project.clone(), source_id.clone())
        .unwrap();
    assert_eq!(state.active_session_id.as_deref(), Some(source_id.as_str()));
    assert_eq!(discover_sessions_in_project(&project).len(), 1);
    let (id, sessions) = std::thread::spawn(work).join().unwrap().unwrap();
    assert_eq!(state.active_session_id.as_deref(), Some(source_id.as_str()));
    // A foreground change made while the worker ran must not be overwritten.
    state.projects[0].sessions.clear();
    state.finish_session_fork(project.clone(), id, sessions);
    assert_eq!(state.projects[0].sessions.len(), 1);
    assert_ne!(state.active_session_id.as_deref(), Some(source_id.as_str()));
    let fork = state.active_session_info().unwrap();
    assert!(!fork.is_worktree);
    assert_eq!(fork.runtime_work_dir, project);
}

#[test]
fn fork_session_worktree_owner_cannot_escape_project() {
    let project = tempfile::tempdir().unwrap();
    for owner in ["../escape", "/tmp/escape", "a/b", ".."] {
        let facts = std::collections::BTreeMap::from([
            ("is_worktree".into(), "true".into()),
            ("worktree_owner".into(), owner.into()),
        ]);
        assert_eq!(
            crate::discovery::effective_session_work_dir(project.path(), "fork", &facts),
            project.path().join(".threadlane/worktrees/fork")
        );
    }
}

#[test]
fn new_session_persists_draft_reasoning_effort() {
    let project = tempfile::tempdir().unwrap();
    let mut state = issue_work_state(project.path());
    state.reasoning_effort = ReasoningEffort::High;

    let session_id = state.create_new_session().unwrap();
    let session_file = project
        .path()
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));

    assert_eq!(
        JsonlStore::open_read_only(session_file).unwrap().facts()["reasoning_effort"],
        "High"
    );
}

#[test]
fn issue_work_session_persists_link_and_uses_isolated_worktree() {
    let repo = tempfile::tempdir().unwrap();
    init_test_repo(&repo);
    std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    run_git(repo.path(), &["add", "."]);
    run_git(repo.path(), &["commit", "-m", "initial"]);

    let work_dir = repo.path().canonicalize().unwrap();
    let issue = issue_ref(42);
    let mut state = issue_work_state(&work_dir);
    let session_id = state
        .start_issue_work_with_prompt(
            work_dir.clone(),
            issue.clone(),
            "Fix flaky auth!".into(),
            "opencode-go/test-model".into(),
            ReasoningEffort::High,
            threadlane_protocol::OrchestratorMode::Fusion,
            |state, prompt| {
                assert_eq!(state.selected_model, "opencode-go/test-model");
                assert_eq!(state.reasoning_effort, ReasoningEffort::High);
                assert!(prompt.contains("create_draft_pull_request"));
                assert!(prompt.contains("verify the resulting PR URL"));
                assert!(prompt.ends_with(threadlane_prompt::git_workflow::PR_COMPLETION_POLICY));
                assert!(prompt.contains("authorizes marking the PR ready"));
                Ok(())
            },
        )
        .unwrap();

    let session_file = work_dir
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    let facts = JsonlStore::open_read_only(&session_file).unwrap().facts();
    assert_eq!(
        facts.get("model").map(String::as_str),
        Some("opencode-go/test-model")
    );
    assert_eq!(
        facts.get("reasoning_effort").map(String::as_str),
        Some("High")
    );
    assert_eq!(
        facts.get("orchestrator_mode").map(String::as_str),
        Some("fusion")
    );
    assert_eq!(
        threadlane_project::subagent_settings::load(&work_dir).orchestrator_mode,
        threadlane_protocol::OrchestratorMode::Fusion
    );
    assert_eq!(
        state.orchestrator_mode,
        threadlane_protocol::OrchestratorMode::Fusion
    );
    assert_eq!(facts.get("is_worktree").map(String::as_str), Some("true"));
    assert_eq!(
        facts.get("worktree_path").map(String::as_str),
        Some(
            work_dir
                .join(".threadlane/worktrees")
                .join(&session_id)
                .to_string_lossy()
                .as_ref()
        )
    );
    assert!(
        facts
            .get("git_branch")
            .is_some_and(|branch| branch.starts_with("issue/42-fix-flaky-auth-"))
    );
    assert_eq!(
        facts.get("github_issue"),
        Some(&serde_json::to_string(&issue).unwrap())
    );
    assert_eq!(
        facts.get("name").map(String::as_str),
        Some("#42 Fix flaky auth!")
    );

    let session = &state.projects[0].sessions[0];
    assert_eq!(session.github_issue, Some(issue));
    assert!(session.is_worktree);
    assert_eq!(
        state.active_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(
        state.active_git_work_dir(),
        Some(session.runtime_work_dir.clone())
    );
}

#[test]
fn issue_work_failure_never_selects_or_runs_in_canonical_checkout() {
    let repo = tempfile::tempdir().unwrap();
    run_git(repo.path(), &["init", "-b", "main"]);
    let work_dir = repo.path().canonicalize().unwrap();
    let mut state = issue_work_state(&work_dir);

    let error = state
        .start_issue_work_with_prompt(
            work_dir.clone(),
            issue_ref(9),
            "Unborn".into(),
            "test-model".into(),
            ReasoningEffort::High,
            threadlane_protocol::OrchestratorMode::Normal,
            |_, _| panic!("must not run"),
        )
        .unwrap_err();

    assert!(!error.is_empty());
    assert!(state.active_session_id.is_none());
    assert!(state.projects[0].sessions.is_empty());
    assert!(state.daemon_core.runtimes().is_empty());
    assert!(!work_dir.join(".threadlane/sessions").exists());
}

#[test]
fn issue_work_prompt_failure_rolls_back_artifacts_and_selection() {
    let repo = tempfile::tempdir().unwrap();
    let work_dir = init_test_repo(&repo);
    std::fs::write(work_dir.join("base.txt"), "base\n").unwrap();
    run_git(&work_dir, &["add", "."]);
    run_git(&work_dir, &["commit", "-m", "initial"]);

    let prior_session_file = work_dir.join(".threadlane/sessions/prior.jsonl");
    std::fs::create_dir_all(prior_session_file.parent().unwrap()).unwrap();
    CodingSessionHarness::append_fact_to_path(
        &prior_session_file,
        "main",
        "name",
        "Prior session",
        None,
    )
    .unwrap();
    let mut state = issue_work_state(&work_dir);
    state.projects[0].sessions = discover_sessions_in_project(&work_dir);
    state.select_session(work_dir.clone(), "prior".into());
    let active_work_dir = state.active_work_dir.clone();
    let active_session_id = state.active_session_id.clone();
    let is_new_task = state.is_new_task;
    let draft_work_mode = state.draft_work_mode;
    let workspace_page = state.workspace_page;
    let session_status = state.session_status.clone();
    let pending_hydrations = state.pending_hydrations.clone();
    let prior_model = state.selected_model.clone();
    let prior_effort = state.reasoning_effort;
    let persisted_before = threadlane_project::load_project_registry()
        .into_iter()
        .find(|project| project.path == work_dir)
        .map(|project| (project.last_session_id, project.last_opened_at));

    let error = state
        .start_issue_work_with_prompt(
            work_dir.clone(),
            issue_ref(77),
            "Prompt failure".into(),
            "test-model".into(),
            ReasoningEffort::High,
            threadlane_protocol::OrchestratorMode::Fusion,
            |_, prompt| {
                assert!(prompt.contains("create_draft_pull_request"));
                assert!(prompt.contains("publish the issue branch"));
                assert!(prompt.contains("credential-aware tool"));
                assert!(!prompt.contains("Do not push or publish anything"));
                Err("prompt acceptance failed".into())
            },
        )
        .unwrap_err();

    assert_eq!(error, "prompt acceptance failed");
    assert_eq!(state.selected_model, prior_model);
    assert_eq!(state.reasoning_effort, prior_effort);
    assert_eq!(
        state.orchestrator_mode,
        threadlane_protocol::OrchestratorMode::Normal
    );
    assert_eq!(
        threadlane_project::subagent_settings::load(&work_dir).orchestrator_mode,
        threadlane_protocol::OrchestratorMode::Normal
    );
    assert_eq!(state.active_work_dir, active_work_dir);
    assert_eq!(state.active_session_id, active_session_id);
    assert_eq!(state.is_new_task, is_new_task);
    assert_eq!(state.draft_work_mode, draft_work_mode);
    assert_eq!(state.workspace_page, workspace_page);
    assert_eq!(state.session_status, session_status);
    assert_eq!(state.pending_hydrations.len(), pending_hydrations.len());
    assert_eq!(
        state.pending_hydrations[0].session_id,
        pending_hydrations[0].session_id
    );
    assert_eq!(
        state.pending_hydrations[0].session_file,
        pending_hydrations[0].session_file
    );
    assert_eq!(state.projects[0].sessions.len(), 1);
    assert_eq!(state.projects[0].sessions[0].id, "prior");
    assert_eq!(
        discover_sessions_in_project(&work_dir)[0].id,
        state.projects[0].sessions[0].id
    );
    assert_eq!(
        std::fs::read_dir(work_dir.join(".threadlane/worktrees"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(work_dir.join(".threadlane/sessions"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "jsonl"))
            .count(),
        1
    );
    let persisted_after = threadlane_project::load_project_registry()
        .into_iter()
        .find(|project| project.path == work_dir)
        .map(|project| (project.last_session_id, project.last_opened_at));
    assert_eq!(persisted_after, persisted_before);
}

#[test]
fn startup_hydration_targets_existing_worktree_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().canonicalize().unwrap().join("project");
    let session_id = "session-worktree";
    let root_session_file = project
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    let worktree = project.join(".threadlane/worktrees").join(session_id);
    let worktree_session_file = worktree
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    std::fs::create_dir_all(root_session_file.parent().unwrap()).unwrap();
    std::fs::create_dir_all(worktree_session_file.parent().unwrap()).unwrap();

    let mut stub = JsonlStore::open(&root_session_file).unwrap();
    stub.append_fact("main", "is_worktree", "true", None)
        .unwrap();
    stub.append_fact(
        "main",
        "worktree_path",
        worktree.to_string_lossy().as_ref(),
        None,
    )
    .unwrap();
    drop(stub);
    std::fs::write(&worktree_session_file, "").unwrap();

    let mut state = AppState::load_from_registry(vec![AttachedProject {
        id: "project".into(),
        path: project.clone(),
        name: "Project".into(),
        last_selected_task_id: None,
        attached_at: 0,
        last_opened_at: 1,
        last_session_id: Some(session_id.into()),
    }]);

    assert_eq!(state.pending_hydrations.len(), 1);
    let request = state.pending_hydrations.pop().unwrap();
    assert_eq!(request.session_file, worktree_session_file);
    assert!(state.active_session_matches(&request.session_id, &request.session_file));
}

#[test]
fn worktree_session_discovery_keeps_stub_until_local_transcript_exists() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().canonicalize().unwrap().join("project");
    let root_session_file = project.join(".threadlane/sessions/session-worktree.jsonl");
    let worktree = project
        .join(".threadlane/worktrees")
        .join("session-worktree");
    std::fs::create_dir_all(root_session_file.parent().unwrap()).unwrap();
    let mut stub = JsonlStore::open(&root_session_file).unwrap();
    stub.append_fact("main", "is_worktree", "true", None)
        .unwrap();
    stub.append_fact(
        "main",
        "worktree_path",
        worktree.to_string_lossy().as_ref(),
        None,
    )
    .unwrap();
    drop(stub);

    let sessions = discover_sessions_in_project(&project);

    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].session_file,
        root_session_file.canonicalize().unwrap()
    );
    assert_eq!(sessions[0].work_dir, project.canonicalize().unwrap());
    assert_eq!(sessions[0].runtime_work_dir, worktree);
}

#[test]
fn cache_hit_rounding_uses_wide_intermediates_at_u64_max() {
    let metrics = SessionMetricsInfo {
        cache_read_tokens: u64::MAX,
        ..SessionMetricsInfo::default()
    };

    assert_eq!(metrics.billed_input_tokens(), u64::MAX);
    assert_eq!(metrics.cache_hit_percent(), Some(100));
}

#[tokio::test]
async fn reported_session_shape_keeps_total_processed_separate() {
    let (path, state) = reported_session_shape_state().await;

    let projected_context = state.active_context_window().unwrap();
    let projected_metrics = state.active_session_metrics();
    assert!(
        projected_metrics
            .billed_input_tokens()
            .saturating_add(projected_metrics.output_tokens)
            > projected_context.context_limit as u64
    );
    assert!(
        projected_context.current_tokens > 0
            && projected_context.current_tokens < projected_context.context_limit
    );
    assert_eq!(projected_context.context_limit, 128_000);
    assert_eq!(projected_context.effective_model, "gpt-4o");
    assert!(!projected_context.context_limit_is_estimate);

    // Inspect the production journal again, independently of the GPUI projection above.
    use threadlane_runtime::harness::{CompactionReason, TranscriptItem, read_transcript_page};

    let store = JsonlStore::open(&path).unwrap();
    let records = store.records();
    let provider_starts = records
        .iter()
        .filter_map(|record| match record {
            Record::ProviderRequestStarted { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(provider_starts.len(), 280);

    let adaptive_compactions = records
        .iter()
        .filter_map(|record| match record {
            Record::ContextCompacted {
                seq,
                generation,
                reason: CompactionReason::AdaptiveBudget,
                ..
            } => Some((*seq, *generation)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let adaptive_compaction_count = adaptive_compactions.len();
    assert!(adaptive_compaction_count >= 2);

    let mut checkpoint_sequences = HashSet::new();
    for (compaction_seq, generation) in adaptive_compactions {
        let (checkpoint_seq, summary) = store
            .entries()
            .iter()
            .filter_map(|entry| match &entry.message {
                AgentMessage::Custom {
                    custom_type,
                    payload,
                } if custom_type == "compaction_summary" && entry.seq < compaction_seq => payload
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(|summary| (entry.seq, summary)),
                _ => None,
            })
            .next_back()
            .expect("durable summary checkpoint before adaptive compaction");
        assert!(!summary.is_empty());
        assert!(
            checkpoint_sequences.insert(checkpoint_seq),
            "adaptive compactions must have distinct durable checkpoints"
        );

        let next_start_seq = provider_starts
            .iter()
            .copied()
            .find(|seq| *seq > compaction_seq)
            .expect("provider request after adaptive compaction");
        let (manifest_seq, manifest_generation) = records
            .iter()
            .filter_map(|record| match record {
                Record::ContextManifestCaptured {
                    seq,
                    compaction_generation,
                    ..
                } if *seq > next_start_seq => Some((*seq, *compaction_generation)),
                _ => None,
            })
            .next()
            .expect("manifest after post-compaction provider request start");
        assert_eq!(manifest_generation, generation);
        assert!(
            checkpoint_seq < compaction_seq
                && compaction_seq < next_start_seq
                && next_start_seq < manifest_seq,
            "checkpoint={checkpoint_seq}, compaction={compaction_seq}, provider_start={next_start_seq}, manifest={manifest_seq}"
        );
    }
    assert_eq!(checkpoint_sequences.len(), adaptive_compaction_count);

    let page = read_transcript_page(&path, None, 1_000).unwrap();
    assert!(!page.has_older);
    let transcript_messages = page
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Message(message) => Some(message),
            TranscriptItem::ContextCompacted(_) => None,
        })
        .collect::<Vec<_>>();
    let mut call_ids = Vec::new();
    let mut call_positions = HashMap::new();
    let mut result_ids = Vec::new();
    let mut result_positions = HashMap::new();
    for (position, message) in transcript_messages.iter().enumerate() {
        match message {
            AgentMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => {
                for call in calls {
                    assert!(
                        call_positions.insert(call.id.clone(), position).is_none(),
                        "duplicate tool call {}",
                        call.id
                    );
                    call_ids.push(call.id.clone());
                }
            }
            AgentMessage::Tool { tool_call_id, .. } => {
                assert!(
                    result_positions
                        .insert(tool_call_id.clone(), position)
                        .is_none(),
                    "duplicate tool result {tool_call_id}"
                );
                result_ids.push(tool_call_id.clone());
            }
            _ => {}
        }
    }
    let expected_loop_ids = (1..=279)
        .map(|index| format!("loop-{index}"))
        .collect::<Vec<_>>();
    assert_eq!(call_ids, expected_loop_ids);
    assert_eq!(result_ids, expected_loop_ids);
    assert_eq!(call_positions.len(), 279);
    assert_eq!(result_positions.len(), 279);
    for call_id in &expected_loop_ids {
        assert!(
            call_positions[call_id] < result_positions[call_id],
            "tool call {call_id} must precede its matching result"
        );
    }

    let reloaded = compute_session_messages(&path).unwrap();
    assert_eq!(
        reloaded
            .iter()
            .filter(|message| message.role == MessageRole::ContextMarker)
            .count(),
        adaptive_compaction_count
    );
    assert!(reloaded.iter().any(|message| {
        message.role == MessageRole::ContextMarker
            && message.content.starts_with("Context compacted · ")
            && message.content.contains(" → ")
    }));
    assert!(reloaded.iter().any(|message| {
        message.role == MessageRole::User && message.content == "continue the cached tool loop"
    }));
    assert!(reloaded.iter().any(|message| {
        message.role == MessageRole::Assistant && message.content == "complete"
    }));
    let projected_tool_ids = reloaded
        .iter()
        .flat_map(|message| &message.tool_activities)
        .map(|activity| activity.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        projected_tool_ids, expected_loop_ids,
        "projected tool activities must contain every loop exactly once and in order"
    );
    std::fs::remove_file(path).ok();
}

#[tokio::test]
async fn durable_projections_and_hydration_are_scoped_by_session_file() {
    use threadlane_runtime::harness::UsageCause;

    let root = std::env::temp_dir().join(format!(
        "threadlane-same-session-projects-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let project_a = root.join("a");
    let project_b = root.join("b");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    let file_a = project_a.join("same-session.jsonl");
    let file_b = project_b.join("same-session.jsonl");
    std::fs::rename(generated_reported_session_path().await, &file_a).unwrap();
    std::fs::rename(generated_reported_session_path().await, &file_b).unwrap();
    let mut store_b = JsonlStore::open(&file_b).unwrap();
    let next_generation = store_b
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::ContextCompacted { generation, .. } => Some(*generation),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let manifest_seq = store_b.next_sequence();
    store_b
        .append_record(Record::ContextManifestCaptured {
            id: "project-b-manifest".into(),
            seq: manifest_seq,
            lane: "main".into(),
            timestamp: 7,
            run_id: "run".into(),
            attempt: 1,
            request_id: TraceString::new("req").unwrap(),
            total_estimated_tokens: Some(222_222),
            effective_model: Some(TraceString::new("project-b-model").unwrap()),
            context_limit: Some(333_333),
            context_limit_is_estimate: false,
            compaction_generation: next_generation,
            items: Vec::new(),
        })
        .unwrap();
    store_b
        .append_record(Record::Usage {
            id: "project-b-usage".into(),
            seq: manifest_seq + 1,
            lane: "main".into(),
            timestamp: 8,
            run_id: None,
            cause: UsageCause::Provider,
            entry_id: None,
            tool_call_id: None,
            attempt: Some(1),
            usage: TokenUsage {
                input_tokens: 123,
                ..Default::default()
            },
        })
        .unwrap();
    drop(store_b);

    let projection_a = compute_full_session_projection(&file_a).unwrap();
    let projection_b = compute_full_session_projection(&file_b).unwrap();
    let expected_b_billed = projection_b.metrics.billed_input_tokens();
    let expected_b_processed = projection_b.token_efficiency.as_ref().unwrap().usage.processed_tokens();
    let mut state = AppState::load_from_registry(Vec::new());
    activate_test_session(&mut state, "same-session", &file_a);
    state.apply_session_hydration("same-session", &file_a, projection_a);
    activate_test_session(&mut state, "same-session", &file_b);
    state.apply_session_hydration("same-session", &file_b, projection_b);

    assert_eq!(state.context_windows.len(), 2);
    assert_eq!(state.session_metrics.len(), 2);
    assert_eq!(state.token_efficiency_by_session.len(), 2);
    assert_eq!(
        state.active_token_efficiency().unwrap().usage.processed_tokens(),
        expected_b_processed
    );
    assert_eq!(
        state.active_context_window().unwrap().current_tokens,
        222_222
    );
    assert_eq!(
        state.active_session_metrics().billed_input_tokens(),
        expected_b_billed
    );

    let stale = compute_full_session_projection(&file_a).unwrap();
    state.apply_session_hydration("same-session", &file_a, stale);
    assert_eq!(
        state.active_token_efficiency().unwrap().usage.processed_tokens(),
        expected_b_processed
    );
    assert_eq!(
        state.active_context_window().unwrap().current_tokens,
        222_222
    );
    assert_eq!(
        state.active_session_metrics().billed_input_tokens(),
        expected_b_billed
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn newer_provisional_compaction_clears_manifest_estimation() {
    use threadlane_runtime::harness::CompactionReason;

    let path = generated_reported_session_path().await;
    let mut store = JsonlStore::open(&path).unwrap();
    let newer_generation = store
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::ContextCompacted { generation, .. } => Some(*generation),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + 1;
    let request_seq = store.next_sequence();
    store
        .append_record(Record::ProviderRequestStarted {
            id: "newer-request".into(),
            seq: request_seq,
            lane: "main".into(),
            timestamp: 7,
            run_id: "run".into(),
            attempt: 2,
            provider: TraceString::new("openai").unwrap(),
            model: TraceString::new("newer-model").unwrap(),
            request_id: Some(TraceString::new("newer-request").unwrap()),
        })
        .unwrap();
    store
        .append_record(Record::ContextCompacted {
            id: "newer-compaction".into(),
            seq: request_seq + 1,
            lane: "main".into(),
            timestamp: 8,
            run_id: "run".into(),
            generation: newer_generation,
            reason: CompactionReason::AdaptiveBudget,
            effective_model: TraceString::new("newer-model").unwrap(),
            context_limit: 500_000,
            context_limit_is_estimate: true,
            pre_tokens: 400_000,
            post_tokens: 111_111,
            retained_tail_target: 0,
            retained_tail_tokens: 0,
            compacted_messages: 1,
        })
        .unwrap();
    drop(store);

    let context = compute_full_session_projection(&path)
        .unwrap()
        .context_window
        .unwrap();
    assert!(context.provisional);
    assert_eq!(context.compaction_generation, newer_generation);
    assert_eq!(context.current_tokens, 111_111);
    assert!(!context.estimating);
    std::fs::remove_file(path).ok();
}

#[tokio::test]
async fn model_switch_estimation_keeps_latest_manifest_model() {
    let path = generated_reported_session_path().await;
    let mut store = JsonlStore::open(&path).unwrap();
    let request_seq = store.next_sequence();
    store
        .append_record(Record::ProviderRequestStarted {
            id: "next-provider".into(),
            seq: request_seq,
            lane: "main".into(),
            timestamp: 7,
            run_id: "run".into(),
            attempt: 2,
            provider: TraceString::new("openai").unwrap(),
            model: TraceString::new("unused-new-model").unwrap(),
            request_id: Some(TraceString::new("next-request").unwrap()),
        })
        .unwrap();
    drop(store);
    let context = compute_full_session_projection(&path)
        .unwrap()
        .context_window
        .unwrap();
    assert!(context.estimating);
    assert_eq!(context.effective_model, "gpt-4o");
    assert_eq!(context.context_limit, 128_000);
    std::fs::remove_file(path).ok();
}

#[tokio::test]
async fn transcript_marker_survives_reload_without_summary_content() {
    let path = generated_reported_session_path().await;
    let first = compute_session_messages(&path).unwrap();
    let second = compute_session_messages(&path).unwrap();
    assert!(first.iter().any(|message| {
        message.role == MessageRole::ContextMarker
            && message.content.starts_with("Context compacted · ")
    }));
    assert_eq!(
        first.iter().map(|row| &row.id).collect::<Vec<_>>(),
        second.iter().map(|row| &row.id).collect::<Vec<_>>()
    );
    assert!(
        !first
            .iter()
            .any(|message| message.content.contains("Context checkpoint from"))
    );
    assert!(first.iter().any(|message| {
        message.role == MessageRole::User && message.content == "continue the cached tool loop"
    }));
    assert!(first.iter().any(|message| {
        message.role == MessageRole::Assistant && message.content == "complete"
    }));
    std::fs::remove_file(path).ok();
}

#[test]
fn legacy_session_without_compaction_has_no_fabricated_marker() {
    let path = std::env::temp_dir().join(format!(
        "threadlane-gpui-legacy-{}.jsonl",
        std::process::id()
    ));
    let mut store = JsonlStore::open(&path).unwrap();
    store
        .append_record(Record::ContextManifestCaptured {
            id: "manifest".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            run_id: "legacy".into(),
            attempt: 1,
            request_id: TraceString::new("request").unwrap(),
            total_estimated_tokens: Some(99),
            effective_model: None,
            context_limit: None,
            context_limit_is_estimate: false,
            compaction_generation: 0,
            items: Vec::new(),
        })
        .unwrap();
    drop(store);
    assert!(
        compute_session_messages(&path)
            .unwrap()
            .iter()
            .all(|message| message.role != MessageRole::ContextMarker)
    );
    assert_eq!(
        compute_full_session_projection(&path)
            .unwrap()
            .context_window
            .unwrap()
            .last_compaction_seq,
        None
    );
    std::fs::remove_file(path).ok();
}

fn cached_key(state: &AppState, session_id: &str) -> SessionProjectionKey {
    state
        .trajectory_by_session
        .keys()
        .chain(state.session_metrics.keys())
        .chain(state.session_token_usage.keys())
        .find(|key| key.session_id == session_id)
        .cloned()
        .expect("session projection must be cached")
}

#[test]
fn sidebar_project_filter_is_presentation_only_and_rejects_unattached_paths() {
    let mut state = AppState::load_from_registry(Vec::new());
    let project_work_dir = std::env::temp_dir().join("threadlane-filter-project");
    state.projects.push(ProjectInfo {
        name: "Filter project".into(),
        work_dir: project_work_dir.clone(),
        sessions: Vec::new(),
        is_expanded: true,
    });
    state.active_work_dir = Some(std::env::temp_dir().join("threadlane-active-project"));
    state.active_session_id = Some("active-session".into());
    let active_work_dir = state.active_work_dir.clone();
    let active_session_id = state.active_session_id.clone();

    state.set_sidebar_project_filter(Some(project_work_dir.clone()));

    assert_eq!(
        state.sidebar_project_filter.as_ref(),
        Some(&project_work_dir)
    );
    assert_eq!(state.active_work_dir, active_work_dir);
    assert_eq!(state.active_session_id, active_session_id);

    state.set_sidebar_project_filter(Some(
        std::env::temp_dir().join("threadlane-unattached-project"),
    ));
    assert_eq!(state.sidebar_project_filter, None);
}

#[test]
fn begin_new_task_returns_from_a_session_worktree_to_its_project_root() {
    let mut state = AppState::load_from_registry(Vec::new());
    let project_work_dir = std::env::temp_dir().join("threadlane-project-root");
    let worktree_work_dir = project_work_dir
        .join(".threadlane/worktrees")
        .join("session-worktree");
    let session_file = project_work_dir
        .join(".threadlane/sessions")
        .join("session-worktree.jsonl");
    state.projects.push(ProjectInfo {
        name: "Project".into(),
        work_dir: project_work_dir.clone(),
        sessions: vec![SessionInfo {
            id: "session-worktree".into(),
            title: "Worktree session".into(),
            work_dir: project_work_dir.clone(),
            runtime_work_dir: worktree_work_dir.clone(),
            session_file,
            updated_at: 0,
            health: SessionHealth::Working,
            git_branch: Some("worktree/session-worktree".into()),
            github_issue: None,
            is_worktree: true,
            worktree_available: true,
            completion_summary: SessionCompletionSummary::Unknown,
        }],
        is_expanded: true,
    });
    state.active_work_dir = Some(worktree_work_dir);
    state.active_session_id = Some("session-worktree".into());
    state.is_new_task = false;
    state.draft_work_mode = WorkMode::Worktree;

    state.begin_new_task();

    assert_eq!(state.active_work_dir.as_ref(), Some(&project_work_dir));
    assert_eq!(state.active_session_id, None);
    assert!(state.is_new_task);
    assert_eq!(state.draft_work_mode, WorkMode::Local);
}

fn apply_pending_hydration(state: &mut AppState) {
    let request = state.pending_hydrations.pop().unwrap();
    if request.reload_messages {
        let presented_completion =
            compute_latest_run_completion(&request.session_file).ok().flatten();
        let messages = compute_session_messages(&request.session_file).unwrap();
        state.apply_session_messages(
            &request.session_id,
            &request.session_file,
            messages,
            presented_completion,
        );
    }
    let projection = compute_full_session_projection(&request.session_file).unwrap();
    state.apply_session_hydration(&request.session_id, &request.session_file, projection);
}

#[test]
fn tool_activity_display_summary_is_prepared_during_projection() {
    assert_eq!(
        tool_activity_display_summary("read file · src/main.rs\nignored"),
        "read file · src/main.rs …"
    );
    assert_eq!(
        tool_activity_display_summary("still working...\nmore detail"),
        "still working..."
    );
    assert_eq!(tool_activity_display_summary(""), "");
}

#[test]
fn persisted_thinking_message_projects_as_reasoning_content() {
    let messages = project_agent_messages(vec![AgentMessage::Custom {
        custom_type: "thinking".into(),
        payload: serde_json::json!({"text": "Planning codebase inspection"}),
    }]);

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert!(messages[0].content.is_empty());
    assert_eq!(
        messages[0].reasoning_content.as_deref(),
        Some("Planning codebase inspection")
    );
    assert!(messages[0].tool_activities.is_empty());
}

#[test]
fn persisted_thinking_is_attached_to_the_following_assistant() {
    let messages = project_agent_messages(vec![
        AgentMessage::Custom {
            custom_type: "thinking".into(),
            payload: serde_json::json!({"text": "Planning"}),
        },
        AgentMessage::Assistant {
            content: Some("Answer".into()),
            tool_calls: None,
            stop_reason: Some("end_turn".into()),
            deferred_handle: None,
        },
    ]);

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "Answer");
    assert_eq!(messages[0].reasoning_content.as_deref(), Some("Planning"));
}

#[test]
fn startup_restores_the_most_recent_project_and_its_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let first_project = dir.path().join("first-project");
    let recent_project = dir.path().join("recent-project");
    std::fs::create_dir_all(&first_project).unwrap();
    let session_file = recent_project.join(".threadlane/sessions/recent-session.jsonl");
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    let mut store = threadlane_runtime::harness::JsonlStore::open(&session_file).unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "node_1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("recent prompt", Vec::new()),
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    drop(store);

    let state = AppState::load_from_registry(vec![
        AttachedProject {
            id: "first".into(),
            path: first_project,
            name: "first".into(),
            last_selected_task_id: None,
            attached_at: 1,
            last_opened_at: 1,
            last_session_id: None,
        },
        AttachedProject {
            id: "recent".into(),
            path: recent_project.clone(),
            name: "recent".into(),
            last_selected_task_id: None,
            attached_at: 2,
            last_opened_at: 10,
            last_session_id: Some("recent-session".into()),
        },
    ]);

    assert_eq!(
        state.active_work_dir.as_deref(),
        Some(recent_project.as_path())
    );
    assert_eq!(state.active_session_id.as_deref(), Some("recent-session"));
    assert!(state.daemon_core.runtimes().is_empty());
    assert_eq!(
        state
            .projects
            .iter()
            .find(|project| project.work_dir == recent_project)
            .unwrap()
            .sessions
            .len(),
        1
    );
}

#[test]
fn startup_seeds_session_rows_without_reducing_every_journal() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let sessions_dir = project.join(".threadlane/sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::write(sessions_dir.join("selected-session.jsonl"), "").unwrap();
    std::fs::write(sessions_dir.join("deferred-session.jsonl"), "not jsonl").unwrap();

    let mut state = AppState::load_from_registry(vec![AttachedProject {
        id: "project".into(),
        path: project.clone(),
        name: "project".into(),
        last_selected_task_id: None,
        attached_at: 1,
        last_opened_at: 1,
        last_session_id: Some("selected-session".into()),
    }]);

    assert_eq!(state.active_session_id.as_deref(), Some("selected-session"));
    assert_eq!(
        state.projects[0]
            .sessions
            .iter()
            .find(|session| session.id == "deferred-session")
            .unwrap()
            .title,
        "deferred-session"
    );

    let mut receiver = state.session_refresh_rx.take().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let (work_dir, sessions) = loop {
        if let Ok((generation, work_dir, sessions)) = receiver.try_recv() {
            assert_eq!(generation, 0);
            break (work_dir, sessions);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "startup session refresh was never queued"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(state.apply_session_refresh(work_dir, sessions, 0));
    let deferred = state.projects[0]
        .sessions
        .iter()
        .find(|session| session.id == "deferred-session")
        .unwrap();
    assert_eq!(deferred.title, "Unreadable session");
    assert_eq!(deferred.health, SessionHealth::Warning);
}

#[test]
fn app_state_startup_defers_messages_and_full_projection() {
    use threadlane_provider::openai::{ToolCall, ToolCallFunction};
    use threadlane_runtime::harness::{
        CapabilitySnapshot, OperationIntent, PromptSnapshot, ProviderOutcome, Record, SessionStore,
        TraceString, UsageCause,
    };

    let unique = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let work_dir = std::env::temp_dir().join(format!(
        "threadlane-gpui-session-hydration-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&work_dir).unwrap();
    let work_dir = std::fs::canonicalize(&work_dir).unwrap();
    let session_file = work_dir.join(".threadlane/sessions/hydration-test.jsonl");
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();

    let usage = TokenUsage {
        input_tokens: 17,
        output_tokens: 9,
        cache_read_tokens: 4,
        cache_write_tokens: 2,
        total_tokens: 32,
    };
    let mut store = threadlane_runtime::harness::JsonlStore::open(&session_file).unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "node_1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::User {
                content: "Inspect the project".into(),
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "node_2".into(),
            parent_id: Some("node_1".into()),
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::Custom {
                custom_type: "thinking".into(),
                payload: serde_json::json!({"text": "Reading the relevant files"}),
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "assistant-1".into(),
            parent_id: Some("node_2".into()),
            lane: "main".into(),
            seq: 3,
            timestamp: 3,
            message: AgentMessage::Assistant {
                content: Some("The issue is fixed.".into()),
                tool_calls: Some(vec![ToolCall {
                    id: "call-read".into(),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{"path":"src/main.rs"}"#.into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "node_4".into(),
            parent_id: Some("assistant-1".into()),
            lane: "main".into(),
            seq: 4,
            timestamp: 4,
            message: AgentMessage::Tool {
                tool_call_id: "call-read".into(),
                name: "read_file".into(),
                content: "file contents".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-1".into(),
            seq: 99,
            lane: "main".into(),
            timestamp: 99,
            wall_time_ms: None,
            source_leaf_id: None,
            intent: OperationIntent::Run,
        })
        .unwrap();
    store
        .append_record(Record::StepAttempt {
            id: "attempt-1".into(),
            seq: 100,
            lane: "main".into(),
            timestamp: 100,
            run_id: "run-1".into(),
            attempt: 1,
            result_entry_id: "assistant-1".into(),
            compaction_reason: None,
        })
        .unwrap();
    store
        .append_record(Record::Usage {
            id: "usage-1".into(),
            seq: 101,
            lane: "main".into(),
            timestamp: 101,
            run_id: Some("run-1".into()),
            cause: UsageCause::Provider,
            entry_id: Some("assistant-1".into()),
            tool_call_id: None,
            attempt: Some(1),
            usage: usage.clone(),
        })
        .unwrap();
    store
        .append_record(Record::RunContextCaptured {
            id: "context-1".into(),
            context_window_limit: None,
            route_defaults: None,
            seq: 102,
            lane: "main".into(),
            timestamp: 102,
            run_id: "run-1".into(),
            attempt: None,
            model: TraceString::new("test-model").unwrap(),
            provider: TraceString::new("openai").unwrap(),
            reasoning_effort: ReasoningEffort::Medium,
            prompt_cache_enabled: false,
            work_dir: TraceString::new(work_dir.to_string_lossy()).unwrap(),
            system_prompt: PromptSnapshot::Redacted {
                sha256: TraceString::new("prompt-sha").unwrap(),
                byte_len: 128,
                reason: TraceString::new("test-policy").unwrap(),
            },
            tool_schema_sha256: TraceString::new("tool-sha").unwrap(),
            enabled_tool_names: vec![TraceString::new("read_file").unwrap()],
            capabilities: CapabilitySnapshot {
                capabilities: vec![TraceString::new("read_file").unwrap()],
                fingerprint: Some(TraceString::new("capability-sha").unwrap()),
            },
            prompt_template_ids: Vec::new(),
            git_head: None,
        })
        .unwrap();
    store
        .append_record(Record::ProviderRequestStarted {
            id: "provider-start-1".into(),
            seq: 103,
            lane: "main".into(),
            timestamp: 103,
            run_id: "run-1".into(),
            attempt: 1,
            provider: TraceString::new("openai").unwrap(),
            model: TraceString::new("test-model").unwrap(),
            request_id: Some(TraceString::new("request-1").unwrap()),
        })
        .unwrap();
    store
        .append_record(Record::ProviderRequestFinished {
            id: "provider-finish-1".into(),
            seq: 104,
            lane: "main".into(),
            timestamp: 104,
            run_id: "run-1".into(),
            attempt: 1,
            request_id: Some(TraceString::new("request-1").unwrap()),
            outcome: ProviderOutcome::Completed,
            error: None,
            duration_ms: Some(25),
            usage: None,
        })
        .unwrap();
    drop(store);

    let mut state = AppState::load_from_registry(vec![AttachedProject {
        id: "hydration-test".into(),
        path: work_dir.clone(),
        name: "hydration-test".into(),
        last_selected_task_id: None,
        attached_at: 0,
        last_opened_at: 0,
        last_session_id: Some("hydration-test".into()),
    }]);

    assert_eq!(state.active_session_id.as_deref(), Some("hydration-test"));
    assert_eq!(state.active_work_dir.as_deref(), Some(work_dir.as_path()));
    assert!(!state.is_new_task);
    assert!(state.messages.is_empty());
    assert_eq!(state.pending_hydrations.len(), 1);
    assert!(state.pending_hydrations[0].reload_messages);

    apply_pending_hydration(&mut state);
    let messages = &state.messages;
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[1].reasoning_content.as_deref(),
        Some("Reading the relevant files")
    );
    assert_eq!(messages[1].content, "The issue is fixed.");
    assert_eq!(messages[1].tool_activities.len(), 1);
    assert_eq!(messages[1].tool_activities[0].id, "call-read");
    assert_eq!(messages[1].tool_activities[0].detail, "file contents");
    assert!(
        state.trajectory_by_session[&cached_key(&state, "hydration-test")]
            .iter()
            .any(|entry| entry.summary == "User input")
    );
    assert!(
        state.trajectory_by_session[&cached_key(&state, "hydration-test")]
            .iter()
            .any(|entry| entry.summary == "read_file finished")
    );
    let trace = &state.trajectory_by_session[&cached_key(&state, "hydration-test")];
    let context_index = trace
        .iter()
        .position(|entry| entry.category == "Context")
        .unwrap();
    let provider_start_index = trace
        .iter()
        .position(|entry| entry.summary == "openai request started")
        .unwrap();
    let provider_finish_index = trace
        .iter()
        .position(|entry| entry.summary == "Provider request Completed")
        .unwrap();
    assert!(context_index < provider_start_index && provider_start_index < provider_finish_index);
    assert_eq!(
        state.session_metrics[&cached_key(&state, "hydration-test")].turns,
        1
    );
    assert_eq!(
        state.session_metrics[&cached_key(&state, "hydration-test")].tool_calls,
        1
    );
    let metrics = &state.session_metrics[&cached_key(&state, "hydration-test")];
    assert_eq!(metrics.input_tokens, 17);
    assert_eq!(metrics.output_tokens, 9);
    assert_eq!(metrics.cache_read_tokens, 4);
    assert_eq!(metrics.cache_write_tokens, 2);
    assert_eq!(metrics.billed_input_tokens(), 23);
    assert_eq!(metrics.cache_hit_percent(), Some(17));
    assert_eq!(state.current_session_token_usage(), usage);

    drop(state);
    let _ = std::fs::remove_dir_all(work_dir);
}

#[test]
fn durable_projection_restores_ordered_tool_lifecycle_and_exact_usage() {
    use threadlane_provider::openai::{ToolCall, ToolCallFunction};
    use threadlane_runtime::harness::{
        Entry, OperationIntent, OperationOutcome, Record, SessionStore, ToolReplaySafety,
        UsageCause,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, "").unwrap();
    let mut store = threadlane_runtime::harness::JsonlStore::open(&path).unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-1".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            wall_time_ms: None,
            source_leaf_id: None,
            intent: OperationIntent::Run,
        })
        .unwrap();
    store
        .append_entry(Entry {
            id: "user-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::user("inspect", vec![]),
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::StepAttempt {
            id: "attempt-1".into(),
            seq: 3,
            lane: "main".into(),
            timestamp: 3,
            run_id: "run-1".into(),
            attempt: 1,
            result_entry_id: "assistant-1".into(),
            compaction_reason: None,
        })
        .unwrap();
    store
        .append_entry(Entry {
            id: "assistant-1".into(),
            parent_id: Some("user-1".into()),
            lane: "main".into(),
            seq: 4,
            timestamp: 4,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{"path":"src/lib.rs"}"#.into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::ToolStarted {
            id: "tool-start-1".into(),
            seq: 5,
            lane: "main".into(),
            timestamp: 5,
            run_id: "run-1".into(),
            assistant_entry_id: "assistant-1".into(),
            tool_index: 0,
            tool_call_id: "call-1".into(),
            tool_name: "read_file".into(),
            effective_args: serde_json::json!({"path": "src/lib.rs"}),
            result_entry_id: "tool-result-1".into(),
            replay: ToolReplaySafety::Safe,
        })
        .unwrap();
    store
        .append_entry(Entry {
            id: "tool-result-1".into(),
            parent_id: Some("assistant-1".into()),
            lane: "main".into(),
            seq: 6,
            timestamp: 6,
            message: AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "read_file".into(),
                content: "result".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::ToolFinished {
            id: "tool-finish-1".into(),
            seq: 7,
            lane: "main".into(),
            timestamp: 7,
            run_id: "run-1".into(),
            tool_call_id: "call-1".into(),
            result_entry_id: "tool-result-1".into(),
            terminate: false,
        })
        .unwrap();
    let usage = TokenUsage {
        input_tokens: 11,
        output_tokens: 7,
        cache_read_tokens: 5,
        cache_write_tokens: 3,
        total_tokens: 26,
    };
    store
        .append_record(Record::Usage {
            id: "usage-1".into(),
            seq: 8,
            lane: "main".into(),
            timestamp: 8,
            run_id: Some("run-1".into()),
            cause: UsageCause::Provider,
            entry_id: Some("assistant-1".into()),
            tool_call_id: None,
            attempt: Some(1),
            usage: usage.clone(),
        })
        .unwrap();
    store
        .append_record(Record::OperationFinished {
            id: "finish-1".into(),
            seq: 9,
            lane: "main".into(),
            timestamp: 9,
            wall_time_ms: None,
            run_id: "run-1".into(),
            outcome: OperationOutcome::Completed,
            error: None,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-2".into(),
            seq: 10,
            lane: "main".into(),
            timestamp: 10,
            wall_time_ms: None,
            source_leaf_id: Some("tool-result-1".into()),
            intent: OperationIntent::Run,
        })
        .unwrap();
    store
        .append_record(Record::StepAttempt {
            id: "attempt-2".into(),
            seq: 11,
            lane: "main".into(),
            timestamp: 11,
            run_id: "run-2".into(),
            attempt: 1,
            result_entry_id: "assistant-2".into(),
            compaction_reason: None,
        })
        .unwrap();
    store
        .append_entry(Entry {
            id: "assistant-2".into(),
            parent_id: Some("tool-result-1".into()),
            lane: "main".into(),
            seq: 12,
            timestamp: 12,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: "write_file".into(),
                        arguments: r#"{"path":"src/new.rs"}"#.into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::ToolStarted {
            id: "tool-start-2".into(),
            seq: 13,
            lane: "main".into(),
            timestamp: 13,
            run_id: "run-2".into(),
            assistant_entry_id: "assistant-2".into(),
            tool_index: 0,
            tool_call_id: "call-1".into(),
            tool_name: "write_file".into(),
            effective_args: serde_json::json!({"path": "src/new.rs"}),
            result_entry_id: "tool-result-2".into(),
            replay: ToolReplaySafety::Never,
        })
        .unwrap();
    store
        .append_entry(Entry {
            id: "tool-result-2".into(),
            parent_id: Some("assistant-2".into()),
            lane: "main".into(),
            seq: 14,
            timestamp: 14,
            message: AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "write_file".into(),
                content: "written".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::ToolFinished {
            id: "tool-finish-2".into(),
            seq: 15,
            lane: "main".into(),
            timestamp: 15,
            run_id: "run-2".into(),
            tool_call_id: "call-1".into(),
            result_entry_id: "tool-result-2".into(),
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::OperationFinished {
            id: "finish-2".into(),
            seq: 16,
            lane: "main".into(),
            timestamp: 16,
            wall_time_ms: None,
            run_id: "run-2".into(),
            outcome: OperationOutcome::Completed,
            error: None,
        })
        .unwrap();
    drop(store);

    let mut state = AppState::load_from_registry(Vec::new());
    state.hydrate_session_projection("session", &path).unwrap();

    assert_eq!(
        state.session_token_usage[&cached_key(&state, "session")],
        usage
    );
    let diagnostics = &state.diagnostics_by_session[&cached_key(&state, "session")];
    assert!(!diagnostics.model_context.is_empty());
    assert_eq!(
        diagnostics
            .durable_events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        {
            let mut seqs = diagnostics
                .durable_events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>();
            seqs.sort_unstable();
            seqs
        }
    );
    assert_eq!(diagnostics.recovery.len(), 1);
    let tool_rows = state.trajectory_by_session[&cached_key(&state, "session")]
        .iter()
        .filter(|entry| entry.correlation_id.as_deref() == Some("call-1"))
        .collect::<Vec<_>>();
    assert_eq!(tool_rows.len(), 4);
    assert_eq!(tool_rows[0].seq, Some(4));
    assert_eq!(tool_rows[0].run_id.as_deref(), Some("run-1"));
    assert_eq!(tool_rows[0].summary, "read_file running");
    assert_eq!(tool_rows[1].seq, Some(6));
    assert_eq!(tool_rows[1].run_id.as_deref(), Some("run-1"));
    assert_eq!(tool_rows[1].summary, "read_file finished");
    assert_eq!(tool_rows[2].seq, Some(12));
    assert_eq!(tool_rows[2].run_id.as_deref(), Some("run-2"));
    assert_eq!(tool_rows[2].summary, "write_file running");
    assert_eq!(tool_rows[3].seq, Some(14));
    assert_eq!(tool_rows[3].run_id.as_deref(), Some("run-2"));
    assert_eq!(tool_rows[3].summary, "write_file finished");
}

#[test]
fn durable_subagent_projection_ignores_unrelated_named_lanes() {
    use threadlane_runtime::harness::{Entry, SurfaceOperation};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut store = JsonlStore::open(&path).unwrap();
    store
        .append_entry(Entry {
            id: "background-entry".into(),
            parent_id: None,
            lane: "background-task".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::Assistant {
                content: Some("not a subagent".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            surface_op: SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    drop(store);

    assert!(
        compute_full_session_projection(&path)
            .unwrap()
            .subagents
            .is_empty()
    );
}

#[test]
fn live_subagent_updates_are_isolated_from_main_transcript() {
    let mut state = AppState::load_from_registry(Vec::new());
    let dir = tempfile::tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    state.active_work_dir = Some(work_dir.clone());
    state.active_session_id = Some("session".into());
    state.subagents_by_session.insert(
        AppState::projection_key("session", &state.session_file(&work_dir, "session")),
        Vec::new(),
    );
    state.record_subagent_activity(&AgentEvent::SubagentQueued {
        run_id: 1,
        task_index: 0,
        agent: "scout".into(),
        task: "inspect".into(),
    });
    state.record_subagent_activity(&AgentEvent::SubagentStarted {
        run_id: 1,
        task_index: 0,
        journal_run_id: "child-run".into(),
        lane: "child-lane".into(),
        agent: "scout".into(),
        task: "inspect".into(),
        model: "gpt-5.6-luna".into(),
        isolation: None,
    });
    state.record_subagent_activity(&AgentEvent::SubagentUpdate {
        run_id: 1,
        task_index: 0,
        journal_run_id: "child-run".into(),
        lane: "child-lane".into(),
        update: SubagentProgressUpdate::TextDelta {
            delta: "live progress".into(),
        },
    });

    assert!(state.messages.is_empty());
    let subagent = state.active_subagents().first().unwrap();
    assert_eq!(subagent.status, SubagentActivityStatus::Running);
    assert_eq!(subagent.agent, "scout");
    assert_eq!(subagent.task, "inspect");
    assert_eq!(subagent.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(subagent.messages[0].content, "live progress");
}

#[test]
fn live_subagent_events_contribute_to_composer_metrics() {
    let mut state = AppState::load_from_registry(Vec::new());
    let dir = tempfile::tempdir().unwrap();
    state.active_work_dir = Some(dir.path().to_path_buf());
    state.active_session_id = Some("session".into());

    state.drain_chat_stream(vec![
        SessionEvent::Agent {
            session_id: "session".into(),
            event: AgentEvent::SubagentStarted {
                run_id: 1,
                task_index: 0,
                journal_run_id: "child-run".into(),
                lane: "child-lane".into(),
                agent: "scout".into(),
                task: "inspect".into(),
                model: "gpt-5.6-luna".into(),
                isolation: None,
            },
        },
        SessionEvent::Agent {
            session_id: "session".into(),
            event: AgentEvent::SubagentUpdate {
                run_id: 1,
                task_index: 0,
                journal_run_id: "child-run".into(),
                lane: "child-lane".into(),
                update: SubagentProgressUpdate::ToolStarted {
                    tool_call_id: "call-1".into(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            },
        },
        SessionEvent::Agent {
            session_id: "session".into(),
            event: AgentEvent::SubagentUpdate {
                run_id: 1,
                task_index: 0,
                journal_run_id: "child-run".into(),
                lane: "child-lane".into(),
                update: SubagentProgressUpdate::Usage {
                    usage: TokenUsage {
                        input_tokens: 100,
                        output_tokens: 25,
                        total_tokens: 125,
                        ..Default::default()
                    },
                },
            },
        },
    ]);

    let metrics = state.active_session_metrics();
    assert_eq!(metrics.turns, 1);
    assert_eq!(metrics.tool_calls, 1);
    assert_eq!(metrics.input_tokens, 100);
    assert_eq!(metrics.output_tokens, 25);
}

#[test]
fn session_switch_preserves_live_trajectory_and_applies_deferred_events() {
    let dir = tempfile::tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let session_id = "live-session".to_string();
    let session_file = work_dir
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    std::fs::write(&session_file, "").unwrap();
    let mut store = threadlane_runtime::harness::JsonlStore::open(&session_file).unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "call-1-assistant".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: r#"{"path":"src/lib.rs"}"#.into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "call-1-tool".into(),
            parent_id: Some("call-1-assistant".into()),
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "read_file".into(),
                content: "file contents".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();

    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: work_dir.clone(),
        sessions: vec![SessionInfo {
            id: session_id.clone(),
            title: "Live".into(),
            work_dir: work_dir.clone(),
            runtime_work_dir: work_dir.clone(),
            session_file: session_file.clone(),
            updated_at: 0,
            health: SessionHealth::Working,
            git_branch: None,
            github_issue: None,
            is_worktree: false,
            worktree_available: true,
            completion_summary: SessionCompletionSummary::Unknown,
        }],
        is_expanded: true,
    });
    state.trajectory_by_session.insert(
        AppState::projection_key(&session_id, &session_file),
        vec![
            TrajectoryEntry {
                seq: None,
                run_id: None,
                turn: None,
                request: None,
                category: "Tool".into(),
                summary: "read_file running".into(),
                detail: r#"{"path":"src/lib.rs"}"#.into(),
                lane: Some("main".into()),
                correlation_id: Some("call-1".into()),
                diagnostics: TrajectoryDiagnostics::default(),
            },
            TrajectoryEntry {
                seq: None,
                run_id: None,
                turn: None,
                request: None,
                category: "Tool".into(),
                summary: "read_file finished".into(),
                detail: "file contents".into(),
                lane: Some("main".into()),
                correlation_id: Some("call-1".into()),
                diagnostics: TrajectoryDiagnostics::default(),
            },
        ],
    );
    state.deferred_stream_events.insert(
        session_id.clone(),
        vec![
            SessionEvent::Agent {
                session_id: session_id.clone(),
                event: AgentEvent::TurnStart { turn_number: 2 },
            },
            SessionEvent::Finished {
                session_id: session_id.clone(),
                session_file,
            },
        ],
    );

    state.select_session(work_dir, session_id.clone());

    let trajectory = &state.trajectory_by_session[&cached_key(&state, &session_id)];
    assert_eq!(trajectory.len(), 2);
    assert_eq!(trajectory[0].summary, "read_file running");
    assert_eq!(trajectory[1].summary, "read_file finished");
    assert!(trajectory.iter().all(|entry| {
        entry.category == "Tool" && entry.correlation_id.as_deref() == Some("call-1")
    }));
}

#[test]
fn selecting_attention_session_replays_deferred_events_once() {
    let dir = tempfile::tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let session_file = work_dir.join(".threadlane/sessions/background.jsonl");
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    std::fs::write(&session_file, "").unwrap();
    let session = test_session("background", &session_file);
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: work_dir.clone(),
        sessions: vec![session.clone()],
        is_expanded: true,
    });
    state.active_work_dir = Some(work_dir.clone());
    state.active_session_id = Some("foreground".into());

    assert!(state.drain_chat_stream(vec![
        SessionEvent::Agent {
            session_id: session.id.clone(),
            event: AgentEvent::AgentError {
                error: "background failed".into(),
            },
        },
        SessionEvent::Finished {
            session_id: session.id.clone(),
            session_file: session_file.clone(),
        },
    ]));

    state.select_session(work_dir, session.id.clone());

    assert!(!state.deferred_stream_events.contains_key(&session.id));
    assert_eq!(
        state
            .active_trajectory()
            .iter()
            .filter(|entry| entry.summary == "Agent error")
            .count(),
        1
    );
    assert_eq!(
        state
            .messages
            .iter()
            .filter(|message| message.content == "background failed")
            .count(),
        1
    );
    assert_eq!(
        state
            .pending_hydrations
            .iter()
            .filter(|request| {
                request.session_id == session.id && request.session_file == session_file
            })
            .count(),
        1
    );

    assert!(!state.drain_chat_stream(Vec::new()));
    assert_eq!(
        state
            .active_trajectory()
            .iter()
            .filter(|entry| entry.summary == "Agent error")
            .count(),
        1
    );
}

#[test]
fn trajectory_epoch_changes_only_when_entries_are_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, "").unwrap();
    let mut state = AppState::load_from_registry(Vec::new());
    activate_test_session(&mut state, "session", &path);

    let projection = compute_full_session_projection(&path).unwrap();
    state.apply_session_hydration("session", &path, projection);
    assert_eq!(state.trajectory_epoch(), 1);

    state.record_trajectory(
        "session",
        &AgentEvent::ToolExecutionStart {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"src/lib.rs"}"#.into(),
        },
    );
    assert_eq!(state.trajectory_epoch(), 1);
    assert_eq!(state.active_trajectory().len(), 1);
}

#[test]
fn durable_trajectory_hydrates_after_session_switch() {
    use threadlane_runtime::harness::SessionStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, "").unwrap();
    let store = threadlane_runtime::harness::JsonlStore::open(&path).unwrap();
    let mut harness = threadlane_runtime::harness::AgentHarness::new(store);
    harness
        .accept_prompt("run-1", AgentMessage::user("old prompt", vec![]))
        .unwrap();
    harness.drive_to_completion().unwrap();
    let parent_id = harness.store().entries().last().unwrap().id.clone();
    let seq = harness.store().next_sequence();
    harness
        .store_mut()
        .append_entry(threadlane_runtime::harness::Entry {
            id: "legacy-tool-result".into(),
            parent_id: Some(parent_id),
            lane: "main".into(),
            seq,
            timestamp: seq,
            message: AgentMessage::Tool {
                tool_call_id: "legacy-call".into(),
                name: "read_file".into(),
                content: "legacy output".into(),
                is_error: false,
                terminate: false,
                images: Vec::new(),
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    drop(harness);

    let mut state = AppState::load_from_registry(Vec::new());
    state
        .hydrate_session_projection("old-session", &path)
        .unwrap();

    let trajectory = &state.trajectory_by_session[&cached_key(&state, "old-session")];
    assert!(trajectory.iter().any(|entry| entry.category == "Operation"));
    assert!(
        trajectory
            .iter()
            .any(|entry| { entry.category == "Input" && entry.detail == "old prompt" })
    );
    assert!(trajectory.iter().any(|entry| entry.category == "Step"));
    assert!(trajectory.iter().any(|entry| {
        entry.category == "Tool"
            && entry.correlation_id.as_deref() == Some("legacy-call")
            && entry.detail == "legacy output"
    }));
}

#[test]
fn trajectory_projection_is_session_scoped_and_preserves_tool_details() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.active_work_dir = Some(std::env::temp_dir().join("threadlane-trajectory-scope"));
    state.active_session_id = Some("session-a".into());
    state.record_trajectory(
        "session-a",
        &AgentEvent::ToolExecutionStart {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"src/lib.rs"}"#.into(),
        },
    );
    state.active_session_id = Some("session-b".into());
    state.record_trajectory(
        "session-b",
        &AgentEvent::SubagentQueued {
            run_id: 7,
            task_index: 2,
            agent: "reviewer".into(),
            task: "Review the patch".into(),
        },
    );
    assert_eq!(
        state.trajectory_by_session[&cached_key(&state, "session-a")].len(),
        1
    );
    assert_eq!(
        state.trajectory_by_session[&cached_key(&state, "session-a")][0].category,
        "Tool"
    );
    assert!(
        state.trajectory_by_session[&cached_key(&state, "session-a")][0]
            .detail
            .contains("src/lib.rs")
    );
    assert_eq!(
        state.trajectory_by_session[&cached_key(&state, "session-a")][0]
            .correlation_id
            .as_deref(),
        Some("call-1")
    );
    assert_eq!(
        state.trajectory_by_session[&cached_key(&state, "session-b")][0]
            .lane
            .as_deref(),
        Some("reviewer")
    );
    state.active_session_id = Some("session-a".into());
    state.record_trajectory("session-a", &AgentEvent::TurnStart { turn_number: 12 });
    assert_eq!(
        state.trajectory_by_session[&cached_key(&state, "session-a")].len(),
        1
    );
}

#[test]
fn inactive_session_stream_events_replay_after_switching_back() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.messages_mut().clear();
    state.active_work_dir = Some(std::env::temp_dir().join("threadlane-stream-replay"));
    state.active_session_id = Some("foreground-session".into());
    state.is_new_task = false;

    for event in [
        AgentEvent::MessageUpdate {
            text_delta: None,
            reasoning_delta: Some("reasoning while away".into()),
            tool_call_name: None,
        },
        AgentEvent::MessageUpdate {
            text_delta: Some("generated while away".into()),
            reasoning_delta: None,
            tool_call_name: None,
        },
        AgentEvent::ToolExecutionStart {
            tool_call_id: "call-away".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"src/main.rs"}"#.into(),
        },
        AgentEvent::ToolExecutionUpdate {
            tool_call_id: "call-away".into(),
            partial_result: "tool output while away".into(),
        },
    ] {
        state
            .stream_tx
            .send(SessionEvent::Agent {
                session_id: "background-session".into(),
                event,
            })
            .unwrap();
    }

    let events = take_stream_events(&mut state, 128);
    assert!(!state.drain_chat_stream(events));
    assert!(state.messages.is_empty());
    assert_eq!(state.deferred_stream_events.len(), 1);

    state.active_session_id = Some("background-session".into());
    assert!(state.drain_chat_stream(Vec::new()));
    assert_eq!(state.messages.len(), 2);
    assert_eq!(state.messages[0].content, "generated while away");
    assert_eq!(
        state.messages[0].reasoning_content.as_deref(),
        Some("reasoning while away")
    );
    assert!(state.messages[0].streaming);
    assert_eq!(state.messages[1].tool_activities.len(), 1);
    assert_eq!(state.messages[1].tool_activities[0].id, "call-away");
    assert_eq!(
        state.messages[1].tool_activities[0].detail,
        "tool output while away"
    );
    assert!(state.deferred_stream_events.is_empty());
}
#[tokio::test]
async fn stream_drain_preserves_events_beyond_one_frame_budget() {
    use threadlane_daemon::next_event_batch_capped;

    let mut state = AppState::for_tests();
    state.messages_mut().clear();
    state.active_work_dir = Some(std::env::temp_dir().join("threadlane-stream-budget"));
    state.active_session_id = Some("session".into());
    state.is_new_task = false;

    // Startup services share state.stream_rx. Use a controlled stream so the
    // frame budget is spent only on the fixture's ordered agent updates.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for index in 0..130 {
        tx.send(SessionEvent::Agent {
            session_id: "session".into(),
            event: AgentEvent::MessageUpdate {
                text_delta: Some(format!("{index},")),
                reasoning_delta: None,
                tool_call_name: None,
            },
        })
        .unwrap();
    }
    drop(tx);

    let events = next_event_batch_capped(&mut rx, 128).await.unwrap();
    assert_eq!(events.len(), 128);
    assert!(state.drain_chat_stream(events));
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0].content.matches(',').count(), 128);
    let events = next_event_batch_capped(&mut rx, 128).await.unwrap();
    assert_eq!(events.len(), 2);
    assert!(state.drain_chat_stream(events));
    assert_eq!(state.messages[0].content.matches(',').count(), 130);
    assert_eq!(
        state.messages[0].content,
        (0..130).map(|index| format!("{index},")).collect::<String>()
    );
    assert!(next_event_batch_capped(&mut rx, 128).await.is_none());
}

#[test]
fn session_messages_include_complete_durable_history_beyond_legacy_page() {
    const LEGACY_PAGE_SIZE: usize = 40;
    const MESSAGE_COUNT: usize = 45;
    let unique = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "threadlane-gpui-complete-history-{}-{unique}",
        std::process::id()
    ));
    let path = root.join("session.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut store = threadlane_runtime::harness::JsonlStore::open(&path).unwrap();
    let mut parent_id = None;
    for index in 0..MESSAGE_COUNT {
        let id = format!("node_{index}");
        store
            .append_entry(threadlane_runtime::harness::Entry {
                id: id.clone(),
                parent_id,
                lane: "main".into(),
                seq: (index + 1) as u64,
                timestamp: (index + 1) as u64,
                message: AgentMessage::User {
                    content: format!("message-{index}"),
                },
                surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        parent_id = Some(id);
    }
    drop(store);

    // Keep the fixture tied to the regression: the former GPUI helper loaded
    // only this newest page, omitting the first five durable messages.
    let legacy_page =
        threadlane_runtime::harness::read_transcript_page(&path, None, LEGACY_PAGE_SIZE).unwrap();
    assert_eq!(legacy_page.items.len(), LEGACY_PAGE_SIZE);
    assert!(legacy_page.has_older);

    let messages = load_session_messages(&path);
    assert_eq!(messages.len(), MESSAGE_COUNT);
    assert_eq!(messages.first().unwrap().content, "message-0");
    assert_eq!(messages.last().unwrap().content, "message-44");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn startup_hydration_from_project_registry_populates_all_views() {
    let unique = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let project_root = std::env::temp_dir().join(format!(
        "threadlane-gpui-startup-test-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&project_root).unwrap();
    let project_root = std::fs::canonicalize(&project_root).unwrap();
    let sessions_dir = project_root.join(".threadlane").join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let session_file = sessions_dir.join("session_1001.jsonl");
    let mut store = threadlane_runtime::harness::JsonlStore::open(&session_file).unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "node_1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::User {
                content: "Hello on startup".into(),
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "node_2".into(),
            parent_id: Some("node_1".into()),
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::Assistant {
                content: Some("I am ready".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-start-1".into(),
            seq: 10,
            lane: "main".into(),
            timestamp: 10,
            wall_time_ms: None,
            source_leaf_id: None,
            intent: OperationIntent::Run,
        })
        .unwrap();
    store
        .append_record(Record::ProviderRequestFinished {
            id: "finish-1".into(),
            seq: 11,
            lane: "main".into(),
            timestamp: 11,
            run_id: "run-start-1".into(),
            attempt: 1,
            request_id: Some(TraceString::new("req-1").unwrap()),
            outcome: ProviderOutcome::Completed,
            error: None,
            duration_ms: Some(100),
            usage: Some(TokenUsage {
                total_tokens: 50,
                input_tokens: 20,
                output_tokens: 12,
                cache_read_tokens: 15,
                cache_write_tokens: 3,
            }),
        })
        .unwrap();
    drop(store);

    let mut attached_project = AttachedProject::from_path(project_root.clone());
    attached_project.last_opened_at = 1_000_000;

    let mut state = AppState::load_from_registry(vec![attached_project]);

    assert_eq!(state.active_session_id.as_deref(), Some("session_1001"));
    assert!(state.messages.is_empty());

    apply_pending_hydration(&mut state);

    assert_eq!(state.messages.len(), 2);
    assert_eq!(state.messages[0].content, "Hello on startup");
    assert_eq!(state.messages[1].content, "I am ready");

    let trajectory = state
        .trajectory_by_session
        .get(&cached_key(&state, "session_1001"))
        .expect("trajectory must be hydrated on startup");
    assert!(!trajectory.is_empty());
    assert!(trajectory.iter().any(|t| t.category == "Operation"));
    assert!(trajectory.iter().any(|t| t.category == "Provider"));

    let usage = state
        .session_token_usage
        .get(&cached_key(&state, "session_1001"))
        .expect("token usage must be hydrated on startup");
    assert_eq!(usage.total_tokens, 50);

    let metrics = state
        .session_metrics
        .get(&cached_key(&state, "session_1001"))
        .expect("session metrics must be hydrated on startup");
    assert_eq!(metrics.input_tokens, 20);
    assert_eq!(metrics.output_tokens, 12);
    assert_eq!(metrics.cache_read_tokens, 15);
    assert_eq!(metrics.cache_write_tokens, 3);
    assert_eq!(metrics.billed_input_tokens(), 38);
    assert_eq!(metrics.cache_hit_percent(), Some(39));

    let _ = std::fs::remove_dir_all(project_root);
}

#[test]
fn branch_consistency_trajectory_is_session_wide_audit_log_while_chat_is_active_branch() {
    let unique = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "threadlane-gpui-branch-test-{}-{unique}",
        std::process::id()
    ));
    let path = root.join("session.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    let mut store = threadlane_runtime::harness::JsonlStore::open(&path).unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "msg-root".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::User {
                content: "Root question".into(),
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "msg-branch-a".into(),
            parent_id: Some("msg-root".into()),
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::Assistant {
                content: Some("Branch A answer".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_runtime::harness::Entry {
            id: "msg-branch-b".into(),
            parent_id: Some("msg-root".into()),
            lane: "main".into(),
            seq: 3,
            timestamp: 3,
            message: AgentMessage::Assistant {
                content: Some("Branch B alternative answer".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            surface_op: threadlane_runtime::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-branch-a".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            wall_time_ms: None,
            source_leaf_id: None,
            intent: OperationIntent::Run,
        })
        .unwrap();
    store
        .append_record(Record::OperationFinished {
            id: "finish-branch-a".into(),
            seq: 2,
            lane: "main".into(),
            timestamp: 2,
            wall_time_ms: None,
            run_id: "run-branch-a".into(),
            outcome: OperationOutcome::Completed,
            error: None,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-branch-b".into(),
            seq: 3,
            lane: "main".into(),
            timestamp: 3,
            wall_time_ms: None,
            source_leaf_id: None,
            intent: OperationIntent::Run,
        })
        .unwrap();
    store
        .append_record(Record::OperationFinished {
            id: "finish-branch-b".into(),
            seq: 4,
            lane: "main".into(),
            timestamp: 4,
            wall_time_ms: None,
            run_id: "run-branch-b".into(),
            outcome: OperationOutcome::Completed,
            error: None,
        })
        .unwrap();

    let mut state = AppState::load_from_registry(Vec::new());
    state
        .hydrate_session_projection("branch-session", &path)
        .unwrap();

    let branch_messages = store.active_branch_messages("main");
    assert_eq!(branch_messages.len(), 2);
    assert!(matches!(
        &branch_messages[0],
        AgentMessage::User { content } if content == "Root question"
    ));
    assert!(matches!(
        &branch_messages[1],
        AgentMessage::Assistant { content, .. } if content.as_deref() == Some("Branch B alternative answer")
    ));

    let trajectory = state
        .trajectory_by_session
        .get(&cached_key(&state, "branch-session"))
        .unwrap();
    assert!(
        trajectory
            .iter()
            .any(|t| t.run_id.as_deref() == Some("run-branch-a"))
    );
    assert!(
        trajectory
            .iter()
            .any(|t| t.run_id.as_deref() == Some("run-branch-b"))
    );

    let _ = std::fs::remove_dir_all(root);
}

fn live_tool_entry(correlation: &str, summary: &str) -> TrajectoryEntry {
    TrajectoryEntry {
        seq: None,
        run_id: None,
        turn: None,
        request: None,
        category: "Tool".into(),
        summary: summary.into(),
        detail: String::new(),
        lane: None,
        correlation_id: Some(correlation.into()),
        diagnostics: TrajectoryDiagnostics::default(),
    }
}

fn file_tool_entry(seq: u64, correlation: &str, summary: &str) -> TrajectoryEntry {
    TrajectoryEntry {
        seq: Some(seq),
        run_id: None,
        turn: None,
        request: None,
        category: "Tool".into(),
        summary: summary.into(),
        detail: String::new(),
        lane: None,
        correlation_id: Some(correlation.into()),
        diagnostics: TrajectoryDiagnostics::default(),
    }
}

#[test]
fn hydration_merge_keeps_live_tools_missing_from_snapshot() {
    let fresh = vec![file_tool_entry(1, "call-1", "read_file finished")];
    let live = vec![
        live_tool_entry("call-1", "read_file finished"),
        live_tool_entry("call-2", "run_command running"),
    ];
    let merged = merge_live_trajectory(fresh, &live);
    assert_eq!(merged.len(), 2);
    assert_eq!(
        merged
            .iter()
            .filter(|entry| entry.correlation_id.as_deref() == Some("call-1"))
            .count(),
        1,
        "snapshot-covered tools must not duplicate"
    );
    assert!(
        merged
            .iter()
            .any(|entry| entry.correlation_id.as_deref() == Some("call-2")),
        "post-snapshot live tools must survive hydration"
    );
}

#[test]
fn hydration_merge_dedups_uncorrelated_live_entries() {
    let uncorrelated = |summary: &str| TrajectoryEntry {
        seq: None,
        run_id: None,
        turn: None,
        request: None,
        category: "Subagent".into(),
        summary: summary.into(),
        detail: String::new(),
        lane: None,
        correlation_id: None,
        diagnostics: TrajectoryDiagnostics::default(),
    };
    let mut covered = uncorrelated("Subagent 0 started");
    covered.seq = Some(7);
    let live = vec![
        uncorrelated("Subagent 0 started"),
        uncorrelated("Subagent 1 started"),
    ];
    let merged = merge_live_trajectory(vec![covered], &live);
    assert_eq!(merged.len(), 2);
    assert!(
        merged
            .iter()
            .any(|entry| entry.summary == "Subagent 1 started"),
        "post-snapshot live entries must survive hydration"
    );
}

#[test]
fn hydration_merge_subagents_by_identity() {
    let activity = |batch: u64, task: usize| SubagentActivityInfo {
        batch_run_id: batch,
        task_index: task,
        journal_run_id: None,
        lane: None,
        agent: "worker".into(),
        task: "do things".into(),
        model: None,
        status: SubagentActivityStatus::Running,
        messages: Vec::new(),
        isolation: None,
        error: None,
    };
    let merged = merge_live_subagents(vec![activity(1, 0)], &[activity(1, 0), activity(1, 1)]);
    assert_eq!(merged.len(), 2);
    assert!(
        merged
            .iter()
            .any(|entry| entry.batch_run_id == 1 && entry.task_index == 1),
        "live subagents missing from the snapshot must survive hydration"
    );
}

#[test]
fn fusion_trajectory_rows_name_router_events_and_lane_models() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.active_work_dir = Some(std::env::temp_dir().join("threadlane-fusion-trajectory"));
    state.active_session_id = Some("fusion-session".into());
    state.record_trajectory(
        "fusion-session",
        &AgentEvent::SubagentStarted {
            run_id: 3,
            task_index: 0,
            journal_run_id: "child-run".into(),
            lane: "child-lane".into(),
            agent: "worker".into(),
            task: "implement".into(),
            model: "flash-sidekick".into(),
            isolation: None,
        },
    );
    state.record_trajectory(
        "fusion-session",
        &AgentEvent::FusionUpdate {
            model: "frontier-main".into(),
            message: "Fusion escalated to main".into(),
        },
    );
    let rows = &state.trajectory_by_session[&cached_key(&state, "fusion-session")];
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].category, "Subagent");
    assert!(
        rows[0].detail.contains("flash-sidekick"),
        "started rows must name the lane model so sidekick lanes read distinctly"
    );
    assert!(
        rows[0].detail.contains("child-run"),
        "started rows must keep the run id for correlation"
    );
    assert_eq!(rows[1].category, "Router");
    assert!(rows[1].summary.contains("frontier-main"));
}

fn computer_permission_request(id: &str) -> threadlane_protocol::PermissionRequest {
    threadlane_protocol::PermissionRequest {
        id: id.into(),
        capability: "computer".into(),
        title: "Click at (1, 1)".into(),
        detail: "click".into(),
        scopes: vec![threadlane_protocol::PermissionScope::Once],
    }
}

#[test]
fn mirror_trigger_fires_once_per_computer_activity() {
    let mut state = AppState::load_from_registry(Vec::new());
    assert!(!state.take_computer_mirror_trigger());

    state
        .pending_permissions
        .insert("perm-1".into(), computer_permission_request("perm-1"));
    assert!(state.take_computer_mirror_trigger());
    assert!(
        !state.take_computer_mirror_trigger(),
        "same permission must not retrigger"
    );

    state.messages_mut().push(ChatMessageInfo {
        id: "streaming-session-0".into(),
        role: MessageRole::Assistant,
        content: String::new(),
        tool_activities: vec![ToolActivityInfo {
            id: "call-shot".into(),
            category: "Working".into(),
            display_summary: "shot".into(),
            title: "computer_screenshot".into(),
            detail: String::new(),
            arguments: String::new(),
            is_expanded: false,
        }],
        streaming: true,
        reasoning_content: None,
        reasoning_expanded: false,
        retry_prompt: None,
    });
    assert!(state.take_computer_mirror_trigger());
    assert!(
        !state.take_computer_mirror_trigger(),
        "same tool call must not retrigger"
    );
}

#[tokio::test]
async fn one_time_permission_rejects_stale_and_persistent_decisions() {
    use threadlane_permission::PermissionDecision;
    let root = tempfile::tempdir().unwrap();
    let session_id = "scope-check";
    let session_file = root.path().join(".threadlane/sessions/scope-check.jsonl");
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    let mut state = AppState::load_from_registry(Vec::new());
    state.active_work_dir = Some(root.path().to_path_buf());
    state.active_session_id = Some(session_id.into());
    let runtime = state.ensure_session_runtime(root.path().to_path_buf(), session_file.clone());
    assert!(state.daemon_core.runtime_for_file(&session_file).is_some());
    assert!(state.scheduler_results.contains_key(&session_file));
    let handle = runtime.permission_handle();
    let (events, mut rx) = tokio::sync::broadcast::channel(4);
    let pending = tokio::spawn(async move {
        handle
            .request_external(
                &events,
                "test",
                "One-time test".into(),
                "No operation executes".into(),
                false,
                std::future::pending(),
            )
            .await
    });
    let AgentEvent::PermissionRequested { request } = rx.recv().await.unwrap() else {
        panic!("expected permission request");
    };
    state
        .pending_permissions
        .insert(session_id.into(), request.clone());
    assert!(!state.resolve_active_permission("stale-request", PermissionDecision::AllowOnce));
    assert!(!state.resolve_active_permission(&request.id, PermissionDecision::AllowAlways));
    assert!(!pending.is_finished());
    assert!(state.pending_permissions.contains_key(session_id));
    assert!(state.resolve_active_permission(&request.id, PermissionDecision::AllowOnce));
    assert_eq!(pending.await.unwrap(), Some(PermissionDecision::AllowOnce));
    assert!(!state.pending_permissions.contains_key(session_id));
    state.finish_session_removal(root.path(), session_id);
    assert!(state.daemon_core.runtime_for_file(&session_file).is_none());
    assert!(!state.scheduler_results.contains_key(&session_file));
}

fn worktree_session_state(work_dir: &Path, session_id: &str, worktree_dir: &Path) -> AppState {
    let mut state = issue_work_state(work_dir);
    state.projects[0].sessions.push(SessionInfo {
        id: session_id.into(),
        title: "Session".into(),
        work_dir: work_dir.to_path_buf(),
        runtime_work_dir: worktree_dir.to_path_buf(),
        session_file: work_dir.join(format!(".threadlane/sessions/{session_id}.jsonl")),
        updated_at: 0,
        health: SessionHealth::Healthy,
        git_branch: None,
        github_issue: None,
        is_worktree: true,
        worktree_available: true,
        completion_summary: SessionCompletionSummary::Unknown,
    });
    state
}

#[test]
fn remove_session_archives_transcript_before_delete() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(project.join(".threadlane/sessions")).unwrap();
    let project = project.canonicalize().unwrap();
    let mut state = issue_work_state(&project);
    let session_id = "session_gone";
    let session_file = project.join(format!(".threadlane/sessions/{session_id}.jsonl"));
    std::fs::write(&session_file, "{\"id\":\"x\"}\n").unwrap();

    state
        .remove_session(project.clone(), session_id.into(), false)
        .unwrap();
    assert!(!session_file.exists());
    let archived = project.join(format!(".threadlane/sessions/archive/{session_id}.jsonl"));
    assert_eq!(
        std::fs::read_to_string(&archived).unwrap(),
        "{\"id\":\"x\"}\n"
    );
}

#[test]
fn remove_session_refuses_dirty_worktree_but_allows_clean() {
    let dir = tempfile::tempdir().unwrap();
    let project = init_test_repo(&dir);
    std::fs::write(project.join("base.txt"), "base\n").unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-m", "initial"]);

    let session_id = "session_wt";
    let worktree_dir = project.join(format!(".threadlane/worktrees/{session_id}"));
    run_git(
        &project,
        &[
            "worktree",
            "add",
            worktree_dir.to_str().unwrap(),
            "-b",
            "worktree/session_wt",
        ],
    );
    let session_file = project.join(format!(".threadlane/sessions/{session_id}.jsonl"));
    std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    std::fs::write(&session_file, "{\"id\":\"y\"}\n").unwrap();
    let mut state = worktree_session_state(&project, session_id, &worktree_dir);

    // Dirty tracked file: refuse, keep everything.
    std::fs::write(worktree_dir.join("base.txt"), "dirty\n").unwrap();
    let error = state
        .remove_session(project.clone(), session_id.into(), true)
        .unwrap_err();
    assert!(error.contains("Commit or discard"), "{error}");
    assert!(worktree_dir.exists());
    assert!(session_file.exists());

    // Clean again: delete proceeds, transcript archived.
    run_git(&worktree_dir, &["checkout", "--", "."]);
    state
        .remove_session(project.clone(), session_id.into(), true)
        .unwrap();
    assert!(!worktree_dir.exists());
    assert!(!session_file.exists());
    let archived = project.join(format!(".threadlane/sessions/archive/{session_id}.jsonl"));
    assert_eq!(
        std::fs::read_to_string(&archived).unwrap(),
        "{\"id\":\"y\"}\n"
    );
}

#[test]
fn active_session_loading_requires_matching_message_hydration() {
    let mut state = AppState::load_from_registry(Vec::new());
    let work_dir = PathBuf::from("/tmp/threadlane-loading-state");
    let session_file = work_dir.join(".threadlane/sessions/session-1.jsonl");
    state.active_work_dir = Some(work_dir);
    state.active_session_id = Some("session-1".into());
    state.is_new_task = false;

    assert!(!state.active_session_is_loading());
    state.pending_hydrations.push(SessionHydrationRequest {
        session_id: "session-1".into(),
        session_file: session_file.clone(),
        reload_messages: true,
        runtime_options: None,
    });
    assert!(state.active_session_is_loading());

    state.pending_hydrations[0].reload_messages = false;
    assert!(!state.active_session_is_loading());
    state.pending_hydrations[0].reload_messages = true;
    state.pending_hydrations[0].session_id = "session-2".into();
    assert!(!state.active_session_is_loading());
    state.pending_hydrations[0].session_id = "session-1".into();
    state
        .pending_hydrations
        .push(state.pending_hydrations[0].clone());

    state.take_pending_hydrations();
    assert!(state.pending_hydrations.is_empty());
    assert!(state.active_session_is_loading());

    state.finish_session_hydration("session-1", &session_file);
    assert!(state.active_session_is_loading());
    state.finish_session_hydration("session-1", &session_file);
    assert!(!state.active_session_is_loading());
}

#[test]
fn pr_task_resolution_handles_checkout_paths_live_branches_and_project_scope() {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().canonicalize().unwrap();
    let project = project_dir.as_path();
    let checkout = Path::new("/worktrees/app");
    let mut state = worktree_session_state(project, "active", checkout);
    let mut first = state.projects[0].sessions[0].clone();
    first.id = "first".into();
    first.runtime_work_dir = PathBuf::from("/worktrees/first");
    first.git_branch = Some("feature/review".into());
    state.projects[0].sessions.insert(0, first);
    state.active_work_dir = Some(project.into());
    state.active_session_id = Some("active".into());
    // The agent checked out a branch after the durable metadata was saved.
    state.git_statuses.insert(
        checkout.into(),
        threadlane_git::GitStatus {
            branch: Some("feature/review".into()),
            ..Default::default()
        },
    );
    for path in [project, checkout] {
        assert_eq!(
            state.linked_pr_session(path, "feature/review").unwrap().id,
            "active"
        );
    }
    assert!(
        state
            .linked_pr_session(Path::new("/projects/other"), "feature/review")
            .is_none()
    );
    assert!(state.linked_pr_session(project, "").is_none());
    assert!(state.linked_pr_session(checkout, "other").is_none());
    state.projects[0].sessions[1].git_branch = Some("old-branch".into());
    assert!(state.linked_pr_session(checkout, "old-branch").is_none());
    state.git_statuses.get_mut(checkout).unwrap().detached = true;
    assert!(
        state
            .linked_pr_session(checkout, "feature/review")
            .is_none()
    );
    state.git_statuses.clear();
    state.projects[0].sessions[1].git_branch = Some("feature/review".into());
    assert_eq!(
        state
            .linked_pr_session(project, "feature/review")
            .unwrap()
            .id,
        "active"
    );
    state.projects[0].sessions[1].worktree_available = false;
    assert_eq!(
        state
            .linked_pr_session(project, "feature/review")
            .unwrap()
            .id,
        "first"
    );
    state.active_session_id = Some("missing".into());
    assert_eq!(
        state
            .linked_pr_session(project, "feature/review")
            .unwrap()
            .id,
        "first"
    );
}

#[test]
fn pr_review_actions_reject_missing_checkout_without_dispatch_or_tracking() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().canonicalize().unwrap();
    let checkout = project.join("missing-worktree");
    let mut state = worktree_session_state(&project, "review", &checkout);
    state.projects[0].sessions[0].git_branch = Some("feature/review".into());
    state.projects[0].sessions[0].worktree_available = false;
    state.auto_address_pr_reviews_enabled = true;
    let pr = threadlane_git::GitHubPrInfo {
        number: 275,
        state: "OPEN".into(),
        head_ref: "feature/review".into(),
        author: "author".into(),
        review_comments: vec![threadlane_git::PrReviewComment {
            author: "reviewer".into(),
            body: "Fix the missing checkout handling".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    for path in [&project, &checkout] {
        let error = state
            .address_pr_reviews_manual(path.clone(), pr.head_ref.clone(), &pr)
            .unwrap_err();
        assert!(error.contains("checkout is missing"), "{error}");
        assert!(
            state
                .auto_address_pr_reviews(path.clone(), pr.head_ref.clone(), &pr)
                .is_none()
        );
    }
    assert!(state.active_session_id.is_none());
    assert!(state.pr_review_tracking.is_empty());
    assert!(state.daemon_core.runtimes().is_empty());
}

#[test]
fn toggle_and_persist_pinned_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let mut state = AppState::default();

    assert!(!state.is_session_pinned(&work_dir, "session-1"));
    state
        .toggle_pinned_session(work_dir.clone(), "session-1".into())
        .expect("pin session-1");
    assert!(state.is_session_pinned(&work_dir, "session-1"));

    let pinned_file = work_dir.join(".threadlane/pinned_sessions.json");
    let content = std::fs::read_to_string(&pinned_file).expect("read pinned file");
    assert!(content.contains("session-1"));

    state
        .toggle_pinned_session(work_dir.clone(), "session-1".into())
        .expect("unpin session-1");
    assert!(!state.is_session_pinned(&work_dir, "session-1"));

    let content_after =
        std::fs::read_to_string(&pinned_file).expect("read pinned file after unpin");
    assert!(!content_after.contains("session-1"));
}

#[test]
fn pinned_session_atomic_persistence_and_reload() {
    let dir = tempfile::tempdir().unwrap();
    let work_dir = dir.path().to_path_buf();
    let mut state = AppState::default();

    state
        .toggle_pinned_session(work_dir.clone(), "session-1".into())
        .unwrap();
    state
        .toggle_pinned_session(work_dir.clone(), "session-2".into())
        .unwrap();
    assert!(state.is_session_pinned(&work_dir, "session-1"));
    assert!(state.is_session_pinned(&work_dir, "session-2"));

    let mut state2 = AppState::default();
    assert!(!state2.is_session_pinned(&work_dir, "session-1"));
    state2.load_pinned_sessions(&work_dir);
    assert!(state2.is_session_pinned(&work_dir, "session-1"));
    assert!(state2.is_session_pinned(&work_dir, "session-2"));
}

#[test]
fn worktree_setup_failure_and_cancellation_are_scoped_to_the_session() {
    use crate::worktree_setup::{SetupStage, WorktreeSetup};
    let root = tempfile::tempdir().unwrap();
    let project = root.path().to_path_buf();
    let id = "session_setup".to_string();
    let file = project.join(".threadlane/sessions/session_setup.jsonl");
    let mut state = AppState::for_tests();
    let setup = WorktreeSetup {
        project: project.clone(),
        session_id: id.clone(),
        session_file: file.clone(),
        worktree: project.join(".threadlane/worktrees/session_setup"),
        base: "main".into(),
        branch: None,
        stage: SetupStage::Naming,
        error: None,
        text: "Fix login".into(),
        images: Vec::new(),
        model: "test".into(),
        effort: ReasoningEffort::default(),
        acp_config: Vec::new(),
        cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    crate::worktree_setup::persist_request(&setup).unwrap();
    assert!(JsonlStore::open_read_only(&file)
        .unwrap()
        .facts()
        .get("worktree_setup")
        .unwrap()
        .contains("Fix login"));
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: discover_sessions_in_project(&project),
        is_expanded: true,
    });
    let recovered = crate::worktree_setup::recover(&state.projects[0].sessions[0]).unwrap();
    assert_eq!(recovered.text, "Fix login");
    assert!(recovered.error.as_deref().unwrap().contains("interrupted"));
    state.worktree_setups.insert(id.clone(), setup.clone());
    state.select_session_with_persistence(project, id.clone(), false);
    assert!(state.pending_hydrations.is_empty());
    assert!(state.is_generating);
    assert!(state.session_is_generating(&file));
    state.drain_chat_stream(vec![SessionEvent::WorktreeProgress {
        session_id: id.clone(),
        stage: SetupStage::Creating,
        branch: Some("worktree/fix-login".into()),
    }]);
    assert_eq!(
        state.active_worktree_setup().unwrap().stage,
        SetupStage::Creating
    );
    state.finish_worktree_setup(&id, Err("invalid base".into()));
    assert!(!state.is_generating);
    assert_eq!(
        state.active_worktree_setup().unwrap().error.as_deref(),
        Some("invalid base")
    );
    assert!(state.daemon_core.runtimes().is_empty());
    state.cancel_generation().unwrap();
    assert!(setup.cancelled.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        state.requested_composer_inserts.last().unwrap().text,
        "Fix login"
    );
    assert!(!file.exists());
    assert!(state.projects[0].sessions.is_empty());
    assert!(discover_sessions_in_project(&setup.project).is_empty());
    crate::worktree_setup::persist_request(&setup).unwrap();
    // A live worker keeps ownership until completion, even after the UI returns to a draft.
    setup
        .cancelled
        .store(false, std::sync::atomic::Ordering::Relaxed);
    state.worktree_setups.insert(id.clone(), setup.clone());
    state.select_session_with_persistence(setup.project.clone(), id.clone(), false);
    state.cancel_generation().unwrap();
    assert!(state.worktree_setups.contains_key(&id));
    assert!(state
        .remove_session(setup.project.clone(), id.clone(), true)
        .is_err());
    assert!(file.exists());
    // Simulate creation finishing after cancellation but before the worker acknowledgement.
    init_test_repo(&root);
    run_git(
        &setup.project,
        &["commit", "--allow-empty", "-qm", "initial"],
    );
    let branch = "worktree/fix-login-setup";
    threadlane_git::create_worktree_from(&setup.project, &setup.worktree, branch, "main").unwrap();
    let commit = threadlane_git::list_commits(&setup.worktree, 1).unwrap()[0]
        .sha
        .clone();
    for (key, value) in [
        ("git_branch", branch),
        ("worktree_setup_commit", commit.as_str()),
    ] {
        threadlane_coding_agent::harness::CodingSessionHarness::append_fact_to_path(
            &file, "main", key, value, None,
        )
        .unwrap();
    }
    std::fs::write(setup.worktree.join("user.txt"), "keep me").unwrap();
    let error = crate::worktree_setup::cleanup_cancelled(&setup).unwrap_err();
    assert!(error.contains("user.txt"), "{error}");
    assert!(file.exists());
    std::fs::remove_file(setup.worktree.join("user.txt")).unwrap();
    state.finish_worktree_setup(&id, Err("late result".into()));
    assert!(!file.exists(), "cleanup status: {:?}", state.session_status);
    assert!(!setup.worktree.exists());
    assert!(discover_sessions_in_project(&setup.project).is_empty());
    assert!(!threadlane_git::worktree_bases(&setup.project)
        .unwrap()
        .1
        .contains(&branch.to_string()));
    assert!(!state.worktree_setups.contains_key(&id));
    assert!(state.active_session_id.is_none());
    assert!(state.session_status.is_none());
    assert!(state.daemon_core.runtimes().is_empty());
}

/// Build daemon-owned normal/worktree run metadata without creating client-side files.
fn projected_automation_run(
    project: &Path,
    id: &str,
    worktree: bool,
) -> threadlane_automation::Run {
    use threadlane_automation::{Definition, Run, RunStatus, Schedule};
    let session_id = format!("automation-{id}");
    let runtime_root = if worktree {
        project.join(".threadlane/worktrees").join(&session_id)
    } else {
        project.to_path_buf()
    };
    Run {
        id: id.into(),
        definition: Definition {
            id: "definition".into(),
            revision: 1,
            name: "Remote task".into(),
            prompt: "Review changes".into(),
            project: project.to_path_buf(),
            model: "remote-model".into(),
            effort: "high".into(),
            worktree,
            schedule: Schedule::Manual,
            enabled: false,
            notify_all: false,
            anchor: 0,
            next_at: None,
            failures: 0,
            paused_reason: None,
        },
        scheduled_for: None,
        created_at: 1,
        finished_at: None,
        status: RunStatus::Running,
        session_file: Some(
            runtime_root
                .join(".threadlane/sessions")
                .join(format!("{session_id}.jsonl")),
        ),
        session_id,
        error: None,
        reviewed: false,
    }
}

/// Opening remote runs hydrates their exact daemon paths without local files or runtimes.
#[test]
fn remote_automation_run_opens_without_client_project_or_transcript_files() {
    for worktree in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("daemon-only");
        let run = projected_automation_run(&project, "run", worktree);
        let file = run.session_file.clone().unwrap();
        let mut state = AppState::load_from_registry(Vec::new());
        state.daemon_remote = true;
        state.sidebar_project_filter = Some(PathBuf::from("/unchanged-filter"));
        state.automations.snapshot.runs.push(run.clone());
        state.open_automation_run(&run.id).unwrap();
        assert_eq!(state.workspace_page, WorkspacePage::Chat);
        assert_eq!(
            state.active_session_id.as_deref(),
            Some(run.session_id.as_str())
        );
        assert_eq!(state.active_work_dir.as_ref(), Some(&project));
        assert_eq!(state.active_session_info().unwrap().session_file, file);
        assert_eq!(
            state.active_session_info().unwrap().runtime_work_dir,
            file.ancestors().nth(3).unwrap()
        );
        assert_eq!(state.active_session_info().unwrap().is_worktree, worktree);
        assert!(state.is_generating);
        assert_eq!(state.selected_model, "remote-model");
        assert_eq!(state.reasoning_effort, ReasoningEffort::High);
        assert_eq!(
            state.sidebar_project_filter.as_deref(),
            Some(Path::new("/unchanged-filter"))
        );
        assert_eq!(
            state.take_pending_hydrations(),
            vec![SessionHydrationRequest {
                session_id: run.session_id,
                session_file: file,
                reload_messages: true,
                runtime_options: None,
            }]
        );
        assert!(state.daemon_core.runtimes().is_empty());
        assert!(
            !project.exists(),
            "remote navigation must not read/create client project state"
        );
    }
}

/// Previously received daemon session metadata takes precedence over a lightweight run stub.
#[test]
fn remote_automation_navigation_uses_projected_session_metadata() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("daemon-only");
    let run = projected_automation_run(&project, "run", true);
    let mut session = test_session(&run.session_id, run.session_file.as_ref().unwrap());
    session.work_dir = project.clone();
    session.title = "Daemon session title".into();
    session.git_branch = Some("daemon-branch".into());
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    assert!(state.drain_chat_stream(vec![SessionEvent::ProjectChanged {
        project: ProjectInfo {
            work_dir: project.clone(),
            name: "Daemon project".into(),
            sessions: vec![session.clone()],
            is_expanded: true,
        }
    }]));
    state.automations.snapshot.runs.push(run.clone());
    state.open_automation_run(&run.id).unwrap();
    assert_eq!(state.active_session_info(), Some(&session));
    assert_eq!(state.projects[0].name, "Daemon project");
    assert!(!project.exists());
}

/// An older project list cannot erase the selected worktree identity before hydration arrives.
#[test]
fn remote_automation_loading_identity_survives_late_project_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("daemon-only");
    let run = projected_automation_run(&project, "run", true);
    let file = run.session_file.clone().unwrap();
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.automations.snapshot.runs.push(run.clone());
    state.open_automation_run(&run.id).unwrap();
    state.take_pending_hydrations();
    assert!(state.drain_chat_stream(vec![SessionEvent::ProjectChanged {
        project: ProjectInfo {
            work_dir: project.clone(),
            name: "Older project snapshot".into(),
            sessions: vec![],
            is_expanded: true,
        }
    }]));
    assert!(state.active_session_matches(&run.session_id, &file));
    assert!(state.active_session_is_loading());

    let mut session = state.active_session_info().unwrap().clone();
    session.title = "Hydrated remote title".into();
    session.git_branch = Some("automation/branch".into());
    let snapshot = threadlane_protocol::daemon::SessionSnapshot {
        session: session.clone(),
        messages: vec![queued_echo("remote-message")],
        ..Default::default()
    };
    assert!(state.drain_chat_stream(vec![SessionEvent::SessionSnapshot {
        session_id: run.session_id,
        snapshot: Box::new(snapshot),
    }]));
    assert_eq!(state.active_session_info(), Some(&session));
    assert_eq!(state.messages[0].id, "remote-message");
    assert!(!state.active_session_is_loading());
    assert!(state.session_status.is_none());
    assert!(state.presented_completion.is_none());
    assert!(!state.acknowledge_presented_completion());
    assert!(!project.exists());
}

/// Foreign or superseded hydration cannot overwrite the currently selected chat.
#[test]
fn remote_automation_hydration_rejects_foreign_identity_and_newer_navigation_wins() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("daemon-only");
    let first = projected_automation_run(&project, "first", true);
    let second = projected_automation_run(&project, "second", false);
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state
        .automations
        .snapshot
        .runs
        .extend([first.clone(), second.clone()]);
    state.open_automation_run(&first.id).unwrap();
    state.take_pending_hydrations();
    let original = state.active_session_info().unwrap().clone();
    for variant in 0..3 {
        let mut foreign = original.clone();
        match variant {
            0 => foreign.id = "other-session".into(),
            1 => foreign.work_dir = root.path().join("other-project"),
            _ => foreign.session_file = root.path().join("other-transcript.jsonl"),
        }
        assert!(!state.apply_session_snapshot(
            &first.session_id,
            threadlane_protocol::daemon::SessionSnapshot {
                session: foreign,
                messages: vec![queued_echo("wrong-message")],
                ..Default::default()
            }
        ));
        assert!(state.active_session_is_loading());
        assert!(state.messages.is_empty());
    }
    state.open_automation_run(&second.id).unwrap();
    assert!(!state.apply_session_snapshot(
        &first.session_id,
        threadlane_protocol::daemon::SessionSnapshot {
            session: original,
            messages: vec![queued_echo("late-message")],
            ..Default::default()
        }
    ));
    assert_eq!(
        state.active_session_id.as_deref(),
        Some(second.session_id.as_str())
    );
    assert!(state.messages.is_empty());
    assert!(!project.exists());
}

/// Remote generation state preserves live trajectory rows over an older hydration snapshot.
#[test]
fn remote_automation_hydration_preserves_live_activity_without_a_local_runtime() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("daemon-only");
    let run = projected_automation_run(&project, "live", false);
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.automations.snapshot.runs.push(run.clone());
    state.open_automation_run(&run.id).unwrap();
    let key = AppState::projection_key(&run.session_id, run.session_file.as_ref().unwrap());
    state
        .trajectory_by_session
        .insert(key.clone(), vec![live_tool_entry("live", "still running")]);
    let snapshot = threadlane_protocol::daemon::SessionSnapshot {
        session: state.active_session_info().unwrap().clone(),
        trajectory: vec![file_tool_entry(1, "older", "finished earlier")],
        ..Default::default()
    };
    assert!(state.apply_session_snapshot(&run.session_id, snapshot));
    let entries = &state.trajectory_by_session[&key];
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].correlation_id.as_deref(), Some("older"));
    assert_eq!(entries[1].correlation_id.as_deref(), Some("live"));
    assert!(state.daemon_core.runtimes().is_empty());
    assert!(!project.exists());
}

/// Remote snapshots correct stale run health unless a newer live lifecycle event already won.
#[test]
fn remote_automation_hydration_reconciles_health_and_preserves_newer_live_status() {
    for (initial_status, snapshot_health, live_event, expected) in [
        (
            threadlane_automation::RunStatus::Running,
            SessionHealth::Healthy,
            None,
            false,
        ),
        (
            threadlane_automation::RunStatus::Succeeded,
            SessionHealth::Working,
            None,
            true,
        ),
        (
            threadlane_automation::RunStatus::Succeeded,
            SessionHealth::Healthy,
            Some(AgentEvent::AgentStart),
            true,
        ),
        (
            threadlane_automation::RunStatus::Running,
            SessionHealth::Working,
            Some(AgentEvent::AgentEnd {
                usage: Default::default(),
            }),
            false,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("daemon-only");
        let mut run = projected_automation_run(&project, "status", false);
        run.status = initial_status;
        let mut state = AppState::load_from_registry(Vec::new());
        state.daemon_remote = true;
        state.automations.snapshot.runs.push(run.clone());
        state.open_automation_run(&run.id).unwrap();
        state.take_pending_hydrations();
        if let Some(event) = live_event {
            state.drain_chat_stream(vec![SessionEvent::Agent {
                session_id: run.session_id.clone(),
                event,
            }]);
        }
        let mut session = state.active_session_info().unwrap().clone();
        session.health = snapshot_health;
        assert!(state.apply_session_snapshot(
            &run.session_id,
            threadlane_protocol::daemon::SessionSnapshot {
                session,
                ..Default::default()
            }
        ));
        assert_eq!(state.is_generating, expected);
        assert_eq!(
            state.active_session_info().unwrap().health == SessionHealth::Working,
            expected
        );
        assert!(state.session_status.is_none());
    }
}

/// A deferred completion from an older background run is not newer than fresh hydration.
#[test]
fn remote_automation_hydration_status_wins_over_deferred_completion() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("daemon-only");
    let run = projected_automation_run(&project, "restarted", false);
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.automations.snapshot.runs.push(run.clone());
    state.drain_chat_stream(vec![SessionEvent::Finished {
        session_id: run.session_id.clone(),
        session_file: run.session_file.clone().unwrap(),
    }]);
    state.open_automation_run(&run.id).unwrap();
    let session = state.active_session_info().unwrap().clone();
    assert_eq!(session.health, SessionHealth::Working);
    state.drain_chat_stream(vec![SessionEvent::SessionSnapshot {
        session_id: run.session_id,
        snapshot: Box::new(threadlane_protocol::daemon::SessionSnapshot {
            session,
            ..Default::default()
        }),
    }]);
    assert!(state.is_generating);
    assert!(state.remote_live_status.is_none());
}

/// A reconnect snapshot can repair a terminal lifecycle event missed by the previous connection.
#[test]
fn remote_automation_reconnect_snapshot_restores_missed_status() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("daemon-only");
    let run = projected_automation_run(&project, "status", false);
    let client = Arc::new(RecordingDaemonClient::default());
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.daemon_client = client.clone();
    state.automations.snapshot.runs.push(run.clone());
    state.open_automation_run(&run.id).unwrap();
    state.drain_chat_stream(vec![SessionEvent::Agent {
        session_id: run.session_id.clone(),
        event: AgentEvent::AgentStart,
    }]);
    client.epoch.store(1, std::sync::atomic::Ordering::SeqCst);
    let mut session = state.active_session_info().unwrap().clone();
    session.health = SessionHealth::Healthy;
    assert!(state.apply_session_snapshot(
        &run.session_id,
        threadlane_protocol::daemon::SessionSnapshot {
            session,
            ..Default::default()
        }
    ));
    assert!(!state.is_generating);
}

/// Remote completion refreshes go to the daemon and cannot be overwritten by local discovery.
#[tokio::test]
async fn remote_automation_completion_refreshes_daemon_project_only() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("daemon-only");
    let run = projected_automation_run(&project, "done", true);
    let client = Arc::new(RecordingDaemonClient::default());
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.daemon_client = client.clone();
    let (tx, rx) = mpsc::channel();
    state.session_refresh_tx = tx;
    state.automations.snapshot.runs.push(run.clone());
    state.open_automation_run(&run.id).unwrap();
    state.drain_chat_stream(vec![SessionEvent::Finished {
        session_id: run.session_id.clone(),
        session_file: run.session_file.clone().unwrap(),
    }]);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if client.commands.lock().unwrap().iter().any(|command| matches!(command, SessionCommand::GetProjectState { work_dir } if work_dir == &project)) { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(rx.try_recv().is_err());
    assert!(!state.apply_session_refresh(
        project.clone(),
        Vec::new(),
        state.session_refresh_generation
    ));
    assert_eq!(
        state.active_session_id.as_deref(),
        Some(run.session_id.as_str())
    );
    assert_eq!(state.projects[0].sessions.len(), 1);
    assert!(!project.exists());
}

/// Missing run/transcript metadata fails before changing the current selection.
#[test]
fn remote_automation_run_without_chat_fails_without_changing_selection() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.active_session_id = Some("original".into());
    let mut run = projected_automation_run(Path::new("/daemon/project"), "pending", false);
    run.session_file = None;
    state.automations.snapshot.runs.push(run);
    assert_eq!(
        state.open_automation_run("pending").unwrap_err(),
        "This run has no chat yet"
    );
    assert_eq!(
        state.open_automation_run("missing").unwrap_err(),
        "Run no longer exists"
    );
    assert_eq!(state.active_session_id.as_deref(), Some("original"));
    assert!(state.projects.is_empty());
    assert!(state.pending_hydrations.is_empty());
}

#[test]
fn automation_navigation_preserves_chat_and_project_scope() {
    let mut state = crate::AppState::load_from_registry(vec![]);
    state.active_session_id = Some("original".into());
    state.active_work_dir = Some(std::path::PathBuf::from("/project"));
    state.sidebar_project_filter = Some(std::path::PathBuf::from("/filter"));
    crate::controller::dispatch(&mut state, crate::actions::AppAction::OpenAutomations);
    assert_eq!(state.workspace_page, crate::WorkspacePage::Automations);
    assert_eq!(state.active_session_id.as_deref(), Some("original"));
    assert_eq!(
        state.active_work_dir,
        Some(std::path::PathBuf::from("/project"))
    );
    assert_eq!(
        state.sidebar_project_filter,
        Some(std::path::PathBuf::from("/filter"))
    );
}

#[test]
fn completed_run_refresh_preserves_questions_from_a_later_chat_turn() {
    use threadlane_automation::{Definition, RunStatus, Schedule, Store};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let mut store = Store::open(&root).unwrap();
    store
        .save(
            Definition {
                id: "automation-test".into(),
                revision: 0,
                name: "Test".into(),
                prompt: "automation test prompt".into(),
                project: root.clone(),
                model: "gpt-4o".into(),
                effort: "medium".into(),
                worktree: false,
                schedule: Schedule::Manual,
                enabled: false,
                notify_all: false,
                anchor: 0,
                next_at: None,
                failures: 0,
                paused_reason: None,
            },
            0,
        )
        .unwrap();
    let id = store.enqueue("automation-test", false, 1).unwrap();
    store
        .update_run(&id, RunStatus::Succeeded, None, None, 2)
        .unwrap();
    let session_id = store.snapshot().runs[0].session_id.clone();
    let mut state = crate::AppState::load_from_registry(vec![]);
    state.pending_questions.insert(
        session_id.clone(),
        threadlane_protocol::QuestionRequest {
            id: "later-turn-question".into(),
            questions: vec![],
        },
    );
    state.apply_automation_projection(crate::automation::Projection {
        snapshot: store.snapshot().clone(),
        ..Default::default()
    });
    assert_eq!(
        state.pending_questions[&session_id].id,
        "later-turn-question"
    );
}

fn run_completion(seq: u64) -> RunCompletionToken {
    RunCompletionToken {
        record_id: format!("finish-{seq}"),
        run_id: format!("run-{seq}"),
        seq,
    }
}

#[test]
fn completion_alerts_accept_current_inventory_only_and_preserve_result_marker() {
    let (_root, project, mut session) = settled_project_session("alerts");
    session.completion_summary = SessionCompletionSummary::None;
    let mut state = state_with_session(&session);
    state.register_session_seen(&project, &session.id);
    state.set_completion_alerts_enabled(true);
    let generation = state.session_refresh_generation;
    let mut completed = session.clone();
    completed.completion_summary = SessionCompletionSummary::Latest(run_completion(9));
    assert!(!state.apply_session_refresh(project.clone(), vec![completed.clone()], generation.wrapping_sub(1)));
    assert!(state.completion_alerts.drain().is_empty());
    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], generation));
    assert!(state.apply_session_refresh(project.clone(), vec![completed.clone()], generation));
    assert_eq!(state.completion_alerts.drain().len(), 1);
    assert!(state.session_has_unseen_result(&completed));
    // Reconnect/client replacement rejects outstanding local inventory before baselining.
    state.daemon_client = threadlane_client::LocalDaemon::new(state.daemon_core.clone());
    assert!(!state.apply_session_refresh(project.clone(), vec![completed.clone()], generation));
    let generation = state.session_refresh_generation;
    assert!(state.apply_session_refresh(project.clone(), vec![completed.clone()], generation));
    assert!(state.completion_alerts.drain().is_empty());
    state.set_completion_alerts_enabled(false);
    state.set_completion_alerts_enabled(true);
    let generation = state.session_refresh_generation;
    completed.completion_summary = SessionCompletionSummary::Latest(run_completion(10));
    assert!(state.apply_session_refresh(project.clone(), vec![completed.clone()], generation));
    assert!(state.completion_alerts.drain().is_empty());
    state.projects.clear();
    assert!(!state.apply_session_refresh(project, vec![completed], generation));
}

#[test]
fn completion_alerts_automation_remote_and_noncompletion_events_never_emit() {
    let (_root, project, mut session) = settled_project_session("alerts");
    session.completion_summary = SessionCompletionSummary::None;
    let mut state = state_with_session(&session);
    state.set_completion_alerts_enabled(true);
    let generation = state.session_refresh_generation;
    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], generation));
    let mut run = projected_automation_run(&project, "done", false);
    run.session_file = Some(session.session_file.clone());
    state.automations.snapshot.runs.push(run);
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(1));
    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], generation));
    assert!(state.completion_alerts.drain().is_empty());
    state.automations.snapshot.runs.clear();
    state.daemon_remote = true;
    let project_info = ProjectInfo { name: "remote".into(), work_dir: project.clone(), sessions: vec![session.clone()], is_expanded: true };
    state.drain_chat_stream(vec![SessionEvent::ProjectChanged { project: project_info }]);
    assert!(!state.apply_session_refresh(project.clone(), vec![session.clone()], generation));
    assert!(state.completion_alerts.drain().is_empty());
    // Health/status alone (including failures/cancellation/waiting) is not a completion token.
    state.daemon_remote = false;
    state.sync_completion_alert_context();
    let generation = state.session_refresh_generation;
    session.completion_summary = SessionCompletionSummary::None;
    for health in [SessionHealth::Working, SessionHealth::Healthy, SessionHealth::Warning] {
        session.health = health;
        assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], generation));
        assert!(state.completion_alerts.drain().is_empty());
    }
}

#[test]
fn completion_alerts_disabling_preserves_inflight_inventory_and_result_marker() {
    let (_root, project, mut session) = settled_project_session("alerts");
    session.completion_summary = SessionCompletionSummary::None;
    let mut state = state_with_session(&session);
    state.register_session_seen(&project, &session.id);
    state.set_completion_alerts_enabled(true);
    let generation = state.session_refresh_generation;
    state.set_completion_alerts_enabled(false);
    assert_eq!(state.session_refresh_generation, generation);
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(1));
    assert!(state.apply_session_refresh(project, vec![session.clone()], generation));
    assert!(state.completion_alerts.drain().is_empty());
    assert!(state.session_has_unseen_result(&session));
}

#[test]
fn completion_alerts_fork_baselines_inherited_history_before_first_new_run() {
    for inherited in [SessionCompletionSummary::None, SessionCompletionSummary::Latest(run_completion(5))] {
        let (_root, project, source) = settled_project_session("source");
        let mut state = state_with_session(&source);
        state.set_completion_alerts_enabled(true);
        let mut fork = source.clone();
        fork.id = "fork".into();
        fork.session_file = canonical_session_file(&project, "fork");
        fork.completion_summary = inherited;
        state.finish_session_fork(project.clone(), fork.id.clone(), vec![fork.clone()]);
        assert!(state.completion_alerts.drain().is_empty());
        assert!(!state.session_has_unseen_result(&fork));
        fork.completion_summary = SessionCompletionSummary::Latest(run_completion(6));
        assert!(state.apply_session_refresh(project, vec![fork.clone(), source], state.session_refresh_generation));
        assert_eq!(state.completion_alerts.drain().len(), 1);
        assert!(state.session_has_unseen_result(&fork));
    }
}

#[test]
fn session_seen_baseline_hides_existing_history_and_tracks_new_completions() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().to_path_buf();
    let session_file = project.join(".threadlane/sessions/session.jsonl");
    let mut session = test_session("session", &session_file);
    session.work_dir = project.clone();
    session.runtime_work_dir = project.clone();
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(7));

    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: Vec::new(),
        is_expanded: true,
    });

    // First successful discovery after upgrade baselines existing history —
    // a previously unseen result predating the marker is never reported.
    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], 0));
    assert!(!state.session_has_unseen_result(&session));

    // A newer confirmed completion marks the row until it is presented.
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(9));
    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], 0));
    assert!(state.session_has_unseen_result(&session));

    // Temporary absence keeps the watermark instead of rebaselining.
    assert!(state.apply_session_refresh(project.clone(), Vec::new(), 0));
    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], 0));
    assert!(state.session_has_unseen_result(&session));
}

#[test]
fn registered_session_reports_first_result_instead_of_baselines() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().to_path_buf();
    let session_file = project.join(".threadlane/sessions/session.jsonl");
    let mut session = test_session("session", &session_file);
    session.work_dir = project.clone();
    session.runtime_work_dir = project.clone();
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(4));

    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: Vec::new(),
        is_expanded: true,
    });
    state.register_session_seen(&project, "session");

    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], 0));
    assert!(state.session_has_unseen_result(&session));
}

#[test]
fn acknowledged_presented_completion_clears_the_marker() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().to_path_buf();
    let session_file = project.join(".threadlane/sessions/session.jsonl");
    let mut session = test_session("session", &session_file);
    session.work_dir = project.clone();
    session.runtime_work_dir = project.clone();
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(7));

    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: Vec::new(),
        is_expanded: true,
    });
    state.active_work_dir = Some(project.clone());
    state.active_session_id = Some("session".into());
    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], 0));
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(9));
    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], 0));
    assert!(state.session_has_unseen_result(&session));

    // The hydrate captured token 9 and the transcript applied: presenting it
    // acknowledges exactly that completion.
    state.apply_session_messages(
        "session",
        &session_file,
        Vec::new(),
        Some(run_completion(9)),
    );
    assert!(state.acknowledge_presented_completion());
    assert!(!state.session_has_unseen_result(&session));
    assert!(!state.acknowledge_presented_completion());

    // A presentation that could not confirm a token never acknowledges.
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(11));
    assert!(state.apply_session_refresh(project.clone(), vec![session.clone()], 0));
    assert!(state.session_has_unseen_result(&session));
    state.apply_session_messages("session", &session_file, Vec::new(), None);
    assert!(!state.acknowledge_presented_completion());
    assert!(state.session_has_unseen_result(&session));

    // A stale presentation for a different session file is also refused.
    state.apply_session_messages(
        "session",
        Path::new("/elsewhere/session.jsonl"),
        Vec::new(),
        Some(run_completion(11)),
    );
    assert!(!state.acknowledge_presented_completion());
    assert!(state.session_has_unseen_result(&session));
}

#[test]
fn session_work_dir_for_file_prefers_the_canonical_project() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let worktree = project.join(".threadlane/worktrees/session");
    let session_file = worktree.join(".threadlane/sessions/session.jsonl");
    let mut session = test_session("session", &session_file);
    session.work_dir = project.clone();
    session.runtime_work_dir = worktree.clone();
    session.is_worktree = true;

    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: project.clone(),
        sessions: vec![session],
        is_expanded: true,
    });

    assert_eq!(
        state.session_work_dir_for_file(&session_file),
        Some(project)
    );
}

#[test]
fn remote_edit_queued_message_restores_staged_images_via_command_result() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.active_session_id = Some("sess-1".into());
    state.messages = vec![ChatMessageInfo {
        id: "queued-user-sess-1-entry-1".into(),
        role: MessageRole::User,
        content: "draft text".into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
        retry_prompt: None,
    }]
    .into();

    // The optimistic echo returns the staged text immediately; the echo
    // holds no images, so the staged images ride the request's reply.
    // The row stays — the removal is not claimed until confirmed.
    let (text, images) = state
        .edit_queued_message("entry-1")
        .expect("remote edit succeeds");
    assert_eq!(text, "draft text");
    assert!(images.is_empty());
    assert!(
        state
            .messages
            .iter()
            .any(|message| message.id == "queued-user-sess-1-entry-1")
    );
    assert!(state.queued_removal_pending("sess-1", "entry-1"));
    assert_eq!(state.pending_queued_cancels.len(), 1);

    let image = ImageAttachment {
        display_name: "shot.png".into(),
        data_url: "data:image/png;base64,AAAA".into(),
    };
    let changed = state.drain_chat_stream(vec![SessionEvent::CommandResult {
        request_id: 1,
        result: Ok(CommandResponse::CancelledQueuedMessage {
            session_id: "sess-1".into(),
            entry_id: "entry-1".into(),
            text: "draft text".into(),
            images: vec![image.clone()],
        }),
    }]);
    assert!(changed);
    assert!(state.pending_queued_cancels.is_empty());
    assert!(!state.queued_removal_pending("sess-1", "entry-1"));
    assert!(
        state
            .messages
            .iter()
            .all(|message| message.id != "queued-user-sess-1-entry-1")
    );
    let insert = state
        .requested_composer_inserts
        .last()
        .expect("composer insert queued");
    // Text already came back through the echo — the reply adds the images.
    assert!(insert.text.is_empty());
    assert_eq!(insert.images, vec![image]);
    assert_eq!(insert.session_id.as_deref(), Some("sess-1"));
}

#[test]
fn remote_edit_queued_message_scopes_restore_to_project() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.active_work_dir = Some(std::path::PathBuf::from("/tmp/project-a"));
    state.active_session_id = Some("sess-1".into());
    state.messages = vec![ChatMessageInfo {
        id: "queued-user-sess-1-entry-1".into(),
        role: MessageRole::User,
        content: "draft text".into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
        retry_prompt: None,
    }]
    .into();

    state
        .edit_queued_message("entry-1")
        .expect("remote edit succeeds");

    let changed = state.drain_chat_stream(vec![SessionEvent::CommandResult {
        request_id: 1,
        result: Ok(CommandResponse::CancelledQueuedMessage {
            session_id: "sess-1".into(),
            entry_id: "entry-1".into(),
            text: "draft text".into(),
            images: vec![ImageAttachment {
                display_name: "shot.png".into(),
                data_url: "data:image/png;base64,AAAA".into(),
            }],
        }),
    }]);
    assert!(changed);
    let insert = state
        .requested_composer_inserts
        .last()
        .expect("composer insert queued");
    // The staged payload keys on the project too: a same-named session in
    // another project's draft must not receive it.
    assert_eq!(insert.session_id.as_deref(), Some("sess-1"));
    assert_eq!(
        insert.work_dir.as_deref(),
        Some(std::path::Path::new("/tmp/project-a"))
    );
}

#[test]
fn remote_edit_queued_message_reply_survives_a_session_switch() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.active_session_id = Some("sess-1".into());
    state.messages = vec![ChatMessageInfo {
        id: "queued-user-sess-1-entry-1".into(),
        role: MessageRole::User,
        content: "draft text".into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
        retry_prompt: None,
    }]
    .into();

    state
        .edit_queued_message("entry-1")
        .expect("remote edit succeeds");

    // The user switches sessions before the reply lands: the payload is
    // retained, scoped to the session that owned the queued message —
    // it must not be dropped or delivered to the wrong composer.
    state.active_session_id = Some("sess-2".into());
    let changed = state.drain_chat_stream(vec![SessionEvent::CommandResult {
        request_id: 1,
        result: Ok(CommandResponse::CancelledQueuedMessage {
            session_id: "sess-1".into(),
            entry_id: "entry-1".into(),
            text: "draft text".into(),
            images: vec![ImageAttachment {
                display_name: "shot.png".into(),
                data_url: "data:image/png;base64,AAAA".into(),
            }],
        }),
    }]);
    assert!(changed);
    assert!(state.pending_queued_cancels.is_empty());
    let insert = state
        .requested_composer_inserts
        .last()
        .expect("composer insert queued");
    assert_eq!(insert.session_id.as_deref(), Some("sess-1"));
    assert_eq!(insert.images.len(), 1);
}

#[test]
fn remote_edit_queued_message_recovers_via_journaled_cancel_event() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.active_session_id = Some("sess-1".into());
    state.messages = vec![ChatMessageInfo {
        id: "queued-user-sess-1-entry-1".into(),
        role: MessageRole::User,
        content: "draft text".into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
        retry_prompt: None,
    }]
    .into();

    state
        .edit_queued_message("entry-1")
        .expect("remote edit succeeds");

    // The reply died with the socket; the journaled QueuedEntryCancelled
    // replayed on reconnect carries the same payload and resolves the
    // parked intent.
    let image = ImageAttachment {
        display_name: "shot.png".into(),
        data_url: "data:image/png;base64,AAAA".into(),
    };
    let changed = state.drain_chat_stream(vec![SessionEvent::QueuedEntryCancelled {
        session_id: "sess-1".into(),
        entry_id: "entry-1".into(),
        request_id: Some(1),
        text: "draft text".into(),
        images: vec![image.clone()],
    }]);
    assert!(changed);
    assert!(state.pending_queued_cancels.is_empty());
    assert!(
        state
            .messages
            .iter()
            .all(|message| message.id != "queued-user-sess-1-entry-1")
    );
    let insert = state
        .requested_composer_inserts
        .last()
        .expect("composer insert queued");
    assert_eq!(insert.images, vec![image]);

    // An uncorrelated event (fire-and-forget cancel from another client)
    // resolves nothing when no matching echo remains.
    assert!(!state.drain_chat_stream(vec![
        SessionEvent::QueuedEntryCancelled {
            session_id: "sess-1".into(),
            entry_id: "entry-2".into(),
            request_id: None,
            text: String::new(),
            images: Vec::new(),
        },
    ]));
}

/// A `DaemonClient` that answers capability probing like a pre-envelope
/// daemon (or a still-connecting remote) and records what was sent, so
/// tests can prove a bare command went out and no `CommandRequest` did.
#[derive(Default)]
struct RecordingDaemonClient {
    commands: Mutex<Vec<SessionCommand>>,
    requests: Mutex<Vec<CommandRequest>>,
    supports_requests: bool,
    epoch: std::sync::atomic::AtomicU64,
}

#[async_trait::async_trait]
impl threadlane_client::DaemonClient for RecordingDaemonClient {
    async fn command(&self, command: SessionCommand) -> Result<(), String> {
        self.commands.lock().unwrap().push(command);
        Ok(())
    }

    async fn command_request(
        &self,
        request: CommandRequest,
    ) -> Result<CommandResponse, String> {
        self.requests.lock().unwrap().push(request);
        Ok(CommandResponse::Ack)
    }

    fn supports_command_requests(&self) -> bool {
        self.supports_requests
    }

    fn supports_project_io(&self) -> bool {
        false
    }

    fn supports_github_automation(&self) -> bool {
        false
    }

    fn file_search_connection_epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<SessionEvent> {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        rx
    }
}

fn queued_echo(id: &str) -> ChatMessageInfo {
    ChatMessageInfo {
        id: id.into(),
        role: MessageRole::User,
        content: "draft text".into(),
        tool_activities: Vec::new(),
        streaming: false,
        reasoning_content: None,
        reasoning_expanded: false,
        retry_prompt: None,
    }
}

fn queued_state() -> AppState {
    let mut state = AppState::load_from_registry(Vec::new());
    state.daemon_remote = true;
    state.active_session_id = Some("sess-1".into());
    state.messages = vec![queued_echo("queued-user-sess-1-entry-1")].into();
    state
}

#[test]
fn remote_cancel_queued_message_keeps_echo_until_confirmed() {
    let mut state = queued_state();

    state
        .cancel_queued_message("entry-1")
        .expect("remote cancel succeeds");
    // The entry may still be queued on the peer: the row stays in a
    // pending-removal state rather than claiming removal.
    assert!(
        state
            .messages
            .iter()
            .any(|message| message.id == "queued-user-sess-1-entry-1")
    );
    assert!(state.queued_removal_pending("sess-1", "entry-1"));

    let changed = state.drain_chat_stream(vec![SessionEvent::QueuedEntryCancelled {
        session_id: "sess-1".into(),
        entry_id: "entry-1".into(),
        request_id: Some(1),
        text: "draft text".into(),
        images: Vec::new(),
    }]);
    assert!(changed);
    assert!(state.pending_queued_cancels.is_empty());
    assert!(!state.queued_removal_pending("sess-1", "entry-1"));
    assert!(
        state
            .messages
            .iter()
            .all(|message| message.id != "queued-user-sess-1-entry-1")
    );
    // A plain remove discards the payload — nothing goes to the composer.
    assert!(state.requested_composer_inserts.is_empty());
    assert_eq!(
        state.session_status.as_deref(),
        Some("Queued message removed")
    );
}

#[test]
fn remote_queued_cancel_error_keeps_the_row() {
    let mut state = queued_state();

    state
        .cancel_queued_message("entry-1")
        .expect("remote cancel succeeds");

    // A rejected request resolves the intent without touching the row —
    // the entry genuinely remains queued, and the status says so.
    let changed = state.drain_chat_stream(vec![SessionEvent::CommandResult {
        request_id: 1,
        result: Err("not queued".into()),
    }]);
    assert!(changed);
    assert!(state.pending_queued_cancels.is_empty());
    assert!(
        state
            .messages
            .iter()
            .any(|message| message.id == "queued-user-sess-1-entry-1")
    );
    assert_eq!(
        state.session_status.as_deref(),
        Some("Could not remove queued message: not queued")
    );
}

#[test]
fn remote_queued_cancel_not_pending_drops_the_stale_row() {
    let mut state = queued_state();

    state
        .cancel_queued_message("entry-1")
        .expect("remote cancel succeeds");
    assert!(state.queued_removal_pending("sess-1", "entry-1"));

    // The scheduler consumed the entry between the row rendering and the
    // cancel landing: "no longer pending" is authoritative that it left
    // the queue, so the retained echo is reconciled, not kept.
    let changed = state.drain_chat_stream(vec![SessionEvent::CommandResult {
        request_id: 1,
        result: Err(threadlane_coding_agent::scheduler::QUEUED_ENTRY_NOT_PENDING
            .to_string()),
    }]);
    assert!(changed);
    assert!(state.pending_queued_cancels.is_empty());
    assert!(
        state
            .messages
            .iter()
            .all(|message| message.id != "queued-user-sess-1-entry-1")
    );
    assert_eq!(
        state.session_status.as_deref(),
        Some("Queued message is no longer pending")
    );
}

#[test]
fn remote_edit_queued_message_falls_back_to_bare_command_on_v1_daemon() {
    let client = Arc::new(RecordingDaemonClient::default());
    let mut state = queued_state();
    state.daemon_client = client.clone();

    // A version-1 daemon cannot decode the CommandRequest envelope; the
    // edit degrades to the bare CancelQueuedMessage variant the older
    // daemon still dispatches — the honest best available there.
    let (text, images) = state
        .edit_queued_message("entry-1")
        .expect("remote edit succeeds");
    assert_eq!(text, "draft text");
    assert!(images.is_empty());
    assert!(state.pending_queued_cancels.is_empty());
    assert!(
        state
            .messages
            .iter()
            .all(|message| message.id != "queued-user-sess-1-entry-1")
    );
    assert_eq!(
        state.session_status.as_deref(),
        Some("Queued message removed")
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while client.commands.lock().unwrap().is_empty() {
        assert!(std::time::Instant::now() < deadline, "command not dispatched");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let commands = client.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0],
        SessionCommand::CancelQueuedMessage { session_id, entry_id, .. }
            if session_id == "sess-1" && entry_id == "entry-1"
    ));
    drop(commands);
    assert!(client.requests.lock().unwrap().is_empty());
}

#[test]
fn journaled_cancel_from_another_client_drops_the_echo() {
    let mut state = queued_state();

    // A fire-and-forget cancel issued by another client carries no
    // request_id of ours — the journaled event is still authoritative
    // that the entry left the queue, so the retained echo goes away.
    let changed = state.drain_chat_stream(vec![SessionEvent::QueuedEntryCancelled {
        session_id: "sess-1".into(),
        entry_id: "entry-1".into(),
        request_id: None,
        text: "draft text".into(),
        images: Vec::new(),
    }]);
    assert!(changed);
    assert!(
        state
            .messages
            .iter()
            .all(|message| message.id != "queued-user-sess-1-entry-1")
    );
}

// ---- session snooze ---------------------------------------------------------

fn snooze_confirm_writes(state: &mut AppState, work_dir: &Path, session_id: &str) {
    for _ in 0..200 {
        state.drain_chat_stream(Vec::new());
        if state
            .session_snooze(work_dir, session_id)
            .is_some_and(|snooze| !snooze.pending)
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("snooze write was never confirmed");
}

fn settled_project_session(name: &str) -> (tempfile::TempDir, PathBuf, SessionInfo) {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let session_file = project.join(".threadlane/sessions/session.jsonl");
    let mut session = test_session("session", &session_file);
    session.title = name.into();
    session.work_dir = project.clone();
    session.runtime_work_dir = project.clone();
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(3));
    (temp, project, session)
}

fn state_with_session(session: &SessionInfo) -> AppState {
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "project".into(),
        work_dir: session.work_dir.clone(),
        sessions: vec![session.clone()],
        is_expanded: true,
    });
    state
}

#[test]
fn snooze_records_deadline_and_stays_pending_until_confirmed() {
    let (_temp, project, session) = settled_project_session("session");
    let mut state = state_with_session(&session);

    state
        .snooze_session(&project, "session", 3_600)
        .expect("settled session is eligible");
    let snooze = state
        .session_snooze(&project, "session")
        .expect("record exists");
    assert!(snooze.pending, "unconfirmed write never hides");
    assert!(!snooze.save_failed);
    let now = crate::session_snooze::unix_now();
    assert!(snooze.wake_at >= now + 3_590 && snooze.wake_at <= now + 3_600);

    snooze_confirm_writes(&mut state, &project, "session");
    let snooze = state.session_snooze(&project, "session").unwrap();
    assert!(!snooze.pending);
    assert!(
        project.join(".threadlane/session_snooze.json").exists(),
        "confirmed write persists the snooze file"
    );
}

#[test]
fn snooze_eligibility_gates_unsettled_and_remote_sessions() {
    let (_temp, project, session) = settled_project_session("session");
    let mut state = state_with_session(&session);

    // Loading state is not snoozable.
    let mut unknown = session.clone();
    unknown.completion_summary = SessionCompletionSummary::Unknown;
    let reason = state.session_snooze_eligibility(&unknown).unwrap_err();
    assert!(reason.contains("loading"), "{reason}");

    // A generating runtime reports Working and blocks snooze.
    let runtime = state.ensure_session_runtime(project.clone(), session.session_file.clone());
    runtime.begin_generation().unwrap();
    let reason = state.session_snooze_eligibility(&session).unwrap_err();
    assert!(reason.contains("working"), "{reason}");
    assert!(state
        .snooze_session(&project, "session", 3_600)
        .is_err());
    drop(runtime);

    // Remote daemon mode is local-only for this release.
    let mut remote = state_with_session(&session);
    remote.daemon_remote = true;
    let reason = remote.session_snooze_eligibility(&session).unwrap_err();
    assert!(reason.contains("local"), "{reason}");

    // Unknown project/session identity is refused outright.
    assert!(state
        .snooze_session(&project, "nobody", 3_600)
        .is_err());
}

#[test]
fn unsnooze_returns_immediately_even_before_the_write_confirms() {
    let (_temp, project, session) = settled_project_session("session");
    let mut state = state_with_session(&session);

    state.snooze_session(&project, "session", 3_600).unwrap();
    assert!(state.session_snooze(&project, "session").is_some());

    state.unsnooze_session(&project, "session");
    assert!(state.session_snooze(&project, "session").is_none());
    // Unsnooze does not resurrect through the write pipeline: let the
    // submitted delete flush and confirm, then the row stays un-snoozed.
    for _ in 0..200 {
        state.drain_chat_stream(Vec::new());
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(state.session_snooze(&project, "session").is_none());
}

#[test]
fn a_newer_completion_or_attention_ends_the_snooze() {
    let (_temp, project, session) = settled_project_session("session");
    let mut state = state_with_session(&session);

    state.snooze_session(&project, "session", 3_600).unwrap();
    snooze_confirm_writes(&mut state, &project, "session");

    // A completion newer than the captured baseline ends the snooze.
    let mut newer = session.clone();
    newer.completion_summary = SessionCompletionSummary::Latest(run_completion(9));
    assert!(state.apply_session_refresh(project.clone(), vec![newer], 0));
    assert!(state.session_snooze(&project, "session").is_none());
}

#[test]
fn transient_discovery_absence_never_drops_the_record() {
    let (_temp, project, session) = settled_project_session("session");
    let mut state = state_with_session(&session);

    state.snooze_session(&project, "session", 3_600).unwrap();
    snooze_confirm_writes(&mut state, &project, "session");

    // Discovery briefly returning nothing must not unsnooze — only
    // confirmed archive/delete prunes the record.
    assert!(state.apply_session_refresh(project.clone(), Vec::new(), 0));
    assert!(state.session_snooze(&project, "session").is_some());
}

#[test]
fn an_expired_record_never_hides_and_reconcile_cleans_up() {
    let (_temp, project, session) = settled_project_session("session");
    let mut state = state_with_session(&session);

    // Duration zero lands an already-past deadline: the row is never
    // hidden (fail-open) and reconcile drops the record.
    state.snooze_session(&project, "session", 0).unwrap();
    assert!(state.session_snooze(&project, "session").is_none());
    assert!(!state.session_snooze_entries().is_empty());
    assert!(state.reconcile_session_snoozes());
    assert!(state.session_snooze_entries().is_empty());
}

#[test]
fn a_failed_save_keeps_the_row_visible_and_reports_an_error() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let session_file = project.join(".threadlane/sessions/session.jsonl");
    let mut session = test_session("session", &session_file);
    session.work_dir = project.clone();
    session.runtime_work_dir = project.clone();
    session.completion_summary = SessionCompletionSummary::Latest(run_completion(3));
    std::fs::create_dir_all(&project).unwrap();
    // A regular file where the store directory belongs makes every write
    // fail — the record stays pending and the app reports one error.
    std::fs::write(project.join(".threadlane"), "not a directory").unwrap();

    let mut state = state_with_session(&session);
    state.snooze_session(&project, "session", 3_600).unwrap();

    let mut saw_failure = false;
    for _ in 0..200 {
        state.drain_chat_stream(Vec::new());
        if state
            .session_snooze(&project, "session")
            .is_some_and(|snooze| snooze.pending && snooze.save_failed)
        {
            saw_failure = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(saw_failure, "save failure must surface on the row");
    let status = state.session_status.clone().unwrap_or_default();
    assert!(status.contains("snooze"), "{status}");
}

#[test]
fn project_removal_rejects_remote_workspace_without_mutation() {
    let mut state = AppState::load_from_registry(Vec::new());
    let project = PathBuf::from("/remote/project");
    state.projects.push(ProjectInfo {
        name: "Remote".into(),
        work_dir: project.clone(),
        sessions: Vec::new(),
        is_expanded: true,
    });
    state.daemon_remote = true;
    assert!(state
        .remove_project(&project)
        .unwrap_err()
        .contains("hosting"));
    assert_eq!(state.projects.len(), 1);
}

#[test]
fn project_removal_rechecks_active_worktree_setup() {
    let mut state = AppState::load_from_registry(Vec::new());
    let root = tempfile::tempdir().unwrap();
    let project = root.path().to_path_buf();
    let session_file = project.join(".threadlane/sessions/setup.jsonl");
    state.projects.push(ProjectInfo {
        name: "Project".into(),
        work_dir: project.clone(),
        sessions: Vec::new(),
        is_expanded: true,
    });
    assert!(state.project_removal_disabled_reason(&project).is_none());
    state.worktree_setups.insert(
        "setup".into(),
        crate::worktree_setup::WorktreeSetup {
            session_id: "setup".into(),
            session_file,
            project: project.clone(),
            worktree: project.join("worktree"),
            base: "main".into(),
            branch: None,
            stage: crate::worktree_setup::SetupStage::Naming,
            error: None,
            text: "Task".into(),
            images: Vec::new(),
            model: "test".into(),
            effort: ReasoningEffort::default(),
            acp_config: Vec::new(),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );
    assert!(state
        .remove_project(&project)
        .unwrap_err()
        .contains("active work"));
    assert_eq!(state.projects.len(), 1);
}

#[test]
fn project_removal_blocks_live_runtime_after_discovery_loses_sessions() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().to_path_buf();
    let file = project.join(".threadlane/sessions/live.jsonl");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let mut state = AppState::load_from_registry(Vec::new());
    state.projects.push(ProjectInfo {
        name: "Project".into(),
        work_dir: project.clone(),
        sessions: vec![test_session("live", &file)],
        is_expanded: true,
    });
    let runtime = state.ensure_session_runtime(project.clone(), file);
    runtime.begin_generation().unwrap();
    state.projects[0].sessions.clear();
    assert!(state.remove_project(&project).is_err());
    assert_eq!(state.projects.len(), 1);
    runtime.finish_generation(None);
    state
        .client
        .pending_permissions
        .insert("live".into(), permission_request("permission"));
    assert!(state.remove_project(&project).is_err());
    state.client.pending_permissions.clear();
    state
        .client
        .pending_questions
        .insert("live".into(), question_request("question"));
    assert!(state.remove_project(&project).is_err());
    state.client.pending_questions.clear();
    assert!(state.project_removal_disabled_reason(&project).is_none());
}
