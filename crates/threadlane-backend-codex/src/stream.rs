//! Maps the lines of `codex exec --json` to [`BackendEvent`]s. Pure: no
//! process, no I/O.
//!
//! The line types (`thread.started`, `turn.started`, `turn.completed`,
//! `turn.failed`, `item.started`, `item.updated`, `item.completed`, `error`)
//! and item types (`agent_message`, `reasoning`, `command_execution`,
//! `file_change`, `mcp_tool_call`, `web_search`, `todo_list`, `error`) are
//! those `codex-cli` 0.167.0 serializes; they were read from the installed
//! binary, not from a recorded run.

use std::collections::HashSet;

use serde_json::{Map, Value as Json, json};
use threadlane_core::backend::BackendEvent;
use threadlane_mcp::SERVER_NAME;

/// Lower-cased text Codex shows when the account's usage limit is reached:
/// "You've hit your usage limit. ... try again at ...", "Usage limit
/// reached.".
///
/// shortcut: recognised from the message text Codex 0.167.0 carries (its
/// `turn.failed`/`error` lines hold no error code) and never seen in a real
/// run; the reset time it states is in local display form, so it is not
/// parsed and the hub's fallback cooldown applies. Upgrade when the exec
/// stream carries a code or a reset timestamp.
const USAGE_LIMIT_TEXT: &[&str] = &["usage limit", "out of credits"];

/// Text Codex prints when the session to resume does not exist.
const NO_SESSION_TEXT: &[&str] = &[
    "No saved session found",
    "no rollout found",
    "thread not found",
];

/// Turns stdout lines into events, keeping what later lines depend on.
///
/// Produces at most one terminal event; every line after it is ignored.
#[derive(Debug)]
pub(crate) struct StreamParser {
    /// The session a resume asked for. Codex starts a new one rather than
    /// failing when it cannot find it, so another id means it is missing.
    resuming: Option<String>,
    seen_thread: bool,
    /// Tool items already reported as started.
    started: HashSet<String>,
    last_text: Option<String>,
    /// The last non-fatal `error` line, which a later failure may explain.
    last_error: Option<String>,
    ended: bool,
    ended_by_turn: bool,
}

impl StreamParser {
    /// A parser for one process; `resuming` is the session a resume asked for.
    pub(crate) fn new(resuming: Option<String>) -> Self {
        Self {
            resuming,
            seen_thread: false,
            started: HashSet::new(),
            last_text: None,
            last_error: None,
            ended: false,
            ended_by_turn: false,
        }
    }

    /// Whether a terminal event has been produced.
    pub(crate) fn ended(&self) -> bool {
        self.ended
    }

    /// Whether the terminal event came from the end of the turn, after which
    /// the process exits on its own.
    pub(crate) fn ended_by_turn(&self) -> bool {
        self.ended_by_turn
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
        let Ok(Json::Object(message)) = serde_json::from_str::<Json>(line) else {
            tracing::debug!("ignoring a stdout line that is not a JSON object");
            return Vec::new();
        };
        let text = |key: &str| message.get(key).and_then(Json::as_str);
        let events = match text("type") {
            Some("thread.started") => self.thread(text("thread_id")),
            Some("item.started") => self.item(&message, false),
            Some("item.completed") => self.item(&message, true),
            Some("turn.completed") => {
                self.ended_by_turn = true;
                vec![BackendEvent::Finished {
                    final_text: self.last_text.clone(),
                    usage: json!({ "usage": message.get("usage").cloned().unwrap_or(Json::Null) }),
                }]
            }
            Some("turn.failed") => {
                self.ended_by_turn = true;
                let error = message
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Json::as_str)
                    .unwrap_or("codex ended the turn with an error")
                    .to_owned();
                vec![self.failure(error)]
            }
            Some("error") => {
                self.last_error = text("message").map(str::to_owned);
                Vec::new()
            }
            other => {
                tracing::debug!(kind = ?other, "ignoring a stream line");
                Vec::new()
            }
        };
        if events.iter().any(BackendEvent::is_terminal) {
            self.ended = true;
        }
        events
    }

    /// The terminal event for output that closed without one, given the
    /// stderr tail and how the process exited.
    pub(crate) fn closed(&mut self, stderr: &str, status: &str) -> BackendEvent {
        self.ended = true;
        let error = match (&self.last_error, stderr.is_empty()) {
            (Some(error), true) => format!("codex ended without a result ({status}): {error}"),
            (Some(error), false) => {
                format!("codex ended without a result ({status}): {error}; {stderr}")
            }
            (None, true) => format!("codex ended without a result ({status})"),
            (None, false) => format!("codex ended without a result ({status}): {stderr}"),
        };
        self.failure(error)
    }

    fn thread(&mut self, id: Option<&str>) -> Vec<BackendEvent> {
        let Some(id) = id else {
            tracing::debug!("ignoring a thread.started line without an id");
            return Vec::new();
        };
        self.seen_thread = true;
        if self.resuming.as_deref().is_some_and(|asked| asked != id) {
            return vec![BackendEvent::SessionMissing];
        }
        vec![BackendEvent::SessionStarted {
            native_session_id: id.to_owned(),
        }]
    }

    /// A failure as the event the runner acts on.
    fn failure(&self, error: String) -> BackendEvent {
        let lower = error.to_lowercase();
        let earlier = self
            .last_error
            .as_deref()
            .unwrap_or_default()
            .to_lowercase();
        if USAGE_LIMIT_TEXT
            .iter()
            .any(|text| lower.contains(text) || earlier.contains(text))
        {
            return BackendEvent::UsageLimit { resets_at: None };
        }
        if !self.seen_thread && NO_SESSION_TEXT.iter().any(|text| error.contains(text)) {
            return BackendEvent::SessionMissing;
        }
        BackendEvent::Failed { error }
    }

    fn item(&mut self, message: &Map<String, Json>, completed: bool) -> Vec<BackendEvent> {
        let Some(item) = message.get("item").and_then(Json::as_object) else {
            return Vec::new();
        };
        let field = |key: &str| item.get(key).cloned().unwrap_or(Json::Null);
        let id = item.get("id").and_then(Json::as_str).map(str::to_owned);
        let (name, input) = match item.get("type").and_then(Json::as_str) {
            Some("agent_message") if completed => {
                let text = item
                    .get("text")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_owned();
                self.last_text = Some(text.clone());
                return vec![BackendEvent::Text { text }];
            }
            Some("mcp_tool_call") => {
                let server = item
                    .get("server")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let tool = item.get("tool").and_then(Json::as_str).unwrap_or_default();
                let name = if server == SERVER_NAME {
                    tool.to_owned()
                } else {
                    format!("mcp__{server}__{tool}")
                };
                (name, field("arguments"))
            }
            Some("command_execution") => (
                "command_execution".to_owned(),
                json!({ "command": field("command") }),
            ),
            Some("file_change") => (
                "file_change".to_owned(),
                json!({ "changes": field("changes") }),
            ),
            Some("web_search") => ("web_search".to_owned(), json!({ "query": field("query") })),
            _ => return Vec::new(),
        };
        let mut events = Vec::new();
        let first_sight = id.as_ref().is_none_or(|id| self.started.insert(id.clone()));
        if first_sight {
            events.push(BackendEvent::ToolUse {
                call_id: id.clone(),
                name,
                input,
            });
        }
        if completed {
            events.push(tool_result(item, id));
        }
        events
    }
}

/// The outcome of a completed tool item.
fn tool_result(item: &Map<String, Json>, call_id: Option<String>) -> BackendEvent {
    let status = item
        .get("status")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let error = item.get("error").filter(|error| !error.is_null()).cloned();
    let exit_code = item.get("exit_code").and_then(Json::as_i64);
    let is_error =
        status != "completed" || error.is_some() || exit_code.is_some_and(|code| code != 0);
    let output = error
        .or_else(|| item.get("result").cloned())
        .or_else(|| item.get("aggregated_output").cloned())
        .unwrap_or(Json::Null);
    BackendEvent::ToolResult {
        call_id,
        output,
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREAD: &str = "0199a213-81c0-7800-8aa1-bbab2a035a53";

    fn feed(parser: &mut StreamParser, lines: &[Json]) -> Vec<BackendEvent> {
        lines
            .iter()
            .flat_map(|line| parser.line(&line.to_string()))
            .collect()
    }

    fn started() -> Json {
        json!({"type": "thread.started", "thread_id": THREAD})
    }

    #[test]
    fn a_run_with_a_tool_call_and_usage() {
        let mut parser = StreamParser::new(None);
        let call = json!({"id": "item_1", "type": "mcp_tool_call", "server": "threadlane",
            "tool": "send_message", "arguments": {"text": "hi"}, "status": "in_progress"});
        let mut done = call.clone();
        done["status"] = json!("completed");
        done["result"] = json!({"content": [{"type": "text", "text": "{\"sent\":true}"}]});
        let events = feed(
            &mut parser,
            &[
                started(),
                json!({"type": "turn.started"}),
                json!({"type": "item.completed", "item": {"id": "item_0", "type": "reasoning", "text": "…"}}),
                json!({"type": "item.started", "item": call}),
                json!({"type": "item.completed", "item": done}),
                json!({"type": "item.completed", "item": {"id": "item_2", "type": "agent_message", "text": "done"}}),
                json!({"type": "turn.completed", "usage": {"input_tokens": 10, "cached_input_tokens": 2, "output_tokens": 3}}),
            ],
        );
        assert_eq!(
            events,
            vec![
                BackendEvent::SessionStarted {
                    native_session_id: THREAD.to_owned()
                },
                BackendEvent::ToolUse {
                    call_id: Some("item_1".to_owned()),
                    name: "send_message".to_owned(),
                    input: json!({"text": "hi"}),
                },
                BackendEvent::ToolResult {
                    call_id: Some("item_1".to_owned()),
                    output: json!({"content": [{"type": "text", "text": "{\"sent\":true}"}]}),
                    is_error: false,
                },
                BackendEvent::Text {
                    text: "done".to_owned()
                },
                BackendEvent::Finished {
                    final_text: Some("done".to_owned()),
                    usage: json!({"usage": {"input_tokens": 10, "cached_input_tokens": 2, "output_tokens": 3}}),
                },
            ]
        );
        assert!(parser.ended() && parser.ended_by_turn());
        assert!(
            parser
                .line(&json!({"type": "turn.completed"}).to_string())
                .is_empty()
        );
    }

    #[test]
    fn commands_and_other_servers_tools_are_tool_uses() {
        let mut parser = StreamParser::new(None);
        let refused = json!({"type": "item.completed", "item": {"id": "item_3", "type": "command_execution",
            "command": "/bin/zsh -lc 'git push'", "aggregated_output": "forbidden", "exit_code": null, "status": "declined"}});
        let other = json!({"type": "item.completed", "item": {"id": "item_4", "type": "mcp_tool_call",
            "server": "docs", "tool": "search", "arguments": {}, "error": {"message": "boom"}, "status": "failed"}});
        assert_eq!(
            feed(&mut parser, &[refused, other]),
            vec![
                BackendEvent::ToolUse {
                    call_id: Some("item_3".to_owned()),
                    name: "command_execution".to_owned(),
                    input: json!({"command": "/bin/zsh -lc 'git push'"}),
                },
                BackendEvent::ToolResult {
                    call_id: Some("item_3".to_owned()),
                    output: json!("forbidden"),
                    is_error: true,
                },
                BackendEvent::ToolUse {
                    call_id: Some("item_4".to_owned()),
                    name: "mcp__docs__search".to_owned(),
                    input: json!({}),
                },
                BackendEvent::ToolResult {
                    call_id: Some("item_4".to_owned()),
                    output: json!({"message": "boom"}),
                    is_error: true,
                },
            ]
        );
    }

    #[test]
    fn a_usage_limit_is_recognised_from_the_turn_or_an_earlier_error() {
        let limit = "You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), or try again at 3:45 PM.";
        let mut parser = StreamParser::new(None);
        let events = feed(
            &mut parser,
            &[
                started(),
                json!({"type": "turn.failed", "error": {"message": limit}}),
            ],
        );
        assert_eq!(events[1], BackendEvent::UsageLimit { resets_at: None });

        let mut earlier = StreamParser::new(None);
        let events = feed(
            &mut earlier,
            &[
                json!({"type": "error", "message": limit}),
                json!({"type": "turn.failed", "error": {"message": "stream disconnected"}}),
            ],
        );
        assert_eq!(events, vec![BackendEvent::UsageLimit { resets_at: None }]);

        let mut closed = StreamParser::new(None);
        assert_eq!(
            closed.closed("Error: Usage limit reached.", "exit status: 1"),
            BackendEvent::UsageLimit { resets_at: None }
        );
    }

    #[test]
    fn a_resume_that_finds_no_session_is_missing() {
        let mut other_thread = StreamParser::new(Some("an-old-id".to_owned()));
        assert_eq!(
            other_thread.line(&started().to_string()),
            vec![BackendEvent::SessionMissing]
        );
        assert!(other_thread.ended() && !other_thread.ended_by_turn());

        let mut same = StreamParser::new(Some(THREAD.to_owned()));
        assert!(matches!(
            same.line(&started().to_string()).as_slice(),
            [BackendEvent::SessionStarted { .. }]
        ));

        let mut refused = StreamParser::new(Some(THREAD.to_owned()));
        assert_eq!(
            refused.closed(
                &format!("Error: No saved session found with ID {THREAD}"),
                "exit status: 1"
            ),
            BackendEvent::SessionMissing
        );
    }

    #[test]
    fn other_failures_keep_their_text() {
        let mut parser = StreamParser::new(None);
        let events = feed(
            &mut parser,
            &[
                json!({"type": "error", "message": "Reconnecting... 1/5"}),
                json!({"type": "turn.failed", "error": {"message": "unexpected status 500"}}),
            ],
        );
        assert_eq!(
            events,
            vec![BackendEvent::Failed {
                error: "unexpected status 500".to_owned()
            }]
        );
        let mut closed = StreamParser::new(None);
        assert_eq!(
            closed.closed("required MCP servers failed to initialize: threadlane", "exit status: 1"),
            BackendEvent::Failed {
                error: "codex ended without a result (exit status: 1): required MCP servers failed to initialize: threadlane".to_owned()
            }
        );
    }

    #[test]
    fn garbage_lines_are_ignored() {
        let mut parser = StreamParser::new(None);
        for line in [
            "",
            "not json",
            "[1]",
            "{\"type\": \"item.updated\"}",
            "{\"type\": \"item.completed\"}",
        ] {
            assert!(parser.line(line).is_empty(), "{line}");
        }
        assert!(!parser.ended());
    }
}
