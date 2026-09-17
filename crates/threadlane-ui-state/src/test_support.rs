use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use crate::types::{ProjectInfo, SessionHealth, SessionInfo};
use crate::{projection::compute_full_session_projection, AppState};

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

pub async fn generated_reported_session_path() -> PathBuf {
    use threadlane_runtime::AgentConfig;
    use threadlane_coding_agent::CodingAgentOptions;
    use threadlane_prompt::SystemPromptConfig;

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
    let mut agent = threadlane_coding_agent::controller::test_support::coding_agent_with_provider(
        CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: root,
            session_file: Some(path.clone()),
            system_prompt: SystemPromptConfig::default(),
            // Synthetic provider repeats one identical call 102 times; the
            // loop guard would trip at 5 and end the run early.
            agent_config: Some(
                AgentConfig::builder()
                    .loop_guard_enabled(false)
                    .build(),
            ),
            coding_config: None,
            browser: threadlane_protocol::browser::BrowserBridge::unavailable(),
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

pub async fn reported_session_shape_state() -> (PathBuf, AppState) {
    let path = generated_reported_session_path().await;
    // This is the production GPUI projection reading the journal emitted above by CodingAgent.
    let projection = compute_full_session_projection(&path).unwrap();
    let mut state = AppState::load_from_registry(Vec::new());
    activate_test_session(&mut state, "context-session", &path);
    state.apply_session_hydration("context-session", &path, projection);
    (path, state)
}

pub fn activate_test_session(state: &mut AppState, session_id: &str, session_file: &Path) {
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
