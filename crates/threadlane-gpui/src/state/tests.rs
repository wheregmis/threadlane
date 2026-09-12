use super::*;
use crate::services::sessions::SessionRuntimeStatus;
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use threadlane_session::coding_agent::harness::CodingSessionHarness;
use threadlane_session::harness::{
    OperationIntent, OperationOutcome, ProviderOutcome, Record, SessionStore, TraceString,
};

#[test]
fn filesystem_root_is_not_an_attachable_project() {
    assert!(!app_state::is_attachable_project_root(Path::new("/")));
    assert!(app_state::is_attachable_project_root(Path::new("/project")));
}

#[test]
fn model_selection_resets_unsupported_reasoning_and_rejects_hidden_efforts() {
    use threadlane_runtime::ReasoningEffort;
    let mut state = AppState::load_from_registry(Vec::new());
    state.available_models = vec![crate::model_catalog::ModelOption {
        id: "gpt-4o".into(),
        label: "GPT-4o".into(),
        provider: crate::model_catalog::ModelProvider::OpenAi,
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
    });
    state.active_session_id = Some("session".into());

    assert_eq!(state.active_git_work_dir(), Some(worktree.clone()));

    state.projects[0].sessions[0].worktree_available = false;

    assert_eq!(state.active_git_work_dir(), None);

    state.projects[0].sessions.clear();
    state.active_session_id = Some("missing-session".into());

    assert_eq!(state.active_git_work_dir(), None);
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
        }],
        is_expanded: true,
    }];
    state.active_work_dir = Some(project);
    state.active_session_id = Some("session".into());

    state.request_open_file("src/lib.rs".into());

    assert_eq!(
        state.requested_editor_target,
        Some(RequestedEditorTarget::File {
            project: worktree,
            path: "src/lib.rs".into(),
        })
    );
}

fn take_stream_events(state: &mut AppState, limit: usize) -> Vec<ChatStreamEvent> {
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
        let options = || threadlane_session::CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: temp.path().into(),
            session_file: Some(session_file.clone()),
            system_prompt: Default::default(),
            agent_config: None,
            coding_config: None,
            browser: threadlane_session::BrowserBridge::unavailable(),
        };
        let mut original = threadlane_session::CodingAgent::new(options());
        original.set_fact("model", "gpt-4o").unwrap();
        drop(original);
        let mut state = AppState::load_from_registry(Vec::new());
        activate_test_session(&mut state, "session", &session_file);
        // Before hydration the picker can still show another session's
        // selection. Clicking it must update this session's stored model.
        state.selected_model = if has_runtime { "gpt-4o" } else { selected }.into();
        state.available_models = vec![crate::model_catalog::ModelOption {
            id: selected.into(),
            label: "MiniMax M2.7".into(),
            provider: crate::model_catalog::ModelProvider::OpenCode,
        }];
        if has_runtime {
            state.active_session_runtime().unwrap();
        }

        state.set_selected_model(selected.into());
        state.set_reasoning_effort(ReasoningEffort::High);

        assert_eq!(state.selected_model, selected);
        assert_eq!(state.session_runtimes[&session_file].model(), selected);
        assert_eq!(
            state.session_runtimes[&session_file].reasoning_effort(),
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
        let mut restored = threadlane_session::test_support::coding_agent_with_provider(
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
    state.available_models = vec![crate::model_catalog::ModelOption {
        id: "opencode-go/minimax-m2.7".into(),
        label: "MiniMax M2.7".into(),
        provider: crate::model_catalog::ModelProvider::OpenCode,
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
        &state.session_runtimes[&session_file],
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
        state.pending_acp_config.get("opencode2").and_then(|m| m.get("model")).map(String::as_str),
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
    state.session_runtimes.remove(&file_a);
    let (current, _) = state.active_session_runtime().unwrap();
    let reply = |runtime: &Arc<SessionRuntime>, label: &str, error: Option<&str>| {
        ChatStreamEvent::AcpConfigOptions {
            session_id: "session".into(),
            source: Arc::downgrade(runtime),
            options: vec![
                serde_json::from_value(serde_json::json!({
                    "id": "model", "name": "Model", "category": "model",
                    "currentValue": "model", "options": [{ "value": "model", "name": label }]
                }))
                .unwrap(),
            ],
            error: error.map(str::to_string),
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

fn permission_request(id: &str) -> threadlane_session::PermissionRequest {
    threadlane_session::PermissionRequest {
        id: id.into(),
        capability: "network".into(),
        title: "Connect to api.example.test".into(),
        detail: "https://api.example.test".into(),
        scopes: vec![threadlane_session::PermissionScope::Once],
    }
}

fn question_request(id: &str) -> threadlane_session::QuestionRequest {
    threadlane_session::QuestionRequest {
        id: id.into(),
        questions: vec![threadlane_session::QuestionItem {
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
    }
}

#[test]
fn inactive_permission_is_visible_before_session_selection() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.active_session_id = Some("foreground".into());
    let session = test_session("background", Path::new("/project/background.jsonl"));
    let request = permission_request("permission-1");

    let changed = state.drain_chat_stream(vec![ChatStreamEvent::Agent {
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
        ChatStreamEvent::Agent {
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
    assert!(state.drain_chat_stream(vec![ChatStreamEvent::Agent {
        session_id: session.id.clone(),
        event: AgentEvent::PermissionRequested { request },
    }]));

    let changed = state.drain_chat_stream(vec![ChatStreamEvent::Finished {
        session_id: session.id.clone(),
        session_file: session.session_file.clone(),
    }]);

    assert!(changed);
    assert!(!state.pending_permissions.contains_key(&session.id));
    assert_eq!(state.session_attention(&session), SessionAttention::Idle);
    let deferred = &state.deferred_stream_events[&session.id];
    assert_eq!(deferred.len(), 2);
    assert!(matches!(deferred[0], ChatStreamEvent::Agent { .. }));
    assert!(matches!(deferred[1], ChatStreamEvent::Finished { .. }));
}

#[test]
fn active_question_stays_pending_until_answered() {
    let mut state = AppState::load_from_registry(Vec::new());
    let session = test_session("active", Path::new("/project/active.jsonl"));
    state.active_session_id = Some(session.id.clone());
    state.active_work_dir = Some(Path::new("/project").to_path_buf());
    let request = question_request("question-1");

    let changed = state.drain_chat_stream(vec![ChatStreamEvent::Agent {
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
    let changed = state.drain_chat_stream(vec![ChatStreamEvent::Finished {
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
    assert_eq!(state.session_attention(&historical), SessionAttention::Ready);
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

    assert!(state.drain_chat_stream(vec![ChatStreamEvent::Agent {
        session_id: session.id.clone(),
        event: AgentEvent::AgentStart,
    }]));
    assert_eq!(state.session_attention(&session), SessionAttention::Working);

    runtime.finish_generation(Some("provider failed".into()));
    assert!(state.drain_chat_stream(vec![ChatStreamEvent::Agent {
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
    state.active_session_id = Some(session.id);
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
    assert_eq!(
        state.session_status.as_deref(),
        Some("Generation cancelled")
    );
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
        vec![ChatStreamEvent::Finished {
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
    let project = dir.path().canonicalize().unwrap();
    run_git(&project, &["init", "-b", "main"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "Test"]);
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
    let project = dir.path().canonicalize().unwrap();
    run_git(&project, &["init", "-b", "main"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "Test"]);
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
    let project = dir.path().canonicalize().unwrap();
    run_git(&project, &["init", "-b", "main"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "Test"]);
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
    let project = dir.path().canonicalize().unwrap();
    run_git(&project, &["init", "-b", "main"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "Test"]);
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
        .append_entry(threadlane_session::harness::Entry {
            id: "message-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: transcript.next_sequence(),
            timestamp: 1,
            message: AgentMessage::user("Persisted history", Vec::new()),
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
    run_git(repo.path(), &["init", "-b", "main"]);
    run_git(repo.path(), &["config", "user.email", "test@example.com"]);
    run_git(repo.path(), &["config", "user.name", "Test"]);
    std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    run_git(repo.path(), &["add", "."]);
    run_git(repo.path(), &["commit", "-m", "initial"]);

    let work_dir = repo.path().canonicalize().unwrap();
    let issue = issue_ref(42);
    let mut state = issue_work_state(&work_dir);
    let session_id = state
        .start_issue_work(work_dir.clone(), issue.clone(), "Fix flaky auth!".into())
        .unwrap();

    let session_file = work_dir
        .join(".threadlane/sessions")
        .join(format!("{session_id}.jsonl"));
    let facts = JsonlStore::open_read_only(&session_file).unwrap().facts();
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
        .start_issue_work(work_dir.clone(), issue_ref(9), "Unborn".into())
        .unwrap_err();

    assert!(!error.is_empty());
    assert!(state.active_session_id.is_none());
    assert!(state.projects[0].sessions.is_empty());
    assert!(state.session_runtimes.is_empty());
    assert!(!work_dir.join(".threadlane/sessions").exists());
}

#[test]
fn issue_work_prompt_failure_rolls_back_artifacts_and_selection() {
    let repo = tempfile::tempdir().unwrap();
    run_git(repo.path(), &["init", "-b", "main"]);
    run_git(repo.path(), &["config", "user.email", "test@example.com"]);
    run_git(repo.path(), &["config", "user.name", "Test"]);
    std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    run_git(repo.path(), &["add", "."]);
    run_git(repo.path(), &["commit", "-m", "initial"]);

    let work_dir = repo.path().canonicalize().unwrap();
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
    let persisted_before = threadlane_project::load_project_registry()
        .into_iter()
        .find(|project| project.path == work_dir)
        .map(|project| (project.last_session_id, project.last_opened_at));

    let error = state
        .start_issue_work_with_prompt(
            work_dir.clone(),
            issue_ref(77),
            "Prompt failure".into(),
            |_, prompt| {
                assert!(prompt.contains("call create_draft_pull_request"));
                assert!(prompt.contains("publish the issue branch to origin"));
                assert!(prompt.contains("credential-aware tool"));
                assert!(!prompt.contains("Do not push or publish anything"));
                Err("prompt acceptance failed".into())
            },
        )
        .unwrap_err();

    assert_eq!(error, "prompt acceptance failed");
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

struct ReportedShapeProvider {
    attempts: AtomicUsize,
    previous_serialized_request: Mutex<Option<String>>,
}

#[async_trait::async_trait]
impl threadlane_protocol::ProviderPort for ReportedShapeProvider {
    async fn stream_request(
        &self,
        request: threadlane_protocol::RuntimeRequest,
        events: tokio::sync::mpsc::Sender<threadlane_protocol::RuntimeStreamEvent>,
    ) {
        use threadlane_protocol::{
            RuntimeStreamEvent, RuntimeToolCall, RuntimeToolCallFunction, RuntimeUsage,
        };

        let serialized_request = format!("{}\n{}", request.messages, request.tools);
        let estimate = serialized_request.len().div_ceil(4);
        let cache_read_tokens = {
            let mut previous = self.previous_serialized_request.lock().unwrap();
            let repeated_prefix_bytes = previous
                .as_ref()
                .map(|prior| {
                    prior
                        .bytes()
                        .zip(serialized_request.bytes())
                        .take_while(|(left, right)| left == right)
                        .count()
                })
                .unwrap_or(0);
            *previous = Some(serialized_request);
            repeated_prefix_bytes / 4
        };
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        let tool_calls = (attempt < 102)
            .then(|| RuntimeToolCall {
                id: format!("loop-{attempt}"),
                r#type: "function".into(),
                function: RuntimeToolCallFunction {
                    name: threadlane_skills::LOAD_SKILL_TOOL_NAME.into(),
                    arguments: serde_json::json!({ "name": "reported-shape" }).to_string(),
                },
                thought_signature: None,
            })
            .into_iter()
            .collect();
        if attempt == 102 {
            events
                .send(RuntimeStreamEvent::ContentToken("complete".into()))
                .await
                .unwrap();
        }
        let input_tokens = u32::try_from(estimate.saturating_sub(cache_read_tokens)).unwrap();
        let cache_read_tokens = u32::try_from(cache_read_tokens).unwrap();
        events
            .send(RuntimeStreamEvent::Finished {
                tool_calls,
                usage: RuntimeUsage {
                    input_tokens,
                    output_tokens: if attempt == 102 { 1 } else { 20 },
                    cache_read_tokens,
                    cache_write_tokens: 0,
                    total_tokens: u32::try_from(estimate).unwrap()
                        + if attempt == 102 { 1 } else { 20 },
                },
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

async fn generated_reported_session_path() -> PathBuf {
    use threadlane_runtime::AgentConfig;
    use threadlane_session::SystemPromptConfig;
    use threadlane_session::coding_agent::CodingAgentOptions;

    let root = tempfile::tempdir().unwrap().keep();
    let skill_dir = root.join(".agents/skills/reported-shape");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: reported-shape\ndescription: deterministic compaction input\n---\n{}",
            "segment ".repeat(1_000)
        ),
    )
    .unwrap();
    let path = root.join("reported-session-shape.jsonl");
    let provider = Arc::new(ReportedShapeProvider {
        attempts: AtomicUsize::new(0),
        previous_serialized_request: Mutex::new(None),
    });
    let mut agent = threadlane_session::test_support::coding_agent_with_provider(
        CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: root,
            session_file: Some(path.clone()),
            system_prompt: SystemPromptConfig::default(),
            agent_config: Some(AgentConfig::default()),
            coding_config: None,
            browser: threadlane_session::BrowserBridge::unavailable(),
        },
        provider.clone(),
    );
    let result = agent
        .handle_input_with_images("continue the cached tool loop", vec![])
        .await;
    assert!(result.is_none(), "foreground run failed: {result:?}");
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 102);
    drop(agent);
    path
}

pub(crate) async fn reported_session_shape_state() -> (PathBuf, AppState) {
    let path = generated_reported_session_path().await;
    // This is the production GPUI projection reading the journal emitted above by CodingAgent.
    let projection = compute_full_session_projection(&path).unwrap();
    let mut state = AppState::load_from_registry(Vec::new());
    activate_test_session(&mut state, "context-session", &path);
    state.apply_session_hydration("context-session", &path, projection);
    (path, state)
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
    use threadlane_session::harness::{CompactionReason, TranscriptItem, read_transcript_page};

    let store = JsonlStore::open(&path).unwrap();
    let records = store.records();
    let provider_starts = records
        .iter()
        .filter_map(|record| match record {
            Record::ProviderRequestStarted { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(provider_starts.len(), 102);

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
    let expected_loop_ids = (1..=101)
        .map(|index| format!("loop-{index}"))
        .collect::<Vec<_>>();
    assert_eq!(call_ids, expected_loop_ids);
    assert_eq!(result_ids, expected_loop_ids);
    assert_eq!(call_positions.len(), 101);
    assert_eq!(result_positions.len(), 101);
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
    use threadlane_session::harness::UsageCause;

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
    let mut state = AppState::load_from_registry(Vec::new());
    activate_test_session(&mut state, "same-session", &file_a);
    state.apply_session_hydration("same-session", &file_a, projection_a);
    activate_test_session(&mut state, "same-session", &file_b);
    state.apply_session_hydration("same-session", &file_b, projection_b);

    assert_eq!(state.context_windows.len(), 2);
    assert_eq!(state.session_metrics.len(), 2);
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
    use threadlane_session::harness::CompactionReason;

    let path = generated_reported_session_path().await;
    let mut store = JsonlStore::open(&path).unwrap();
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
            generation: 4,
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
    assert_eq!(context.compaction_generation, 4);
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

fn activate_test_session(state: &mut AppState, session_id: &str, session_file: &Path) {
    let work_dir = session_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    state.projects.push(ProjectInfo {
        name: session_id.into(),
        work_dir: work_dir.clone(),
        sessions: vec![SessionInfo {
            id: session_id.into(),
            title: session_id.into(),
            work_dir: work_dir.clone(),
            runtime_work_dir: work_dir.clone(),
            session_file: session_file.to_path_buf(),
            updated_at: 0,
            health: SessionHealth::Healthy,
            git_branch: None,
            github_issue: None,
            is_worktree: false,
            worktree_available: true,
        }],
        is_expanded: true,
    });
    state.active_work_dir = Some(work_dir);
    state.active_session_id = Some(session_id.into());
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
        let messages = compute_session_messages(&request.session_file).unwrap();
        state.apply_session_messages(&request.session_id, &request.session_file, messages);
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
    let mut store = threadlane_session::harness::JsonlStore::open(&session_file).unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
            id: "node_1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::user("recent prompt", Vec::new()),
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
    assert!(state.session_runtimes.is_empty());
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
        if let Ok(refresh) = receiver.try_recv() {
            break refresh;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "startup session refresh was never queued"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(state.apply_session_refresh(work_dir, sessions));
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
    use threadlane_session::harness::{
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
    let mut store = threadlane_session::harness::JsonlStore::open(&session_file).unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
            id: "node_1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::User {
                content: "Inspect the project".into(),
            },
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
            id: "node_2".into(),
            parent_id: Some("node_1".into()),
            lane: "main".into(),
            seq: 2,
            timestamp: 2,
            message: AgentMessage::Custom {
                custom_type: "thinking".into(),
                payload: serde_json::json!({"text": "Reading the relevant files"}),
            },
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-1".into(),
            seq: 99,
            lane: "main".into(),
            timestamp: 99,
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
    use threadlane_session::harness::{
        Entry, OperationIntent, OperationOutcome, Record, SessionStore, ToolReplaySafety,
        UsageCause,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, "").unwrap();
    let mut store = threadlane_session::harness::JsonlStore::open(&path).unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-1".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
    use threadlane_session::harness::{Entry, SurfaceOperation};

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
        ChatStreamEvent::Agent {
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
        ChatStreamEvent::Agent {
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
        ChatStreamEvent::Agent {
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
    let mut store = threadlane_session::harness::JsonlStore::open(&session_file).unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
            ChatStreamEvent::Agent {
                session_id: session_id.clone(),
                event: AgentEvent::TurnStart { turn_number: 2 },
            },
            ChatStreamEvent::Finished {
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
        ChatStreamEvent::Agent {
            session_id: session.id.clone(),
            event: AgentEvent::AgentError {
                error: "background failed".into(),
            },
        },
        ChatStreamEvent::Finished {
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
    use threadlane_session::harness::SessionStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, "").unwrap();
    let store = threadlane_session::harness::JsonlStore::open(&path).unwrap();
    let mut harness = threadlane_session::harness::AgentHarness::new(store);
    harness
        .accept_prompt("run-1", AgentMessage::user("old prompt", vec![]))
        .unwrap();
    harness.drive_to_completion().unwrap();
    let parent_id = harness.store().entries().last().unwrap().id.clone();
    let seq = harness.store().next_sequence();
    harness
        .store_mut()
        .append_entry(threadlane_session::harness::Entry {
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
            .send(ChatStreamEvent::Agent {
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
#[test]
fn stream_drain_preserves_events_beyond_one_frame_budget() {
    let mut state = AppState::load_from_registry(Vec::new());
    state.messages_mut().clear();
    state.active_work_dir = Some(std::env::temp_dir().join("threadlane-stream-budget"));
    state.active_session_id = Some("session".into());
    state.is_new_task = false;

    for index in 0..130 {
        state
            .stream_tx
            .send(ChatStreamEvent::Agent {
                session_id: "session".into(),
                event: AgentEvent::MessageUpdate {
                    text_delta: Some(format!("{index},")),
                    reasoning_delta: None,
                    tool_call_name: None,
                },
            })
            .unwrap();
    }

    let events = take_stream_events(&mut state, 128);
    assert!(state.drain_chat_stream(events));
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0].content.matches(',').count(), 128);
    let events = take_stream_events(&mut state, 128);
    assert_eq!(events.len(), 2);
    assert!(state.drain_chat_stream(events));
    assert_eq!(state.messages[0].content.matches(',').count(), 130);
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
    let mut store = threadlane_session::harness::JsonlStore::open(&path).unwrap();
    let mut parent_id = None;
    for index in 0..MESSAGE_COUNT {
        let id = format!("node_{index}");
        store
            .append_entry(threadlane_session::harness::Entry {
                id: id.clone(),
                parent_id,
                lane: "main".into(),
                seq: (index + 1) as u64,
                timestamp: (index + 1) as u64,
                message: AgentMessage::User {
                    content: format!("message-{index}"),
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        parent_id = Some(id);
    }
    drop(store);

    // Keep the fixture tied to the regression: the former GPUI helper loaded
    // only this newest page, omitting the first five durable messages.
    let legacy_page =
        threadlane_session::harness::read_transcript_page(&path, None, LEGACY_PAGE_SIZE).unwrap();
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
    let mut store = threadlane_session::harness::JsonlStore::open(&session_file).unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
            id: "node_1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::User {
                content: "Hello on startup".into(),
            },
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-start-1".into(),
            seq: 10,
            lane: "main".into(),
            timestamp: 10,
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

    let mut store = threadlane_session::harness::JsonlStore::open(&path).unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
            id: "msg-root".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            message: AgentMessage::User {
                content: "Root question".into(),
            },
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_entry(threadlane_session::harness::Entry {
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
            surface_op: threadlane_session::harness::SurfaceOperation::Append,
            terminate: false,
        })
        .unwrap();
    store
        .append_record(Record::OperationStarted {
            id: "run-branch-a".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
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

fn computer_permission_request(id: &str) -> threadlane_session::PermissionRequest {
    threadlane_session::PermissionRequest {
        id: id.into(),
        capability: "computer".into(),
        title: "Click at (1, 1)".into(),
        detail: "click".into(),
        scopes: vec![threadlane_session::PermissionScope::Once],
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
            is_expanded: false,
        }],
        streaming: true,
        reasoning_content: None,
        reasoning_expanded: false,
    });
    assert!(state.take_computer_mirror_trigger());
    assert!(
        !state.take_computer_mirror_trigger(),
        "same tool call must not retrigger"
    );
}
