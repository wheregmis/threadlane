use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeToolCallFunction {
    pub name: String,
    pub arguments: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeToolCall {
    pub id: String,
    pub r#type: String,
    pub function: RuntimeToolCallFunction,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "thoughtSignature"
    )]
    pub thought_signature: Option<String>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub total_tokens: u32,
}
#[derive(Debug, Clone)]
pub enum RuntimeStreamEvent {
    ContentToken(String),
    ReasoningToken(String),
    ToolCallStart {
        name: String,
    },
    ToolCallArgsDelta {
        args_chunk: String,
    },
    Finished {
        tool_calls: Vec<RuntimeToolCall>,
        usage: RuntimeUsage,
    },
    Error(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferredResponse {
    Pending,
    Ready { content: String },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRequest {
    pub model: String,
    pub messages: serde_json::Value,
    pub tools: serde_json::Value,
    pub prompt_cache_key: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[async_trait::async_trait]
pub trait ProviderPort: Send + Sync {
    async fn stream_request(
        &self,
        request: RuntimeRequest,
        events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
    );
    async fn fetch_deferred(
        &self,
        model: &str,
        handle_id: &str,
    ) -> Result<DeferredResponse, String>;
    async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String>;
    fn provider_kind(&self, model: &str) -> &'static str;
    /// Rotate the OpenAI-branch credential for subsequent requests. Used
    /// when the session model changes providers mid-task (slash `/model`,
    /// prewalk handoffs); the shared cell inside `ProviderClient` makes the
    /// rotation visible to in-flight turn loops. Default no-op so test
    /// doubles and non-OpenAI clients compile unchanged.
    fn refresh_openai_credentials(&self, _api_key: String, _account_id: Option<String>) {}
}

/// Transport-agnostic daemon schemas (issue #79, Phase 1).
///
/// These types are the future wire contract between a `threadlane-daemon`
/// process (owning session, runtime, git, and PTY state) and thin clients
/// such as `threadlane-gpui`. They are deliberately additive and mirror the
/// in-process [`crate`] runtime shapes without importing them, so the daemon
/// can evolve without coupling to executor internals.
pub mod daemon {
    use serde::{Deserialize, Serialize};

    /// A command a client sends to the daemon.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum SessionCommand {
        SubmitPrompt {
            session_id: String,
            prompt: String,
        },
        CancelRun {
            session_id: String,
        },
        AnswerPermission {
            request_id: String,
            response: PermissionResponse,
        },
        AnswerQuestion {
            request_id: String,
            answers: Vec<QuestionItemAnswer>,
            dismissed: bool,
        },
        TerminalInput {
            terminal_id: String,
            data: String,
        },
        TerminalResize {
            terminal_id: String,
            cols: u16,
            rows: u16,
        },
        GetProjectState {
            project_id: String,
        },
    }

    /// An event the daemon broadcasts to clients.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum SessionEvent {
        TurnStarted {
            session_id: String,
            turn_number: u64,
        },
        TurnUpdate {
            session_id: String,
            text_delta: Option<String>,
        },
        TurnFinished {
            session_id: String,
            turn_number: u64,
        },
        PermissionRequested {
            session_id: String,
            request: PermissionRequest,
        },
        PermissionResolved {
            request_id: String,
            response: PermissionResponse,
        },
        QuestionRequested {
            session_id: String,
            request_id: String,
            questions: Vec<QuestionItem>,
        },
        TerminalEvent {
            session_id: String,
            event: TerminalEvent,
        },
        ProjectChanged {
            project: ProjectState,
        },
        DaemonError {
            session_id: Option<String>,
            message: String,
        },
    }

    /// Transport-agnostic permission prompt, mirroring the runtime shape.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct PermissionRequest {
        pub id: String,
        pub capability: String,
        pub title: String,
        pub detail: String,
        pub scopes: Vec<String>,
    }

    /// Transport-agnostic permission resolution.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "decision", rename_all = "snake_case")]
    pub enum PermissionResponse {
        AllowOnce,
        AllowAlways,
        Deny,
        Cancelled,
    }

    /// A single question posed to the user over the daemon transport.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct QuestionItem {
        pub id: String,
        pub header: String,
        pub question: String,
        #[serde(default)]
        pub options: Vec<String>,
        #[serde(default)]
        pub allow_custom: bool,
    }

    /// The user's answer to one [`QuestionItem`].
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct QuestionItemAnswer {
        pub question_id: String,
        #[serde(default)]
        pub selected: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub custom_text: Option<String>,
    }

    /// Terminal output or lifecycle event streamed from the daemon-owned PTY.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub enum TerminalEvent {
        Output {
            terminal_id: String,
            data: String,
        },
        Resized {
            terminal_id: String,
            cols: u16,
            rows: u16,
        },
        Exited {
            terminal_id: String,
            exit_code: Option<i32>,
        },
    }

    /// Point-in-time project snapshot served by the daemon.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ProjectState {
        pub project_id: String,
        pub root: String,
        #[serde(default)]
        pub sessions: Vec<ProjectSessionSummary>,
    }

    /// Lightweight per-session summary inside [`ProjectState`].
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ProjectSessionSummary {
        pub session_id: String,
        pub title: String,
        pub is_generating: bool,
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn sample_request() -> PermissionRequest {
            PermissionRequest {
                id: "perm_1".into(),
                capability: "network".into(),
                title: "Allow host".into(),
                detail: "example.com".into(),
                scopes: vec!["once".into(), "always".into()],
            }
        }

        #[test]
        fn session_command_round_trips_through_json() {
            let commands = vec![
                SessionCommand::SubmitPrompt {
                    session_id: "sess_1".into(),
                    prompt: "hello".into(),
                },
                SessionCommand::CancelRun {
                    session_id: "sess_1".into(),
                },
                SessionCommand::AnswerPermission {
                    request_id: "perm_1".into(),
                    response: PermissionResponse::AllowOnce,
                },
                SessionCommand::TerminalResize {
                    terminal_id: "pty_1".into(),
                    cols: 80,
                    rows: 24,
                },
                SessionCommand::GetProjectState {
                    project_id: "proj_1".into(),
                },
            ];
            for command in commands {
                let json = serde_json::to_string(&command).expect("command serializes");
                let back: SessionCommand =
                    serde_json::from_str(&json).expect("command deserializes");
                assert_eq!(command, back);
            }
        }

        #[test]
        fn session_event_round_trips_through_json() {
            let events = vec![
                SessionEvent::PermissionRequested {
                    session_id: "sess_1".into(),
                    request: sample_request(),
                },
                SessionEvent::QuestionRequested {
                    session_id: "sess_1".into(),
                    request_id: "q_1".into(),
                    questions: vec![QuestionItem {
                        id: "q1".into(),
                        header: "Scope".into(),
                        question: "Which scope?".into(),
                        options: vec!["a".into(), "b".into()],
                        allow_custom: true,
                    }],
                },
                SessionEvent::TerminalEvent {
                    session_id: "sess_1".into(),
                    event: TerminalEvent::Output {
                        terminal_id: "pty_1".into(),
                        data: "$ ".into(),
                    },
                },
                SessionEvent::ProjectChanged {
                    project: ProjectState {
                        project_id: "proj_1".into(),
                        root: "/repo".into(),
                        sessions: vec![ProjectSessionSummary {
                            session_id: "sess_1".into(),
                            title: "work".into(),
                            is_generating: true,
                        }],
                    },
                },
                SessionEvent::DaemonError {
                    session_id: None,
                    message: "boom".into(),
                },
            ];
            for event in events {
                let json = serde_json::to_string(&event).expect("event serializes");
                let back: SessionEvent = serde_json::from_str(&json).expect("event deserializes");
                assert_eq!(event, back);
            }
        }

        #[test]
        fn permission_response_variants_decode() {
            for (json, expected) in [
                (
                    r#"{"decision":"allow_once"}"#,
                    PermissionResponse::AllowOnce,
                ),
                (
                    r#"{"decision":"allow_always"}"#,
                    PermissionResponse::AllowAlways,
                ),
                (r#"{"decision":"deny"}"#, PermissionResponse::Deny),
                (r#"{"decision":"cancelled"}"#, PermissionResponse::Cancelled),
            ] {
                let back: PermissionResponse = serde_json::from_str(json).expect("decodes");
                assert_eq!(back, expected);
            }
        }
    }
}
