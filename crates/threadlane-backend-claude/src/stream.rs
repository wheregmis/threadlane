//! Maps the lines of `claude -p --output-format stream-json --verbose` to
//! [`BackendEvent`]s (M1 §5 "Events"). Pure: no process, no I/O.

use serde_json::{Map, Value as Json};
use threadlane_core::backend::BackendEvent;
use threadlane_core::time::Timestamp;
use threadlane_mcp::SERVER_NAME;

/// Text in an error result that means the session to resume does not exist.
const NO_CONVERSATION: &str = "No conversation found";

/// How older Claude versions worded an exhausted subscription in the result
/// text, followed by `|<unix seconds>`.
const USAGE_LIMIT_TEXT: &str = "Claude AI usage limit reached";

/// The fields of a `result` line kept as the run's usage.
const USAGE_FIELDS: [&str; 4] = ["usage", "total_cost_usd", "num_turns", "modelUsage"];

/// Turns stdout lines into events, keeping what later lines depend on.
///
/// Produces at most one terminal event; every line after it is ignored.
#[derive(Debug)]
pub(crate) struct StreamParser {
    expects_tools: bool,
    tool_prefix: String,
    seen_init: bool,
    ended: bool,
    ended_by_result: bool,
    /// Set by a `rate_limit_event` whose status says requests are refused.
    rate_limited: Option<Option<Timestamp>>,
}

impl StreamParser {
    /// A parser for one process. `expects_tools` says whether the run was
    /// given the system's tools, so the `threadlane` server must connect.
    pub(crate) fn new(expects_tools: bool) -> Self {
        Self {
            expects_tools,
            tool_prefix: format!("mcp__{SERVER_NAME}__"),
            seen_init: false,
            ended: false,
            ended_by_result: false,
            rate_limited: None,
        }
    }

    /// Whether the `system`/`init` line has been seen.
    pub(crate) fn seen_init(&self) -> bool {
        self.seen_init
    }

    /// Whether a terminal event has been produced.
    pub(crate) fn ended(&self) -> bool {
        self.ended
    }

    /// Whether the terminal event came from a `result` line, after which the
    /// process exits on its own.
    pub(crate) fn ended_by_result(&self) -> bool {
        self.ended_by_result
    }

    /// The events one stdout line produces, in order.
    pub(crate) fn line(&mut self, line: &str) -> Vec<BackendEvent> {
        if self.ended {
            return Vec::new();
        }
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let message: Json = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                tracing::debug!(%error, "ignoring a stdout line that is not JSON");
                return Vec::new();
            }
        };
        let Some(message) = message.as_object() else {
            tracing::debug!("ignoring a stdout line that is not a JSON object");
            return Vec::new();
        };
        let events = match message.get("type").and_then(Json::as_str) {
            Some("system") => self.system(message),
            Some("assistant") => self.assistant(message),
            Some("user") => user(message),
            Some("rate_limit_event") => {
                self.rate_limit(message);
                Vec::new()
            }
            Some("result") => {
                self.ended_by_result = true;
                vec![self.result(message)]
            }
            other => {
                tracing::debug!(kind = ?other, "ignoring a stream line of unknown type");
                Vec::new()
            }
        };
        if events.iter().any(BackendEvent::is_terminal) {
            self.ended = true;
        }
        events
    }

    fn system(&mut self, message: &Map<String, Json>) -> Vec<BackendEvent> {
        if message.get("subtype").and_then(Json::as_str) != Some("init") {
            return Vec::new();
        }
        let Some(session_id) = message.get("session_id").and_then(Json::as_str) else {
            tracing::debug!("ignoring an init line without a session id");
            return Vec::new();
        };
        self.seen_init = true;
        let mut events = vec![BackendEvent::SessionStarted {
            native_session_id: session_id.to_owned(),
        }];
        if self.expects_tools {
            let status = message
                .get("mcp_servers")
                .and_then(Json::as_array)
                .and_then(|servers| {
                    servers.iter().find(|server| {
                        server.get("name").and_then(Json::as_str) == Some(SERVER_NAME)
                    })
                })
                .map(|server| {
                    server
                        .get("status")
                        .and_then(Json::as_str)
                        .unwrap_or("unknown")
                });
            match status {
                Some("connected") => {}
                Some(status) => events.push(BackendEvent::Failed {
                    error: format!(
                        "the {SERVER_NAME} tool server did not connect (status {status})"
                    ),
                }),
                None => events.push(BackendEvent::Failed {
                    error: format!("claude did not load the {SERVER_NAME} tool server"),
                }),
            }
        }
        events
    }

    fn assistant(&self, message: &Map<String, Json>) -> Vec<BackendEvent> {
        content_blocks(message)
            .filter_map(|block| match block.get("type").and_then(Json::as_str) {
                Some("text") => Some(BackendEvent::Text {
                    text: block
                        .get("text")
                        .and_then(Json::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }),
                Some("tool_use") => {
                    let name = block.get("name").and_then(Json::as_str).unwrap_or_default();
                    Some(BackendEvent::ToolUse {
                        call_id: block.get("id").and_then(Json::as_str).map(str::to_owned),
                        name: name
                            .strip_prefix(&self.tool_prefix)
                            .unwrap_or(name)
                            .to_owned(),
                        input: block.get("input").cloned().unwrap_or(Json::Null),
                    })
                }
                _ => None,
            })
            .collect()
    }

    fn rate_limit(&mut self, message: &Map<String, Json>) {
        let info = message.get("rate_limit_info");
        let status = info
            .and_then(|info| info.get("status"))
            .and_then(Json::as_str);
        // `allowed_warning` means close to the limit, not over it.
        if matches!(status, Some("allowed" | "allowed_warning")) {
            self.rate_limited = None;
            return;
        }
        let resets_at = info
            .and_then(|info| info.get("resetsAt"))
            .and_then(unix_seconds);
        tracing::debug!(?status, "claude reported a rate limit");
        self.rate_limited = Some(resets_at);
    }

    fn result(&self, message: &Map<String, Json>) -> BackendEvent {
        let subtype = message
            .get("subtype")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let is_error = message
            .get("is_error")
            .and_then(Json::as_bool)
            .unwrap_or(false)
            || subtype != "success";
        let result_text = message.get("result").and_then(Json::as_str);
        if !is_error {
            let usage: Map<String, Json> = USAGE_FIELDS
                .iter()
                .filter_map(|field| {
                    message
                        .get(*field)
                        .map(|value| ((*field).to_owned(), value.clone()))
                })
                .collect();
            return BackendEvent::Finished {
                final_text: result_text.map(str::to_owned),
                usage: Json::Object(usage),
            };
        }
        let mut errors: Vec<&str> = message
            .get("errors")
            .and_then(Json::as_array)
            .map(|errors| errors.iter().filter_map(Json::as_str).collect())
            .unwrap_or_default();
        if let Some(text) = result_text {
            errors.push(text);
        }
        if errors.iter().any(|error| error.contains(NO_CONVERSATION)) {
            return BackendEvent::SessionMissing;
        }
        if let Some(resets_at) = self.rate_limited {
            return BackendEvent::UsageLimit { resets_at };
        }
        if message.get("api_error_status").and_then(Json::as_i64) == Some(429) {
            return BackendEvent::UsageLimit { resets_at: None };
        }
        if let Some(rest) = result_text.and_then(|text| text.strip_prefix(USAGE_LIMIT_TEXT)) {
            let resets_at = rest
                .strip_prefix('|')
                .and_then(|seconds| seconds.trim().parse::<i64>().ok())
                .and_then(|seconds| seconds.checked_mul(1000))
                .map(Timestamp::from_millis);
            return BackendEvent::UsageLimit { resets_at };
        }
        let error = if errors.is_empty() {
            format!("claude ended with {subtype}")
        } else {
            errors.join("; ")
        };
        BackendEvent::Failed { error }
    }
}

fn user(message: &Map<String, Json>) -> Vec<BackendEvent> {
    content_blocks(message)
        .filter(|block| block.get("type").and_then(Json::as_str) == Some("tool_result"))
        .map(|block| BackendEvent::ToolResult {
            call_id: block
                .get("tool_use_id")
                .and_then(Json::as_str)
                .map(str::to_owned),
            output: block.get("content").cloned().unwrap_or(Json::Null),
            is_error: block
                .get("is_error")
                .and_then(Json::as_bool)
                .unwrap_or(false),
        })
        .collect()
}

fn content_blocks(message: &Map<String, Json>) -> impl Iterator<Item = &Map<String, Json>> {
    message
        .get("message")
        .and_then(|inner| inner.get("content"))
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(Json::as_object)
}

fn unix_seconds(value: &Json) -> Option<Timestamp> {
    let millis = match value.as_i64() {
        Some(seconds) => seconds.checked_mul(1000)?,
        // Observed as an integer; a float is accepted in case that changes.
        // `as` saturates, which is fine for a reset time.
        None => (value.as_f64().filter(|seconds| seconds.is_finite())? * 1000.0).round() as i64,
    };
    Some(Timestamp::from_millis(millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    const SESSION: &str = "0b9d4c6e-3f0a-4a7e-9a52-6f1e7f9e2c11";

    fn init(status: &str) -> String {
        json!({
            "type": "system", "subtype": "init", "cwd": "/tmp/x", "session_id": SESSION,
            "tools": ["mcp__threadlane__send_message"],
            "mcp_servers": [{"name": "threadlane", "status": status}],
            "model": "claude-haiku-4-5", "permissionMode": "bypassPermissions",
        })
        .to_string()
    }

    fn feed(parser: &mut StreamParser, lines: &[&str]) -> Vec<BackendEvent> {
        lines.iter().flat_map(|line| parser.line(line)).collect()
    }

    #[test]
    fn init_reports_the_session() {
        let mut parser = StreamParser::new(true);
        assert_eq!(
            parser.line(&init("connected")),
            vec![BackendEvent::SessionStarted {
                native_session_id: SESSION.to_owned()
            }]
        );
        assert!(parser.seen_init());
        assert!(!parser.ended());
    }

    #[test]
    fn init_without_a_connected_tool_server_fails_when_tools_were_given() {
        let mut parser = StreamParser::new(true);
        let events = parser.line(&init("failed"));
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], BackendEvent::Failed { error } if error.contains("failed")));
        assert!(parser.ended());
        assert!(!parser.ended_by_result());

        let mut missing = StreamParser::new(true);
        let line =
            json!({"type": "system", "subtype": "init", "session_id": SESSION, "mcp_servers": []})
                .to_string();
        assert!(matches!(
            missing.line(&line).last(),
            Some(BackendEvent::Failed { .. })
        ));

        let mut no_tools = StreamParser::new(false);
        assert_eq!(no_tools.line(&init("failed")).len(), 1);
    }

    #[test]
    fn other_system_lines_are_ignored() {
        let mut parser = StreamParser::new(false);
        let hook =
            json!({"type": "system", "subtype": "hook_started", "session_id": SESSION}).to_string();
        let response =
            json!({"type": "system", "subtype": "hook_response", "output": "x"}).to_string();
        assert!(feed(&mut parser, &[&hook, &response]).is_empty());
        assert!(!parser.seen_init());
    }

    #[test]
    fn assistant_text_and_tool_use() {
        let mut parser = StreamParser::new(true);
        let text = json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "On it."}]}, "session_id": SESSION}).to_string();
        let tool = json!({"type": "assistant", "message": {"content": [
            {"type": "thinking", "thinking": "…"},
            {"type": "tool_use", "id": "toolu_01X", "name": "mcp__threadlane__send_message", "input": {"text": "hello"}},
            {"type": "tool_use", "id": "toolu_02Y", "name": "Bash", "input": {"command": "ls"}},
        ]}}).to_string();
        assert_eq!(
            feed(&mut parser, &[&text, &tool]),
            vec![
                BackendEvent::Text {
                    text: "On it.".to_owned()
                },
                BackendEvent::ToolUse {
                    call_id: Some("toolu_01X".to_owned()),
                    name: "send_message".to_owned(),
                    input: json!({"text": "hello"}),
                },
                BackendEvent::ToolUse {
                    call_id: Some("toolu_02Y".to_owned()),
                    name: "Bash".to_owned(),
                    input: json!({"command": "ls"}),
                },
            ]
        );
    }

    #[test]
    fn user_tool_results() {
        let mut parser = StreamParser::new(true);
        let line = json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_01X", "content": [{"type": "text", "text": "{\"sent\":true}"}]},
            {"type": "tool_result", "tool_use_id": "toolu_02Y", "content": "boom", "is_error": true},
            {"type": "text", "text": "ignored"},
        ]}}).to_string();
        assert_eq!(
            parser.line(&line),
            vec![
                BackendEvent::ToolResult {
                    call_id: Some("toolu_01X".to_owned()),
                    output: json!([{"type": "text", "text": "{\"sent\":true}"}]),
                    is_error: false,
                },
                BackendEvent::ToolResult {
                    call_id: Some("toolu_02Y".to_owned()),
                    output: json!("boom"),
                    is_error: true,
                },
            ]
        );
    }

    fn success() -> String {
        json!({
            "type": "result", "subtype": "success", "is_error": false, "result": "Done.",
            "session_id": SESSION, "num_turns": 2, "total_cost_usd": 0.0123,
            "usage": {"input_tokens": 10, "output_tokens": 5}, "modelUsage": {"claude-haiku-4-5": {}},
            "api_error_status": null, "duration_ms": 1234,
        })
        .to_string()
    }

    fn error_result(errors: &[&str], api_error_status: Option<i64>) -> String {
        json!({
            "type": "result", "subtype": "error_during_execution", "is_error": true,
            "errors": errors, "api_error_status": api_error_status, "session_id": SESSION,
        })
        .to_string()
    }

    #[test]
    fn result_success_finishes_with_usage() {
        let mut parser = StreamParser::new(false);
        assert_eq!(
            parser.line(&success()),
            vec![BackendEvent::Finished {
                final_text: Some("Done.".to_owned()),
                usage: json!({
                    "usage": {"input_tokens": 10, "output_tokens": 5},
                    "total_cost_usd": 0.0123,
                    "num_turns": 2,
                    "modelUsage": {"claude-haiku-4-5": {}},
                }),
            }]
        );
        assert!(parser.ended());
        assert!(parser.ended_by_result());
        assert!(
            parser.line(&success()).is_empty(),
            "one terminal event only"
        );
    }

    #[test]
    fn allowed_rate_limit_events_change_nothing() {
        let mut parser = StreamParser::new(false);
        let allowed = json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed", "resetsAt": 1_794_892_800, "rateLimitType": "five_hour"}}).to_string();
        assert!(parser.line(&allowed).is_empty());
        assert!(matches!(
            parser.line(&error_result(&["boom"], None)).as_slice(),
            [BackendEvent::Failed { error }] if error == "boom"
        ));
    }

    #[test]
    fn a_rejected_rate_limit_turns_an_error_into_a_usage_limit() {
        let mut parser = StreamParser::new(false);
        let rejected = json!({"type": "rate_limit_event", "rate_limit_info": {"status": "rejected", "resetsAt": 1_794_892_800}}).to_string();
        assert!(parser.line(&rejected).is_empty());
        assert_eq!(
            parser.line(&error_result(&["limit"], None)),
            vec![BackendEvent::UsageLimit {
                resets_at: Some(Timestamp::from_millis(1_794_892_800_000))
            }]
        );
    }

    #[test]
    fn status_429_is_a_usage_limit() {
        let mut parser = StreamParser::new(false);
        assert_eq!(
            parser.line(&error_result(&["API Error: 429"], Some(429))),
            vec![BackendEvent::UsageLimit { resets_at: None }]
        );
    }

    #[test]
    fn no_conversation_found_is_session_missing() {
        let mut parser = StreamParser::new(false);
        let line = error_result(
            &[&format!("No conversation found with session ID: {SESSION}")],
            None,
        );
        assert_eq!(parser.line(&line), vec![BackendEvent::SessionMissing]);
        assert!(parser.ended_by_result());
    }

    #[test]
    fn other_errors_fail_with_their_text() {
        let mut parser = StreamParser::new(false);
        let line = json!({"type": "result", "subtype": "error_max_turns", "is_error": false, "session_id": SESSION}).to_string();
        assert_eq!(
            parser.line(&line),
            vec![BackendEvent::Failed {
                error: "claude ended with error_max_turns".to_owned()
            }]
        );

        let mut text = StreamParser::new(false);
        let line = json!({"type": "result", "subtype": "success", "is_error": true, "result": "API Error: 500 overloaded"}).to_string();
        assert_eq!(
            text.line(&line),
            vec![BackendEvent::Failed {
                error: "API Error: 500 overloaded".to_owned()
            }]
        );
    }

    #[test]
    fn garbage_lines_are_ignored() {
        let mut parser = StreamParser::new(false);
        let lines = [
            "",
            "   ",
            "not json",
            "{\"type\": ",
            "[1, 2, 3]",
            "42",
            r#"{"type":"stream_event","event":{}}"#,
            r#"{"no_type":true}"#,
            r#"{"type":"assistant"}"#,
            r#"{"type":"user","message":{"content":"plain text"}}"#,
        ];
        assert!(feed(&mut parser, &lines).is_empty());
        assert!(!parser.ended());
    }
}
