use std::sync::Arc;
use threadlane_daemon::{DaemonServer, RpcDispatcher};
use threadlane_protocol::client::DaemonClient;
use threadlane_protocol::permission::*;
use threadlane_protocol::session::*;
use threadlane_protocol::terminal::*;

#[tokio::test]
async fn test_end_to_end_client_daemon_uds() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let socket_path = temp_dir.path().join("test_daemon.sock");
    let project_path = temp_dir.path().join("test_project");
    std::fs::create_dir_all(&project_path).expect("create project dir");

    // 1. Start Daemon Server on temporary UDS
    let dispatcher = Arc::new(RpcDispatcher::new());
    let server = DaemonServer::new(dispatcher);
    server
        .serve_uds(socket_path.clone())
        .await
        .expect("serve UDS");

    // Give the server a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // 2. Connect Daemon Client
    let client = DaemonClient::connect_uds(&socket_path)
        .await
        .expect("connect client");

    // 3. Test daemon info
    let info = client.get_daemon_info().await.expect("get daemon info");
    assert_eq!(info.protocol_version, "2.0");

    // 4. Test list models
    let models = client.list_models().await.expect("list models");
    assert!(!models.models.is_empty());

    // 5. Test register project
    let project_str = project_path.to_string_lossy().to_string();
    let canonical_project_str = project_path
        .canonicalize()
        .expect("canonicalize")
        .to_string_lossy()
        .to_string();
    let project_record = client
        .register_project(&project_str)
        .await
        .expect("register project");
    assert_eq!(project_record.path, canonical_project_str);

    // 6. Test create session
    let session = client
        .create_session(CreateSessionRequest {
            project_path: project_str.clone(),
            session_id: Some("session_integration_1".to_string()),
            model: Some("antigravity/gemini-3.7-flash".to_string()),
            title: Some("Integration Session".to_string()),
        })
        .await
        .expect("create session");
    assert_eq!(session.session_id, "session_integration_1");

    // 7. Test list sessions
    let sessions = client
        .list_sessions(&project_str)
        .await
        .expect("list sessions");
    assert!(sessions.iter().any(|s| s.session_id == "session_integration_1"));

    // 8. Test terminal spawn and write
    let term = client
        .spawn_terminal(SpawnTerminalRequest {
            project_path: project_str.clone(),
            terminal_id: Some("term_integration_1".to_string()),
            cols: 80,
            rows: 24,
        })
        .await
        .expect("spawn terminal");
    assert_eq!(term.terminal_id, "term_integration_1");

    client
        .write_terminal_input("term_integration_1", "echo 'hello from client'\n")
        .await
        .expect("write terminal input");

    // 9. Test permission submission handling
    let perm_res = client
        .submit_permission(SubmitPermissionRequest {
            request_id: "non_existent_perm_id".to_string(),
            decision: PermissionDecision::Allow {
                scope: PermissionScope::Once,
            },
        })
        .await;
    // Expected to return error since the request_id is fictitious
    assert!(perm_res.is_err());
}
