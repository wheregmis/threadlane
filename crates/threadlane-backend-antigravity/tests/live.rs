use std::{sync::Arc, time::Duration};

use serde_json::json;
use threadlane_backend_antigravity::AntigravityBackend;
use threadlane_core::backend::{AgentBackend, AskRequest};
use threadlane_testkit::conformance::Conformance;

#[tokio::test]
#[ignore = "uses the signed-in Antigravity CLI"]
async fn real_agy_runs_tools_and_resumes() {
    let backend = AntigravityBackend::new("agy".into());
    assert!(backend.probe().await.unwrap().usable());
    let suite = Conformance {
        backend: Arc::new(backend),
        model: std::env::var("THREADLANE_AGY_MODEL").ok(),
        new_session_id: String::new,
        limit: Duration::from_secs(120),
    };
    let work = tempfile::tempdir().unwrap();
    suite.run_with_tools_then_resume(work.path()).await;
}

#[tokio::test]
#[ignore = "uses the signed-in Antigravity CLI"]
async fn real_agy_answers_a_structured_question() {
    let backend = AntigravityBackend::new("agy".into());
    let answer = backend.ask(AskRequest {
        question: "Return ok set to true.".into(),
        answer_schema: json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]}),
        model: None,
        timeout: Duration::from_secs(60),
    }).await.unwrap();
    assert_eq!(answer, json!({"ok":true}));
}

/// A resumed conversation must use the new run's tool endpoint, with its new lease.
#[tokio::test]
#[ignore = "uses the signed-in Antigravity CLI"]
async fn resumed_lead_calls_the_new_endpoint_with_remembered_text() {
    use threadlane_core::backend::{BackendEvent, NativeSession, RunRequest};
    use threadlane_testkit::conformance::Recorder;

    let backend = AntigravityBackend::new("agy".into());
    let work = tempfile::tempdir().unwrap();
    let marker = format!("plum-{}", uuid::Uuid::new_v4());
    let mut session = NativeSession::Start { id: String::new() };
    for (prompt, expected) in [
        (
            format!(
                "Remember {marker}. Use send_message exactly once to send just the word saved."
            ),
            "saved",
        ),
        (
            "Use send_message exactly once to send just the code I asked you to remember.".into(),
            marker.as_str(),
        ),
    ] {
        let endpoint = Arc::new(Recorder::default());
        let mut run = backend
            .run(RunRequest {
                working_dir: work.path().into(),
                prompt,
                appended_instructions: "Deliver replies with the send_message tool.".into(),
                remove_builtin_tools: true,
                tools: Some(endpoint.clone()),
                model: None,
                skills: Vec::new(),
                images: Vec::new(),
                session: session.clone(),
                deny_rules: Vec::new(),
                max_turns: None,
                wall_time_limit: Duration::from_secs(90),
            })
            .await
            .unwrap();
        let Some(BackendEvent::SessionStarted { native_session_id }) = run.next_event().await
        else {
            panic!("missing native session");
        };
        if let NativeSession::Resume { id } = &session {
            assert_eq!(&native_session_id, id);
        }
        session = NativeSession::Resume {
            id: native_session_id,
        };
        let mut terminal = None;
        while let Some(event) = run.next_event().await {
            terminal = Some(event);
        }
        assert!(
            matches!(terminal, Some(BackendEvent::Finished { .. })),
            "{terminal:?}"
        );
        let calls = endpoint.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].1, "send_message");
        assert_eq!(calls[0].2["text"], expected);
    }
}
