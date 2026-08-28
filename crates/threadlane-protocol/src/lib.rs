pub mod capabilities;
pub mod client;
pub mod commands;
pub mod git;
pub mod harness;
pub mod permission;
pub mod project;
pub mod provider;
pub mod rpc;
pub mod session;
pub mod settings;
pub mod tasks;
pub mod terminal;
pub mod update;
pub mod workspace;

// Re-export provider types for backward compatibility across the workspace
pub use capabilities::*;
pub use client::*;
pub use commands::*;
pub use git::*;
pub use harness::*;
pub use permission::*;
pub use project::*;
pub use provider::*;
pub use rpc::*;
pub use session::*;
pub use settings::*;
pub use tasks::*;
pub use terminal::*;
pub use update::*;
pub use workspace::*;

/// Normalise an LLM-generated session title by stripping surrounding quotes,
/// leading "Title:" prefixes, collapsing whitespace, and capping at 42 chars.
/// Lives here so both the daemon and GPUI can share a single implementation.
pub fn normalize_session_title(value: &str) -> String {
    let mut title = value.trim().to_string();
    loop {
        let before = title.clone();
        if title
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("title:"))
        {
            title = title[6..].trim().to_string();
        }
        let quoted = ((title.starts_with('"') && title.ends_with('"'))
            || (title.starts_with('\'') && title.ends_with('\'')))
            && title.len() >= 2;
        if quoted {
            title = title[1..title.len() - 1].trim().to_string();
        }
        if title == before {
            break;
        }
    }

    let mut collapsed = String::with_capacity(title.len());
    let mut previous_was_space = true;
    for character in title.chars() {
        if character.is_whitespace() {
            if !previous_was_space {
                collapsed.push(' ');
                previous_was_space = true;
            }
        } else {
            collapsed.push(character);
            previous_was_space = false;
        }
    }
    if collapsed.ends_with(' ') {
        collapsed.pop();
    }
    collapsed.chars().take(42).collect()
}


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
            error: Some("reader failed".to_string()),
        };
        let json_str = serde_json::to_string(&term_out).expect("serialize terminal output");
        let parsed: TerminalOutputEvent = serde_json::from_str(&json_str).expect("deserialize terminal output");
        assert_eq!(parsed, term_out);

        let legacy: TerminalOutputEvent =
            serde_json::from_str(r#"{"terminal_id":"term_legacy","data":"ok","exit_code":null}"#)
                .expect("deserialize legacy terminal output");
        assert_eq!(legacy.error, None);
    }

    #[test]
    fn test_workspace_and_git_capability_serialization() {
        let event = WorkspaceChangedEvent {
            project_path: "/workspace/project".into(),
            git_dirty: true,
            files_dirty: false,
        };
        assert_eq!(serde_json::from_value::<WorkspaceChangedEvent>(serde_json::to_value(&event).unwrap()).unwrap(), event);

        let checkout = GitCheckoutRequest {
            project_path: "/workspace/project".into(),
            branch: "feature/test".into(),
            create_if_missing: false,
            mode: GitCheckoutMode::Carry,
        };
        assert_eq!(serde_json::from_value::<GitCheckoutRequest>(serde_json::to_value(&checkout).unwrap()).unwrap(), checkout);
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
