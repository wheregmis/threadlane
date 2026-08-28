use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use threadlane_daemon::{DaemonServer, RpcDispatcher};
use threadlane_protocol::capabilities::{GitCommitRequest, GitStageFileRequest};
use threadlane_protocol::client::DaemonClient;
use threadlane_protocol::git::{GitBranchesRequest, GitDiffRequest};
use threadlane_protocol::permission::*;
use threadlane_protocol::project::*;
use threadlane_protocol::session::*;
use threadlane_protocol::terminal::*;
use tokio::sync::broadcast;

async fn wait_for_terminal_marker(
    events: &mut broadcast::Receiver<TerminalOutputEvent>,
    terminal_id: &str,
    marker: &str,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.expect("terminal event");
            if event.terminal_id == terminal_id && event.data.contains(marker) {
                break;
            }
        }
    })
    .await
    .expect("terminal marker");
}

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

    // 6. Project file RPCs stay on the daemon host and support bounded trees.
    std::fs::create_dir_all(project_path.join("src/nested/deeper")).expect("create nested dirs");
    client
        .write_project_file(WriteFileRequest {
            project_path: project_str.clone(),
            relative_path: "src/nested/lib.rs".into(),
            content: "pub fn value() {}\n".into(),
            overwrite: true,
        })
        .await
        .expect("write project file");
    client
        .write_project_file(WriteFileRequest {
            project_path: project_str.clone(),
            relative_path: "src/nested/deeper/hidden.rs".into(),
            content: "fn hidden() {}\n".into(),
            overwrite: true,
        })
        .await
        .expect("write deep project file");
    let file = client
        .read_project_file(ReadFileRequest {
            project_path: project_str.clone(),
            relative_path: "src/nested/lib.rs".into(),
        })
        .await
        .expect("read project file");
    assert_eq!(file.content, "pub fn value() {}\n");
    assert!(client
        .read_project_file(ReadFileRequest {
            project_path: project_str.clone(),
            relative_path: "../outside.txt".into(),
        })
        .await
        .is_err());
    assert!(client
        .write_project_file(WriteFileRequest {
            project_path: project_str.clone(),
            relative_path: "../outside.txt".into(),
            content: "must stay outside".into(),
            overwrite: true,
        })
        .await
        .is_err());
    let tree = client
        .list_project_directory(ListDirectoryRequest {
            project_path: project_str.clone(),
            relative_path: None,
            max_depth: Some(2),
        })
        .await
        .expect("list project directory");
    assert!(tree
        .entries
        .iter()
        .any(|entry| entry.path == "src/nested/lib.rs"));
    assert!(!tree
        .entries
        .iter()
        .any(|entry| entry.path == "src/nested/deeper/hidden.rs"));

    // 7. Test create session
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
    assert!(sessions
        .iter()
        .any(|s| s.session_id == "session_integration_1"));

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
    client
        .close_terminal(CloseTerminalRequest {
            terminal_id: "term_integration_1".to_string(),
        })
        .await
        .expect("close terminal");

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

#[tokio::test]
async fn test_terminal_pty_round_trip_through_daemon() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let socket_path = temp_dir.path().join("terminal_daemon.sock");
    let project_path = temp_dir.path().join("terminal_project");
    std::fs::create_dir_all(&project_path).expect("create project dir");

    let dispatcher = Arc::new(RpcDispatcher::new());
    DaemonServer::new(dispatcher)
        .serve_uds(socket_path.clone())
        .await
        .expect("serve UDS");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::connect_uds(&socket_path)
        .await
        .expect("connect client");
    client
        .subscribe_terminal()
        .await
        .expect("subscribe terminal output");
    client
        .subscribe_terminal()
        .await
        .expect("subscribe terminal output idempotently");
    let mut events = client.subscribe_terminal_events();
    let project_path = project_path.to_string_lossy().into_owned();

    for terminal_id in ["term_a", "term_b"] {
        client
            .spawn_terminal(SpawnTerminalRequest {
                project_path: project_path.clone(),
                terminal_id: Some(terminal_id.into()),
                cols: 80,
                rows: 24,
            })
            .await
            .expect("spawn terminal");
    }
    assert!(client
        .spawn_terminal(SpawnTerminalRequest {
            project_path: project_path.clone(),
            terminal_id: Some("term_a".into()),
            cols: 80,
            rows: 24,
        })
        .await
        .is_err());

    client
        .resize_terminal(ResizeTerminalRequest {
            terminal_id: "term_a".into(),
            cols: 100,
            rows: 30,
        })
        .await
        .expect("resize terminal");
    client
        .write_terminal_input("term_a", "printf 'pty-a-marker\\n'\n")
        .await
        .expect("write terminal a");
    client
        .write_terminal_input("term_b", "printf 'pty-b-marker\\n'\n")
        .await
        .expect("write terminal b");

    wait_for_terminal_marker(&mut events, "term_a", "pty-a-marker").await;
    wait_for_terminal_marker(&mut events, "term_b", "pty-b-marker").await;

    client
        .close_terminal(CloseTerminalRequest {
            terminal_id: "term_a".into(),
        })
        .await
        .expect("close terminal a");
    client
        .close_terminal(CloseTerminalRequest {
            terminal_id: "term_b".into(),
        })
        .await
        .expect("close terminal b");
    assert!(client
        .write_terminal_input("term_a", "echo should-fail\n")
        .await
        .is_err());
}

#[tokio::test]
async fn test_git_core_operations_through_daemon() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let socket_path = temp_dir.path().join("git_daemon.sock");
    let repo = temp_dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");

    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {args:?}: {:?}", output.stderr);
        output
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.email", "threadlane@example.com"]);
    git(&["config", "user.name", "Threadlane Test"]);
    std::fs::write(repo.join("README.md"), "base\n").expect("write base");
    git(&["add", "README.md"]);
    git(&["commit", "--quiet", "-m", "initial"]);
    std::fs::write(repo.join("README.md"), "changed\n").expect("write change");

    let dispatcher = Arc::new(RpcDispatcher::new());
    DaemonServer::new(dispatcher)
        .serve_uds(socket_path.clone())
        .await
        .expect("serve UDS");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let client = DaemonClient::connect_uds(&socket_path)
        .await
        .expect("connect client");
    let project_path = repo.to_string_lossy().to_string();

    let status = client.git_status(&project_path).await.expect("git status");
    assert!(status.branch.is_some());
    assert!(status.files.iter().any(|file| file.path == "README.md"));

    let branches = client
        .git_branches(GitBranchesRequest {
            project_path: project_path.clone(),
        })
        .await
        .expect("git branches");
    let current_branch = branches
        .branches
        .iter()
        .find(|branch| branch.is_current)
        .expect("current branch");
    assert!(!current_branch.relative_time.is_empty());
    assert!(current_branch.committer_date_unix > 0);

    let diff = client
        .git_diff(GitDiffRequest {
            project_path: project_path.clone(),
            file_path: Some("README.md".into()),
            staged: false,
        })
        .await
        .expect("git diff");
    assert!(diff.diff.contains("-base"));
    assert!(diff.diff.contains("+changed"));

    client
        .git_checkout(threadlane_protocol::git::GitCheckoutRequest {
            project_path: project_path.clone(),
            branch: "feature/test".into(),
            create_if_missing: true,
        })
        .await
        .expect("create and checkout branch");
    assert_eq!(
        client
            .git_status(&project_path)
            .await
            .expect("status after checkout")
            .branch
            .as_deref(),
        Some("feature/test")
    );

    client
        .git_stage_file(GitStageFileRequest {
            project_path: project_path.clone(),
            file_path: "README.md".into(),
            stage: true,
        })
        .await
        .expect("stage file");
    let commit = client
        .git_commit(GitCommitRequest {
            project_path: project_path.clone(),
            message: "update readme".into(),
        })
        .await
        .expect("commit");
    assert_eq!(commit.sha.len(), 40);
    assert!(client
        .git_status(&project_path)
        .await
        .expect("final status")
        .files
        .is_empty());
}
