//! Offline coverage of agy's plugin catalog and session handling.
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
    let request = RunRequest {
        working_dir: dir.path().into(),
        prompt: "private follow-up".into(),
        images: Vec::new(),
        skills: Vec::new(),
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
        single_use: false,
    };
    let mut run = backend.run(request).await.unwrap();
    assert_eq!(run.next_event().await, Some(BackendEvent::SessionMissing));
    assert_eq!(run.next_event().await, None);
    assert!(!dir.path().join("delivered").exists());
}

#[tokio::test]
async fn an_antigravity_task_requests_enabled_plugin_skills_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("agy");
    std::fs::write(
        &program,
        r#"#!/bin/sh
if [ "$*" = 'plugin list --json' ]; then
    cat <<'JSON'
[
    {"name":"tools","enabled":true,"skills":["verify","review","shared","invalid name"]},
    {"name":"disabled","enabled":false,"skills":["hidden","review"]},
    {"name":"extras","enabled":true,"skills":["shared"]},
    {"name":"other","enabled":true}
]
JSON
    exit
fi
printf '%s\n' "$@" > args
echo '{"event":"init","conversation_id":"run","init":{}}'
read -r prompt || exit 1
printf '%s\n' "$prompt" > delivered
echo '{"event":"result","result":{"status":"SUCCESS"}}'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let backend = AntigravityBackend::new(program);
    assert_eq!(
        backend.installed_skills(None).await.unwrap(),
        ["review", "verify"]
    );

    let mut request = RunRequest {
        working_dir: dir.path().into(),
        prompt: "/usage is part of the task text".into(),
        images: Vec::new(),
        skills: vec!["review".into(), "verify".into()],
        appended_instructions: String::new(),
        model: None,
        session: NativeSession::Start { id: String::new() },
        tools: None,
        remove_builtin_tools: true,
        deny_rules: Vec::new(),
        wall_time_limit: Duration::from_secs(5),
        max_turns: Some(1),
        single_use: false,
    };
    assert!(
        matches!(backend.run(request.clone()).await, Err(error) if error.to_string().contains("require native tools"))
    );
    assert!(!dir.path().join("args").exists());
    request.remove_builtin_tools = false;
    request.skills.push("hidden".into());

    let mut run = backend.run(request).await.unwrap();
    assert_eq!(
        run.next_event().await,
        Some(BackendEvent::Warning {
            message: "antigravity skill \"hidden\" was skipped for this run: it is not installed on this runner.".into()
        })
    );
    assert!(matches!(
        run.next_event().await,
        Some(BackendEvent::SessionStarted { .. })
    ));
    assert!(matches!(
        run.next_event().await,
        Some(BackendEvent::Finished { .. })
    ));
    let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
    assert!(args.lines().any(|arg| arg == "--disable-slash-commands"));
    let delivered = std::fs::read(dir.path().join("delivered")).unwrap();
    let delivered: serde_json::Value = serde_json::from_slice(&delivered).unwrap();
    assert_eq!(
        delivered["message"]["content"],
        "Use these installed skills for this run: review verify\n\n/usage is part of the task text"
    );
}
