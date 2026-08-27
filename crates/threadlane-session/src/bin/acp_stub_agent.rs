//! A minimal ACP agent used to test Threadlane's ACP client end to end.
//!
//! `AcpEngine` spawns a real subprocess and talks JSON-RPC over its pipes, so
//! the spawn, handshake, streaming, permission, and cancel paths cannot be
//! covered by an in-process stub. This binary is that subprocess. It is not
//! part of the product: it exists so `tests/acp_engine_tests.rs` can drive the
//! client against something that behaves like an agent without depending on a
//! third-party binary being installed.
//!
//! Behavior is chosen by `THREADLANE_STUB_MODE`:
//!
//! * `stream` (default) — streams a thought, some text, and a tool call.
//! * `permission` — asks the client for permission and reports the answer.
//! * `cancel` — never answers the prompt, so the turn must be cancelled. On
//!   receiving `session/cancel` it touches `THREADLANE_STUB_CANCEL_MARKER`,
//!   which is how a test observes that the notification arrived without
//!   inspecting global process state.
//! * `no_images` — declares no image support, and echoes the prompt back.
//! * `config` — exposes model and effort settings and replies with the values
//!   currently applied, so a test can prove a setting crossed the wire.

use std::io::{BufRead, Write};

fn main() {
    let mode = std::env::var("THREADLANE_STUB_MODE").unwrap_or_else(|_| "stream".to_string());
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    // Current values of the settings this stub exposes, mutated by
    // `session/set_config_option` exactly as a real agent would.
    let mut model = "default".to_string();
    let mut effort = "default".to_string();

    while let Some(Ok(line)) = lines.next() {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        // Responses to our own client-bound requests carry no method.
        let Some(method) = message.get("method").and_then(|m| m.as_str()) else {
            continue;
        };
        let id = message.get("id").cloned();

        match method {
            "initialize" => reply(
                id,
                serde_json::json!({
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": false,
                        "promptCapabilities": { "image": mode != "no_images" },
                    },
                    "authMethods": [],
                    "agentInfo": { "name": "Stub Agent", "version": "0.0.0" },
                }),
            ),
            "session/new" => {
                if mode == "config" {
                    reply(
                        id,
                        serde_json::json!({
                            "sessionId": "stub-session",
                            "configOptions": config_options(&model, &effort),
                        }),
                    );
                } else {
                    reply(id, serde_json::json!({ "sessionId": "stub-session" }));
                }
            }
            "session/set_config_option" => {
                let params = message.get("params");
                let config_id = params
                    .and_then(|p| p.get("configId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let value = params
                    .and_then(|p| p.get("value"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                match config_id {
                    "model" => model = value,
                    "effort" => effort = value,
                    _ => {
                        error(id, -32602, &format!("Unknown configId: {config_id}"));
                        continue;
                    }
                }
                reply(
                    id,
                    serde_json::json!({ "configOptions": config_options(&model, &effort) }),
                );
            }
            "session/prompt" => {
                if mode == "config" {
                    notify_update(serde_json::json!({
                        "sessionUpdate": "agent_message_chunk",
                        "content": {
                            "type": "text",
                            "text": format!("model:{model} effort:{effort}"),
                        },
                    }));
                    reply(id, serde_json::json!({ "stopReason": "end_turn" }));
                } else {
                    handle_prompt(&mode, id, &message, &mut lines);
                }
            }
            "session/cancel" => {
                // A cancel is a notification; the prompt is answered by the
                // pending `session/prompt` handler instead.
            }
            _ => {
                if id.is_some() {
                    error(id, -32601, &format!("Unsupported method: {method}"));
                }
            }
        }
    }
}

fn handle_prompt(
    mode: &str,
    id: Option<serde_json::Value>,
    message: &serde_json::Value,
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
) {
    match mode {
        "permission" => {
            notify_update(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_1",
                "title": "Run `echo hi`",
                "kind": "execute",
                "rawInput": { "command": "echo hi" },
            }));
            let answer = request_permission(lines);
            notify_update(serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": format!("permission:{answer}") },
            }));
            reply(id, serde_json::json!({ "stopReason": "end_turn" }));
        }
        "cancel" => {
            notify_update(serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "working" },
            }));
            // Answer only once the client sends `session/cancel`, which is what
            // proves the client actually sent it rather than just walking away.
            for line in lines.by_ref().map_while(Result::ok) {
                if line.contains("session/cancel") {
                    if let Ok(marker) = std::env::var("THREADLANE_STUB_CANCEL_MARKER") {
                        let _ = std::fs::write(marker, "cancelled");
                    }
                    reply(id, serde_json::json!({ "stopReason": "cancelled" }));
                    return;
                }
            }
        }
        "no_images" => {
            let text = prompt_text(message);
            notify_update(serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": format!("echo:{text}") },
            }));
            reply(id, serde_json::json!({ "stopReason": "end_turn" }));
        }
        _ => {
            notify_update(serde_json::json!({
                "sessionUpdate": "agent_thought_chunk",
                "content": { "type": "text", "text": "thinking" },
            }));
            notify_update(serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "hello " },
            }));
            notify_update(serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "world" },
            }));
            notify_update(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call_1",
                "title": "Read main.rs",
                "kind": "read",
                "rawInput": { "path": "src/main.rs" },
            }));
            notify_update(serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call_1",
                "status": "completed",
                "content": [{
                    "type": "content",
                    "content": { "type": "text", "text": "fn main() {}" },
                }],
            }));
            // An update addressed to another session must not reach this turn.
            send(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "someone-else",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": "LEAKED" },
                    },
                },
            }));
            reply(id, serde_json::json!({ "stopReason": "end_turn" }));
        }
    }
}

/// The settings this stub exposes, shaped like a real agent's.
fn config_options(model: &str, effort: &str) -> serde_json::Value {
    serde_json::json!([
        {
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "default",
            "options": [
                {
                    "value": "default",
                    "name": "Default",
                    "description": "Standard behavior, prompts for dangerous operations",
                },
                { "value": "plan", "name": "Plan Mode", "description": "Planning mode, no actual tool execution" },
            ],
        },
        {
            "id": "agent",
            "name": "Agent",
            "type": "select",
            "currentValue": "default",
            "options": [
                { "value": "default", "name": "Default" },
                { "value": "some-persona", "name": "some-persona" },
            ],
        },
        {
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": model,
            "options": [
                { "value": "default", "name": "Default (recommended)", "description": "Opus 4.8 · Best for everyday tasks" },
                { "value": "opus", "name": "Opus", "description": "Opus 4.8" },
                { "value": "sonnet", "name": "Sonnet" },
            ],
        },
        {
            "id": "effort",
            "name": "Effort",
            "category": "thought_level",
            "type": "select",
            "currentValue": effort,
            "options": [
                { "value": "default", "name": "Default" },
                { "value": "low", "name": "Low" },
                { "value": "medium", "name": "Medium" },
                { "value": "high", "name": "High" },
            ],
        },
    ])
}

/// Concatenates the text blocks of a prompt.
fn prompt_text(message: &serde_json::Value) -> String {
    message
        .get("params")
        .and_then(|params| params.get("prompt"))
        .and_then(|prompt| prompt.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Asks the client to approve a tool call and returns the chosen option id.
///
/// Reads from the caller's iterator rather than locking stdin again: the lock
/// is not reentrant, so a second `stdin().lock()` on this thread would hang.
fn request_permission(lines: &mut std::io::Lines<std::io::StdinLock<'_>>) -> String {
    send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 9001,
        "method": "session/request_permission",
        "params": {
            "sessionId": "stub-session",
            "toolCall": {
                "toolCallId": "call_1",
                "title": "Run `echo hi`",
                "kind": "execute",
                "rawInput": { "command": "echo hi" },
            },
            "options": [
                { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                { "optionId": "no", "name": "Deny", "kind": "reject_once" },
            ],
        },
    }));

    for line in lines.by_ref().map_while(Result::ok) {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if message.get("id").and_then(|id| id.as_u64()) != Some(9001) {
            continue;
        }
        return message
            .get("result")
            .and_then(|result| result.get("outcome"))
            .and_then(|outcome| outcome.get("optionId"))
            .and_then(|id| id.as_str())
            .unwrap_or("cancelled")
            .to_string();
    }
    "closed".to_string()
}

fn notify_update(update: serde_json::Value) {
    send(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": { "sessionId": "stub-session", "update": update },
    }));
}

fn reply(id: Option<serde_json::Value>, result: serde_json::Value) {
    send(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn error(id: Option<serde_json::Value>, code: i32, message: &str) {
    send(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }));
}

fn send(message: serde_json::Value) {
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}
