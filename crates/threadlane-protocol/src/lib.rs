pub mod capabilities;
pub mod git;
pub mod permission;
pub mod project;
pub mod provider;
pub mod rpc;
pub mod session;
pub mod tasks;
pub mod terminal;

// Re-export provider types for backward compatibility across the workspace
pub use capabilities::*;
pub use git::*;
pub use permission::*;
pub use project::*;
pub use provider::*;
pub use rpc::*;
pub use session::*;
pub use tasks::*;
pub use terminal::*;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_rpc_request_response_serialization() {
        let req = RpcRequest::new(1u64, "session/create", Some(json!({"project_path": "/tmp/test"})));
        let json_str = serde_json::to_string(&req).expect("serialize request");
        let parsed: RpcRequest = serde_json::from_str(&json_str).expect("deserialize request");
        assert_eq!(parsed.id, RequestId::Number(1));
        assert_eq!(parsed.method, "session/create");

        let res = RpcResponse::success(1u64, json!({"session_id": "sess_123"}));
        let res_json = serde_json::to_string(&res).expect("serialize response");
        let parsed_res: RpcResponse = serde_json::from_str(&res_json).expect("deserialize response");
        assert_eq!(parsed_res.id, RequestId::Number(1));
        assert!(parsed_res.result.is_some());
        assert!(parsed_res.error.is_none());
    }

    #[test]
    fn test_rpc_error_serialization() {
        let err = RpcError::session_not_found("sess_404");
        let res = RpcResponse::error(2u64, err);
        let res_json = serde_json::to_string(&res).expect("serialize error response");
        let parsed_res: RpcResponse = serde_json::from_str(&res_json).expect("deserialize error response");
        assert_eq!(parsed_res.id, RequestId::Number(2));
        assert!(parsed_res.result.is_none());
        let err_obj = parsed_res.error.expect("error object present");
        assert_eq!(err_obj.code, ERROR_SESSION_NOT_FOUND);
    }

    #[test]
    fn test_session_event_serialization() {
        let event = SessionEvent::TokenDelta {
            delta: "hello world".to_string(),
        };
        let json_str = serde_json::to_string(&event).expect("serialize token delta");
        let parsed: SessionEvent = serde_json::from_str(&json_str).expect("deserialize token delta");
        assert_eq!(parsed, event);

        let perm_event = SessionEvent::PermissionRequested {
            request: PermissionRequest {
                id: "perm_1".to_string(),
                session_id: Some("sess_1".to_string()),
                capability: "run_command".to_string(),
                title: "Execute Command".to_string(),
                detail: "cargo check".to_string(),
                scopes: vec![PermissionScope::Once, PermissionScope::Always],
                options: vec![],
            },
        };
        let perm_json = serde_json::to_string(&perm_event).expect("serialize perm event");
        let parsed_perm: SessionEvent = serde_json::from_str(&perm_json).expect("deserialize perm event");
        assert_eq!(parsed_perm, perm_event);
    }

    #[test]
    fn test_terminal_event_serialization() {
        let term_out = TerminalOutputEvent {
            terminal_id: "term_1".to_string(),
            data: "ls -la\r\n".to_string(),
            exit_code: None,
        };
        let json_str = serde_json::to_string(&term_out).expect("serialize terminal output");
        let parsed: TerminalOutputEvent = serde_json::from_str(&json_str).expect("deserialize terminal output");
        assert_eq!(parsed, term_out);
    }

    #[test]
    fn test_backward_compatibility_provider_types() {
        let usage = RuntimeUsage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 30,
        };
        let tool_call = RuntimeToolCall {
            id: "call_1".to_string(),
            r#type: "function".to_string(),
            function: RuntimeToolCallFunction {
                name: "view_file".to_string(),
                arguments: "{}".to_string(),
            },
            thought_signature: None,
        };
        let stream_event = RuntimeStreamEvent::Finished {
            tool_calls: vec![tool_call],
            usage,
        };
        let json_str = serde_json::to_string(&stream_event).expect("serialize stream event");
        let parsed: RuntimeStreamEvent = serde_json::from_str(&json_str).expect("deserialize stream event");
        assert_eq!(parsed, stream_event);
    }
}
