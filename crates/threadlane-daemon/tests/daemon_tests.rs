use serde_json::json;
use std::sync::Arc;
use threadlane_daemon::RpcDispatcher;
use threadlane_protocol::rpc::*;

#[tokio::test]
async fn test_dispatcher_daemon_info_and_models() {
    let dispatcher = Arc::new(RpcDispatcher::new());

    let req = RpcRequest::new(1u64, "daemon/info", None);
    let res = dispatcher.dispatch(req).await;
    assert_eq!(res.id, RequestId::Number(1));
    assert!(res.error.is_none());
    let result = res.result.expect("result present");
    assert_eq!(result.get("protocol_version").unwrap(), "2.0");

    let req_models = RpcRequest::new(2u64, "capabilities/models", None);
    let res_models = dispatcher.dispatch(req_models).await;
    assert_eq!(res_models.id, RequestId::Number(2));
    assert!(res_models.error.is_none());
    let models_val = res_models.result.expect("models present");
    assert!(models_val.get("models").unwrap().is_array());
}

#[tokio::test]
async fn test_dispatcher_project_lifecycle() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let project_path = temp_dir.path().to_string_lossy().to_string();

    let dispatcher = Arc::new(RpcDispatcher::new());

    // Register project
    let req = RpcRequest::new(
        1u64,
        "project/register",
        Some(json!({ "path": project_path })),
    );
    let res = dispatcher.dispatch(req).await;
    assert!(res.error.is_none());

    // Write file
    let write_req = RpcRequest::new(
        2u64,
        "project/write_file",
        Some(json!({
            "project_path": project_path,
            "relative_path": "hello.txt",
            "content": "Hello World\nLine 2",
            "overwrite": true
        })),
    );
    let write_res = dispatcher.dispatch(write_req).await;
    assert!(write_res.error.is_none());

    // Read file
    let read_req = RpcRequest::new(
        3u64,
        "project/read_file",
        Some(json!({
            "project_path": project_path,
            "relative_path": "hello.txt"
        })),
    );
    let read_res = dispatcher.dispatch(read_req).await;
    assert!(read_res.error.is_none());
    let read_val = read_res.result.unwrap();
    assert_eq!(read_val.get("content").unwrap(), "Hello World\nLine 2");
    assert_eq!(read_val.get("line_count").unwrap(), 2);
}

#[tokio::test]
async fn test_dispatcher_session_lifecycle() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let project_path = temp_dir.path().to_string_lossy().to_string();

    let dispatcher = Arc::new(RpcDispatcher::new());

    // Create session
    let create_req = RpcRequest::new(
        1u64,
        "session/create",
        Some(json!({
            "project_path": project_path,
            "session_id": "test_session_1",
            "model": "antigravity/gemini-3.7-flash"
        })),
    );
    let create_res = dispatcher.dispatch(create_req).await;
    assert!(create_res.error.is_none(), "create_res: {:?}", create_res.error);
    let sess_val = create_res.result.unwrap();
    assert_eq!(sess_val.get("session_id").unwrap(), "test_session_1");

    // List sessions
    let list_req = RpcRequest::new(
        2u64,
        "session/list",
        Some(json!({
            "project_path": project_path
        })),
    );
    let list_res = dispatcher.dispatch(list_req).await;
    assert!(list_res.error.is_none());
}
