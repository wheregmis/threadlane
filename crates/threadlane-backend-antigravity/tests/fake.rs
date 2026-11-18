//! Offline coverage of agy's silent fallback when a saved conversation disappears.
#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, time::Duration};
use threadlane_backend_antigravity::AntigravityBackend;
use threadlane_core::backend::{AgentBackend, BackendEvent, NativeSession, RunRequest};

#[tokio::test]
async fn missing_conversation_never_receives_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("agy");
    std::fs::write(
        &program,
        r#"#!/bin/sh
echo '{"event":"init","conversation_id":"replacement","init":{}}'
if read -r prompt; then touch delivered; fi
"#,
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let backend = AntigravityBackend::new(program);
    let mut run = backend
        .run(RunRequest {
            working_dir: dir.path().into(),
            prompt: "private follow-up".into(),
            skills: Vec::new(),
            images: Vec::new(),
            appended_instructions: String::new(),
            model: None,
            session: NativeSession::Resume {
                id: "original".into(),
            },
            tools: None,
            remove_builtin_tools: true,
            deny_rules: Vec::new(),
            wall_time_limit: Duration::from_secs(5),
            max_turns: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(run.next_event().await, Some(BackendEvent::SessionMissing));
    assert_eq!(run.next_event().await, None);
    assert!(!dir.path().join("delivered").exists());
}
