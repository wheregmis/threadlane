//! Conformance against the real `claude` (Plan.md §5.7 "Versions are tracked").
//!
//! Ignored by default: it uses the account `claude` is logged in to. Run with
//! `cargo test -p threadlane-backend-claude -- --ignored`.
//!
//! [`real_claude_passes_the_conformance_suite`] is the shared suite of
//! Plan.md §16 that `conformance.rs` runs on a fake. It is what proves that
//! each deny rule, sent as `Bash(<command>:*)`, refuses its command in the
//! real `claude`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as Json, json};
use threadlane_backend_claude::{ClaudeBackend, ClaudeConfig};
use threadlane_core::backend::{AgentBackend, BackendEvent, BackendRun, NativeSession, RunRequest};
use threadlane_core::tools::{Tier, ToolDefinition, ToolEndpoint, ToolError};
use threadlane_testkit::conformance::Conformance;

/// Bounds each run; the prompts are tiny.
const RUN_LIMIT: Duration = Duration::from_secs(180);

type Call = (Option<String>, String, Json);

#[derive(Debug, Default)]
struct Messages {
    calls: Mutex<Vec<Call>>,
}

impl Messages {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("lock poisoned").clone()
    }
}

#[async_trait]
impl ToolEndpoint for Messages {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "send_message".to_owned(),
            description: "Sends a chat message to the user.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {"text": {"type": "string", "description": "The message"}},
                "required": ["text"],
            }),
            tier: Tier::Local,
            safe_to_repeat: false,
        }]
    }

    async fn call(
        &self,
        call_id: Option<&str>,
        name: &str,
        arguments: Json,
    ) -> Result<Json, ToolError> {
        self.calls.lock().expect("lock poisoned").push((
            call_id.map(str::to_owned),
            name.to_owned(),
            arguments,
        ));
        Ok(json!({"sent": true}))
    }
}

fn request(work: &std::path::Path, prompt: &str, session: NativeSession) -> RunRequest {
    RunRequest {
        working_dir: work.to_owned(),
        prompt: prompt.to_owned(),
        images: Vec::new(),
        skills: Vec::new(),
        appended_instructions: String::new(),
        model: Some("haiku".to_owned()),
        session,
        tools: None,
        deny_rules: Vec::new(),
        remove_builtin_tools: true,
        wall_time_limit: RUN_LIMIT,
        max_turns: Some(4),
    }
}

async fn collect(run: &mut dyn BackendRun) -> Vec<BackendEvent> {
    let mut events = Vec::new();
    let collecting = async {
        while let Some(event) = run.next_event().await {
            eprintln!("event: {event:?}");
            events.push(event);
        }
    };
    tokio::time::timeout(RUN_LIMIT, collecting)
        .await
        .expect("the run ends within the limit");
    events
}

#[tokio::test]
#[ignore = "runs the real claude on the logged-in account"]
async fn real_claude_conforms() {
    let backend = ClaudeBackend::new(ClaudeConfig::default());
    let probe = backend.probe().await.expect("probe succeeds");
    eprintln!("probe: {probe:?}");
    assert!(probe.usable(), "claude must be installed and logged in");

    let work = tempfile::tempdir().expect("temp dir");
    let session = uuid::Uuid::new_v4().to_string();
    let messages = Arc::new(Messages::default());

    // A new session that uses the system's tool.
    let mut first = request(
        work.path(),
        "Call the send_message tool exactly once with text \"hi\". Then reply with the single word: done",
        NativeSession::Start {
            id: session.clone(),
        },
    );
    first.tools = Some(messages.clone());
    let mut run = backend.run(first).await.expect("run starts");
    let events = collect(run.as_mut()).await;
    drop(run);

    assert_eq!(
        events.first(),
        Some(&BackendEvent::SessionStarted {
            native_session_id: session.clone()
        }),
        "{events:#?}"
    );
    let tool_use_id = events
        .iter()
        .find_map(|event| match event {
            BackendEvent::ToolUse { call_id, name, .. } if name == "send_message" => {
                Some(call_id.clone())
            }
            _ => None,
        })
        .expect("a send_message ToolUse");
    assert!(tool_use_id.is_some(), "the ToolUse carries an id");
    let calls = messages.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, tool_use_id, "the endpoint got the ToolUse id");
    assert_eq!(calls[0].1, "send_message");
    assert!(
        events.iter().any(|event| matches!(
            event,
            BackendEvent::ToolResult { call_id, is_error: false, .. } if *call_id == tool_use_id
        )),
        "{events:#?}"
    );
    assert!(
        matches!(events.last(), Some(BackendEvent::Finished { .. })),
        "{events:#?}"
    );
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);

    // The same session, resumed.
    let mut run = backend
        .run(request(
            work.path(),
            "Reply with the single word: ok",
            NativeSession::Resume {
                id: session.clone(),
            },
        ))
        .await
        .expect("resume starts");
    let events = collect(run.as_mut()).await;
    drop(run);
    assert_eq!(
        events.first(),
        Some(&BackendEvent::SessionStarted {
            native_session_id: session.clone()
        }),
        "{events:#?}"
    );
    assert!(
        matches!(events.last(), Some(BackendEvent::Finished { .. })),
        "{events:#?}"
    );

    // Starting it again under the same id falls back to resuming it.
    let mut run = backend
        .run(request(
            work.path(),
            "Reply with the single word: again",
            NativeSession::Start {
                id: session.clone(),
            },
        ))
        .await
        .expect("restart starts");
    let events = collect(run.as_mut()).await;
    drop(run);
    assert_eq!(
        events.first(),
        Some(&BackendEvent::SessionStarted {
            native_session_id: session.clone()
        }),
        "{events:#?}"
    );
    assert!(
        matches!(events.last(), Some(BackendEvent::Finished { .. })),
        "{events:#?}"
    );

    // A session that does not exist.
    let unknown = uuid::Uuid::new_v4().to_string();
    let mut run = backend
        .run(request(
            work.path(),
            "Reply with the single word: ok",
            NativeSession::Resume { id: unknown },
        ))
        .await
        .expect("resume starts");
    let events = collect(run.as_mut()).await;
    assert_eq!(events, vec![BackendEvent::SessionMissing]);
}

#[tokio::test]
#[ignore = "runs the real claude on the logged-in account"]
async fn real_claude_passes_the_conformance_suite() {
    let suite = Conformance {
        backend: Arc::new(ClaudeBackend::new(ClaudeConfig::default())),
        model: Some("haiku".to_owned()),
        new_session_id: || uuid::Uuid::new_v4().to_string(),
        limit: RUN_LIMIT,
    };
    let work = tempfile::tempdir().expect("temp dir");
    suite.run_with_tools_then_resume(work.path()).await;
    let work = tempfile::tempdir().expect("temp dir");
    suite.each_deny_rule_refuses_its_command(work.path()).await;
    // A real usage limit cannot be provoked; the fake covers that row.
}
