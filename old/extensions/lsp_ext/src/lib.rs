mod editor;

use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct WasiToolDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct WasiExtensionManifest {
    api_version: u32,
    name: String,
    version: String,
    description: String,
    capabilities: Vec<String>,
    tools: Vec<WasiToolDefinition>,
    commands: Vec<serde_json::Value>,
    hooks: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Invocation {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
    #[serde(default)]
    state: serde_json::Value,
    #[serde(default)]
    events: Vec<ExtensionEvent>,
}

#[derive(Debug, Deserialize)]
struct ExtensionEvent {
    topic: String,
    #[serde(default)]
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct BrokerResponsePayload {
    capability: String,
    operation: String,
    ok: bool,
    #[serde(default)]
    value: Option<BrokerResponseValue>,
    #[serde(default)]
    error: Option<BrokerResponseError>,
}

#[derive(Debug, Deserialize)]
struct BrokerResponseValue {
    message: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct BrokerResponseError {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ProcessRunResult {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct BrokerRequest {
    api_version: u32,
    capability: String,
    operation: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize, PartialEq)]
struct Response {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    continue_after_broker: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<serde_json::Value>,
}

impl Response {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error: None,
            continue_after_broker: false,
            state: None,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        let msg = message.into();
        Self {
            message: msg.clone(),
            error: Some(msg),
            continue_after_broker: false,
            state: None,
        }
    }

    fn continue_after_broker(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error: None,
            continue_after_broker: true,
            state: None,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

const CARGO_CHECK_TIMEOUT_MS: u64 = 120_000;
const CARGO_CHECK_MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PROCESS_FAILURE_DETAIL_CHARS: usize = 400;

fn parse_cargo_diagnostics(json_output: &str, target_path: &str) -> (usize, usize, Vec<String>) {
    let mut errors = 0;
    let mut warnings = 0;
    let mut formatted = Vec::new();

    let target_clean = target_path.replace('\\', "/");
    let is_workspace_wide = target_clean.is_empty() || target_clean == "." || target_clean == "./";

    for line in json_output.lines() {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if val.get("reason").and_then(|v| v.as_str()) != Some("compiler-message") {
            continue;
        }

        let Some(msg) = val.get("message") else {
            continue;
        };

        let level = msg.get("level").and_then(|v| v.as_str()).unwrap_or("info");
        let text_msg = msg.get("message").and_then(|v| v.as_str()).unwrap_or("");

        let spans = msg.get("spans").and_then(|v| v.as_array());
        let mut matched_file = String::new();
        let mut line_no = 0;
        let mut col_no = 0;

        if let Some(spans) = spans {
            for span in spans {
                if let Some(file_name) = span.get("file_name").and_then(|v| v.as_str()) {
                    let file_clean = file_name.replace('\\', "/");
                    if is_workspace_wide
                        || file_clean.ends_with(&target_clean)
                        || target_clean.ends_with(&file_clean)
                    {
                        matched_file = file_clean;
                        line_no = span.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0);
                        col_no = span
                            .get("column_start")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        break;
                    }
                }
            }
        }

        if !matched_file.is_empty() || is_workspace_wide {
            let file_label = if matched_file.is_empty() {
                String::new()
            } else {
                format!("{matched_file}:")
            };
            if level == "error" {
                errors += 1;
                if formatted.len() < 20 {
                    formatted.push(format!(
                        "- [ERROR] {file_label}{line_no}:{col_no}: {text_msg}"
                    ));
                }
            } else if level == "warning" {
                warnings += 1;
                if formatted.len() < 20 {
                    formatted.push(format!(
                        "- [WARNING] {file_label}{line_no}:{col_no}: {text_msg}"
                    ));
                }
            }
        }
    }

    let total_issues: usize = errors + warnings;
    if total_issues > 20 {
        formatted.push(format!(
            "... (and {} more diagnostics omitted for brevity)",
            total_issues.saturating_sub(20)
        ));
    }

    (errors, warnings, formatted)
}

impl ExtensionEvent {
    fn process_run_result(&self) -> Option<Result<ProcessRunResult, String>> {
        if self.topic != "broker_response"
            || self
                .payload
                .get("capability")
                .and_then(|value| value.as_str())
                != Some("process")
            || self
                .payload
                .get("operation")
                .and_then(|value| value.as_str())
                != Some("run")
        {
            return None;
        }

        Some(
            serde_json::from_value::<BrokerResponsePayload>(self.payload.clone())
                .map_err(|error| format!("Invalid process/run broker response: {error}"))
                .and_then(BrokerResponsePayload::into_process_run_result),
        )
    }
}

impl BrokerResponsePayload {
    fn into_process_run_result(self) -> Result<ProcessRunResult, String> {
        if self.capability != "process" || self.operation != "run" {
            return Err("Broker response did not match process/run".into());
        }

        if !self.ok {
            return Err(match self.error {
                Some(error) => format!(
                    "process/run broker error `{}`: {}",
                    error.code, error.message
                ),
                None => "process/run broker request failed without error details".into(),
            });
        }

        let message = self
            .value
            .ok_or_else(|| "process/run broker response is missing `value`".to_string())?
            .message;
        match message {
            serde_json::Value::String(encoded) => serde_json::from_str(&encoded),
            value => serde_json::from_value(value),
        }
        .map_err(|error| format!("Invalid process/run result message: {error}"))
    }
}

fn cargo_check_request() -> BrokerRequest {
    BrokerRequest {
        api_version: 2,
        capability: "process".into(),
        operation: "run".into(),
        arguments: serde_json::json!({
            "program": "cargo",
            "args": ["check", "--message-format=json"],
            "timeout_ms": CARGO_CHECK_TIMEOUT_MS,
            "max_output_bytes": CARGO_CHECK_MAX_OUTPUT_BYTES,
        }),
    }
}

fn prepare_diagnostics(
    invocation: &Invocation,
    file_path: &str,
) -> (Response, Option<BrokerRequest>) {
    if !file_path.ends_with(".rs") {
        return (
            Response::error(format!(
                "lsp_diagnostics supports only Rust (.rs) files via cargo check; '{file_path}' is unsupported. This is compiler diagnostics post-processing, not generic LSP support."
            )),
            None,
        );
    }

    if let Some(result) = invocation
        .events
        .iter()
        .find_map(ExtensionEvent::process_run_result)
    {
        let response = match result {
            Ok(process_result) => format_process_diagnostics(file_path, &process_result),
            Err(error) => Response::error(format!("Rust diagnostics failed: {error}")),
        };
        return (response, None);
    }

    (
        Response::continue_after_broker(format!(
            "Running cargo check for Rust diagnostics in '{file_path}'."
        )),
        Some(cargo_check_request()),
    )
}

fn format_process_diagnostics(file_path: &str, result: &ProcessRunResult) -> Response {
    let output = format!("{}\n{}", result.stdout, result.stderr);
    let (errors, warnings, diagnostics) = parse_cargo_diagnostics(&output, file_path);

    if errors + warnings > 0 {
        let error_label = if errors == 1 { "error" } else { "errors" };
        let warning_label = if warnings == 1 { "warning" } else { "warnings" };
        let mut message = format!(
            "Rust diagnostics for '{file_path}': {errors} {error_label}, {warnings} {warning_label}"
        );
        if !diagnostics.is_empty() {
            message.push('\n');
            message.push_str(&diagnostics.join("\n"));
        }
        return Response::ok(message);
    }

    let (all_errors, all_warnings, _) = parse_cargo_diagnostics(&output, "");
    if result.exit_code == Some(0) || all_errors + all_warnings > 0 {
        return Response::ok(format!(
            "No Rust errors or warnings found for '{file_path}'."
        ));
    }

    let status = match result.exit_code {
        Some(exit_code) => format!("exited with code {exit_code}"),
        None => "terminated without an exit code".into(),
    };
    let raw_detail = if result.stderr.trim().is_empty() {
        &result.stdout
    } else {
        &result.stderr
    };
    let detail = compact_process_output(raw_detail);
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    Response::error(format!(
        "cargo check {status} without usable diagnostics for '{file_path}'{suffix}"
    ))
}

fn compact_process_output(output: &str) -> String {
    let compact = output.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let mut bounded = chars
        .by_ref()
        .take(MAX_PROCESS_FAILURE_DETAIL_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}

fn handle_diagnostics(invocation: &Invocation, file_path: &str) -> Response {
    let (response, request) = prepare_diagnostics(invocation, file_path);
    if let Some(request) = request.as_ref() {
        send_broker_request(request);
    }
    response
}

fn lsp_state_response(message: impl Into<String>, state: serde_json::Value) -> Response {
    let mut response = Response::continue_after_broker(message);
    response.state = Some(state);
    response
}

fn lsp_request(id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn lsp_notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params})
}

fn frame_jsonrpc(payload: &serde_json::Value) -> String {
    let body = serde_json::to_string(payload).expect("JSON-RPC payload must serialize");
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

fn process_request(operation: &str, arguments: serde_json::Value) -> BrokerRequest {
    BrokerRequest {
        api_version: 2,
        capability: "process".into(),
        operation: operation.into(),
        arguments,
    }
}

fn fs_request(operation: &str, arguments: serde_json::Value) -> BrokerRequest {
    BrokerRequest {
        api_version: 2,
        capability: "fs".into(),
        operation: operation.into(),
        arguments,
    }
}

fn fs_message(event: &ExtensionEvent, operation: &str) -> Option<Result<String, String>> {
    if event.topic != "broker_response"
        || event
            .payload
            .get("capability")
            .and_then(|value| value.as_str())
            != Some("fs")
        || event
            .payload
            .get("operation")
            .and_then(|value| value.as_str())
            != Some(operation)
    {
        return None;
    }
    let payload: BrokerResponsePayload = match serde_json::from_value(event.payload.clone()) {
        Ok(payload) => payload,
        Err(error) => {
            return Some(Err(format!(
                "Invalid fs/{operation} broker response: {error}"
            )))
        }
    };
    if !payload.ok {
        return Some(Err(payload.error.map_or_else(
            || format!("fs/{operation} failed"),
            |error| {
                format!(
                    "fs/{operation} broker error {}: {}",
                    error.code, error.message
                )
            },
        )));
    }
    let Some(value) = payload.value else {
        return Some(Err(format!(
            "fs/{operation} broker response is missing value"
        )));
    };
    Some(Ok(value.message.as_str().unwrap_or_default().to_owned()))
}

#[derive(Debug, PartialEq)]
enum ProcessRecvMessage {
    Message(serde_json::Value),
    Timeout,
    Eof,
}

fn broker_message(
    event: &ExtensionEvent,
    operation: &str,
) -> Option<Result<serde_json::Value, String>> {
    let response = process_broker_message(event, operation)?;
    if operation != "recv" {
        return Some(response);
    }
    Some(response.and_then(|message| match process_recv_message(message)? {
        ProcessRecvMessage::Message(message) => Ok(message),
        ProcessRecvMessage::Timeout => {
            Err("process/recv timed out without a JSON-RPC response".into())
        }
        ProcessRecvMessage::Eof => {
            Err("process/recv reached EOF before a JSON-RPC response".into())
        }
    }))
}

fn broker_recv_message(
    event: &ExtensionEvent,
) -> Option<Result<ProcessRecvMessage, String>> {
    process_broker_message(event, "recv")
        .map(|message| message.and_then(process_recv_message))
}

fn process_broker_message(
    event: &ExtensionEvent,
    operation: &str,
) -> Option<Result<serde_json::Value, String>> {
    if event.topic != "broker_response"
        || event
            .payload
            .get("capability")
            .and_then(|value| value.as_str())
            != Some("process")
        || event
            .payload
            .get("operation")
            .and_then(|value| value.as_str())
            != Some(operation)
    {
        return None;
    }
    let payload: BrokerResponsePayload = match serde_json::from_value(event.payload.clone()) {
        Ok(payload) => payload,
        Err(error) => {
            return Some(Err(format!(
                "Invalid process/{operation} broker response: {error}"
            )))
        }
    };
    if !payload.ok {
        return Some(Err(payload.error.map_or_else(
            || format!("process/{operation} failed"),
            |error| {
                format!(
                    "process/{operation} broker error {}: {}",
                    error.code, error.message
                )
            },
        )));
    }
    let Some(value) = payload.value else {
        return Some(Err(format!(
            "process/{operation} broker response is missing value"
        )));
    };
    let message = match value.message {
        serde_json::Value::String(message) => {
            serde_json::from_str(&message).unwrap_or(serde_json::Value::String(message))
        }
        value => value,
    };
    Some(Ok(message))
}

fn process_recv_message(
    message: serde_json::Value,
) -> Result<ProcessRecvMessage, String> {
    let data = message
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "process/recv response is missing string data".to_owned())?;
    if data.is_empty() {
        return match message.get("eof").and_then(serde_json::Value::as_bool) {
            Some(true) => Ok(ProcessRecvMessage::Eof),
            Some(false) => Ok(ProcessRecvMessage::Timeout),
            None => Err("process/recv response is missing its EOF flag".into()),
        };
    }
    serde_json::from_str(data)
        .map(ProcessRecvMessage::Message)
        .map_err(|error| format!("Invalid process/recv JSON-RPC payload: {error}"))
}

fn lsp_language_id(path: &str) -> &'static str {
    match detect_lsp_server(path) {
        "rust-analyzer" => "rust",
        "typescript-language-server" => "typescript",
        "gopls" => "go",
        "pyright" => "python",
        _ => "plaintext",
    }
}

fn workspace_root(absolute_path: &str, path: &str) -> Result<String, Response> {
    let components = Path::new(path)
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    let mut root = Path::new(absolute_path).to_path_buf();
    for _ in 0..components {
        root.pop();
    }
    root.to_str()
        .filter(|root| !root.is_empty())
        .map(|root| root.trim_end_matches('/').to_owned())
        .ok_or_else(|| Response::error("Could not determine workspace root."))
}

fn file_uri(path: &str) -> String {
    format!("file:///{}", path.trim_start_matches('/'))
}

fn lsp_position(arguments: &serde_json::Value) -> Result<serde_json::Value, Response> {
    let line = arguments
        .get("line")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Response::error("'line' parameter is required."))?;
    let character = arguments
        .get("character")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Response::error("'character' parameter is required."))?;
    if line == 0 || character == 0 {
        return Err(Response::error(
            "'line' and 'character' are 1-indexed positive integers.",
        ));
    }
    Ok(serde_json::json!({"line": line - 1, "character": character - 1}))
}

fn lsp_offset(text: &str, line: u64, character: u64) -> Result<usize, String> {
    let line = usize::try_from(line).map_err(|_| "line is too large")?;
    let character = usize::try_from(character).map_err(|_| "character is too large")?;
    let start = text
        .split_inclusive('\n')
        .take(line)
        .map(str::len)
        .sum::<usize>();
    let current = text.get(start..).ok_or("line is outside the document")?;
    let mut units = 0;
    for (offset, ch) in current.char_indices() {
        if ch == '\n' || units == character {
            return Ok(start + offset);
        }
        units += ch.len_utf16();
        if units > character {
            return Err("character splits a UTF-16 code point".into());
        }
    }
    if units == character {
        Ok(text.len())
    } else {
        Err("character is outside the line".into())
    }
}

fn apply_text_edits(text: &str, edits: &serde_json::Value) -> Result<String, String> {
    let edits = edits.as_array().ok_or("LSP edits must be an array")?;
    let mut edits = edits
        .iter()
        .map(|edit| {
            let range = edit.get("range").ok_or("LSP edit is missing range")?;
            let start = range
                .get("start")
                .ok_or("LSP edit is missing range.start")?;
            let end = range.get("end").ok_or("LSP edit is missing range.end")?;
            let start = lsp_offset(
                text,
                start
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("LSP edit start line is missing")?,
                start
                    .get("character")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("LSP edit start character is missing")?,
            )?;
            let end = lsp_offset(
                text,
                end.get("line")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("LSP edit end line is missing")?,
                end.get("character")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("LSP edit end character is missing")?,
            )?;
            if start > end {
                return Err("LSP edit range is reversed".into());
            }
            Ok((
                start,
                end,
                edit.get("newText")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut output = text.to_owned();
    for (start, end, replacement) in edits {
        output.replace_range(start..end, &replacement);
    }
    Ok(output)
}

fn workspace_relative_path(state: &serde_json::Value, uri: &str) -> Result<String, String> {
    let absolute = uri
        .strip_prefix("file://")
        .ok_or("LSP edit URI is not a file URI")?;
    let root = state["workspace_root"]
        .as_str()
        .ok_or("Missing workspace root")?;
    absolute
        .strip_prefix(root)
        .and_then(|path| path.strip_prefix('/').or(Some(path)))
        .filter(|path| !path.is_empty() && !path.starts_with("../"))
        .map(str::to_owned)
        .ok_or_else(|| "LSP edit escapes workspace".into())
}

fn path_matches_uri(path: &str, uri: &str, workspace_path: &str) -> bool {
    path == workspace_path || uri.strip_prefix("file://") == Some(path)
}

fn workspace_edit_changes(
    edit: &serde_json::Value,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    if let Some(changes) = edit.get("changes").and_then(serde_json::Value::as_object) {
        return Ok(changes.clone());
    }
    let mut changes = serde_json::Map::new();
    for change in edit
        .get("documentChanges")
        .and_then(serde_json::Value::as_array)
        .ok_or("LSP workspace edit has no changes")?
    {
        let uri = change
            .get("textDocument")
            .and_then(|document| document.get("uri"))
            .and_then(serde_json::Value::as_str)
            .ok_or("LSP document change is missing URI")?;
        let edits = change
            .get("edits")
            .cloned()
            .ok_or("LSP document change is missing edits")?;
        changes.insert(uri.to_owned(), edits);
    }
    Ok(changes)
}

const MAX_LSP_PUMP_STEPS: u64 = 200;

enum LspReply {
    Complete(serde_json::Value),
    Pending(Vec<BrokerRequest>),
}

/// Notifications, server requests, and stale replies can precede our reply.
/// They do not advance the operation, and each consumes the bounded pump.
fn receive_lsp_reply(
    invocation: &Invocation,
    state: &mut serde_json::Value,
) -> Result<LspReply, Response> {
    let message = invocation
        .events
        .iter()
        .find_map(|event| broker_message(event, "recv"))
        .ok_or_else(|| Response::error("Missing LSP response from language server."))?
        .map_err(Response::error)?;
    if message["jsonrpc"] != "2.0" {
        return Err(Response::error(
            "Invalid JSON-RPC version from language server.",
        ));
    }
    let method = message.get("method").and_then(serde_json::Value::as_str);
    if method.is_none()
        && (message.get("id").is_none()
            || message.get("result").is_some() == message.get("error").is_some())
    {
        return Err(Response::error(
            "Invalid LSP response: expected id and result or error.",
        ));
    }
    let pending_id = state["pending_request_id"]
        .as_u64()
        .ok_or_else(|| Response::error("Missing pending LSP request id."))?;
    if method.is_none() && message["id"].as_u64() == Some(pending_id) {
        if message.get("result").is_some() == message.get("error").is_some() {
            return Err(Response::error(
                "Invalid LSP response: expected result or error.",
            ));
        }
        if let Some(error) = message.get("error") {
            let code = error["code"]
                .as_i64()
                .ok_or_else(|| Response::error("Invalid LSP error code."))?;
            let detail: String = error["message"]
                .as_str()
                .ok_or_else(|| Response::error("Invalid LSP error message."))?
                .chars()
                .take(MAX_PROCESS_FAILURE_DETAIL_CHARS)
                .collect();
            let mut response = Response::error(format!("Language server error {code}: {detail}"));
            // A rejected query does not invalidate a successfully initialized server.
            if state["phase"] == "requesting" {
                state["phase"] = serde_json::json!("ready");
                state["pump_steps"] = serde_json::json!(0);
                response.state = Some(state.clone());
            }
            return Err(response);
        }
        state["pump_steps"] = serde_json::json!(0);
        return Ok(LspReply::Complete(message));
    }
    let steps = state["pump_steps"].as_u64().unwrap_or(0).saturating_add(1);
    if steps > MAX_LSP_PUMP_STEPS {
        return Err(Response::error(
            "LSP response budget exhausted; language server must be restarted.",
        ));
    }
    state["pump_steps"] = serde_json::json!(steps);
    let name = state["process_name"]
        .as_str()
        .ok_or_else(|| Response::error("Missing LSP process name."))?;
    let mut requests = Vec::new();
    if method.is_some() {
        if let Some(id) = message.get("id") {
            let reply = serde_json::json!({"jsonrpc":"2.0", "id":id,
                "error":{"code":-32601, "message":"Client method not supported"}});
            requests.push(process_request(
                "send",
                serde_json::json!({"name":name, "data":frame_jsonrpc(&reply)}),
            ));
        }
    }
    requests.push(process_request(
        "recv",
        serde_json::json!({"name":name, "framing":"content-length", "timeout_ms":30_000}),
    ));
    Ok(LspReply::Pending(requests))
}

fn prepare_lsp(invocation: &Invocation) -> Result<(Response, Vec<BrokerRequest>), Response> {
    prepare_lsp_step(invocation).map_err(|mut response| {
        if response.state.is_none() && invocation.state["process_name"].is_string() {
            let mut state = invocation.state.clone();
            // Local argument/file validation before dispatch does not break
            // an initialized server. Transport/protocol failures do.
            let phase = match invocation.state["phase"].as_str() {
                Some("ready" | "format_refreshed" | "refreshing_format") => "ready",
                _ => "failed",
            };
            state["phase"] = serde_json::json!(phase);
            response.state = Some(state);
        }
        response
    })
}

fn prepare_lsp_step(invocation: &Invocation) -> Result<(Response, Vec<BrokerRequest>), Response> {
    let path = invocation
        .arguments
        .get("path")
        .and_then(|value| value.as_str())
        .filter(|path| !path.is_empty())
        .ok_or_else(|| Response::error("'path' parameter is required."))?;
    let mut state = invocation.state.clone();
    let phase = state
        .get("phase")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let server = detect_lsp_server(path);
    let process_name = format!("lsp-{server}");

    match phase {
        "" | "failed" => {
            let mut requests = Vec::new();
            if phase == "failed" {
                if let Some(name) = state["process_name"].as_str() {
                    requests.push(process_request("kill", serde_json::json!({"name":name})));
                }
            }
            state = serde_json::json!({
                "phase": "spawning",
                "server": server,
                "process_name": process_name,
                "next_request_id": 1u64,
            });
            requests.extend([
                    process_request(
                        "spawn",
                        serde_json::json!({
                            "name": process_name, "program": server, "args": [],
                        }),
                    ),
                    fs_request("read_text", serde_json::json!({"path": path})),
                    fs_request("absolute_path", serde_json::json!({"path": path})),
                ]);
            Ok((lsp_state_response(format!("Starting {server}."), state), requests))
        }
        "spawning" => {
            match invocation
                .events
                .iter()
                .find_map(|event| broker_message(event, "spawn"))
            {
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(Response::error(error)),
                None => return Err(Response::error("Missing process/spawn broker response.")),
            }
            let text = match invocation
                .events
                .iter()
                .find_map(|event| fs_message(event, "read_text"))
            {
                Some(Ok(text)) => text,
                Some(Err(error)) => return Err(Response::error(error)),
                None => return Err(Response::error("Missing fs/read_text broker response.")),
            };
            let absolute_path = match invocation
                .events
                .iter()
                .find_map(|event| fs_message(event, "absolute_path"))
            {
                Some(Ok(path)) => path,
                Some(Err(error)) => return Err(Response::error(error)),
                None => return Err(Response::error("Missing fs/absolute_path broker response.")),
            };
            state["document_text"] = serde_json::Value::String(text);
            state["uri"] = serde_json::Value::String(file_uri(&absolute_path));
            state["path"] = serde_json::Value::String(path.to_owned());
            state["workspace_root"] =
                serde_json::Value::String(workspace_root(&absolute_path, path)?);
            state["phase"] = serde_json::Value::String("initializing".into());
            let request_id = state["next_request_id"].as_u64().unwrap_or(1);
            state["next_request_id"] = serde_json::json!(request_id + 1);
            state["pending_request_id"] = serde_json::json!(request_id);
            let initialize = lsp_request(
                request_id,
                "initialize",
                serde_json::json!({
                    "processId": null, "rootUri": null, "capabilities": {},
                }),
            );
            let name = state["process_name"]
                .as_str()
                .unwrap_or(&process_name)
                .to_owned();
            Ok((
                lsp_state_response(format!("Initializing {server}."), state),
                vec![
                    process_request(
                        "send",
                        serde_json::json!({"name": name, "data": frame_jsonrpc(&initialize)}),
                    ),
                    process_request(
                        "recv",
                        serde_json::json!({"name": name, "framing": "content-length", "timeout_ms": 30_000}),
                    ),
                ],
            ))
        }
        "initializing" => {
            if let LspReply::Pending(requests) = receive_lsp_reply(invocation, &mut state)? {
                return Ok((lsp_state_response("Waiting for initialize response.", state), requests));
            }
            state["phase"] = serde_json::Value::String("requesting".into());
            let request_id = state["next_request_id"].as_u64().unwrap_or(2);
            state["next_request_id"] = serde_json::json!(request_id + 1);
            state["pending_request_id"] = serde_json::json!(request_id);
            let method = match invocation.name.as_str() {
                "lsp_definition" => "textDocument/definition",
                "lsp_type_definition" => "textDocument/typeDefinition",
                "lsp_implementation" => "textDocument/implementation",
                "lsp_references" => "textDocument/references",
                "lsp_hover" => "textDocument/hover",
                "lsp_code_actions" => "textDocument/codeAction",
                "lsp_symbols" => "textDocument/documentSymbol",
                "lsp_format" => "textDocument/formatting",
                "lsp_rename" => "textDocument/rename",
                "lsp_rename_file" => "workspace/willRenameFiles",
                _ => {
                    return Err(Response::error(format!(
                        "Unsupported LSP request {}",
                        invocation.name
                    )))
                }
            };
            let uri = state["uri"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| file_uri(path));
            let position = if matches!(
                method,
                "textDocument/documentSymbol"
                    | "textDocument/formatting"
                    | "workspace/willRenameFiles"
            ) {
                None
            } else {
                Some(lsp_position(&invocation.arguments)?)
            };
            let params = if method == "workspace/willRenameFiles" {
                let root = state["workspace_root"].as_str().unwrap_or("");
                let from = invocation.arguments["old_path"]
                    .as_str()
                    .ok_or_else(|| Response::error("'old_path' parameter is required."))?;
                let to = invocation.arguments["new_path"]
                    .as_str()
                    .ok_or_else(|| Response::error("'new_path' parameter is required."))?;
                serde_json::json!({"files": [{"oldUri": file_uri(&format!("{root}/{from}")), "newUri": file_uri(&format!("{root}/{to}"))}]})
            } else if method == "textDocument/documentSymbol" {
                serde_json::json!({"textDocument": {"uri": uri}})
            } else if method == "textDocument/formatting" {
                serde_json::json!({"textDocument": {"uri": uri}, "options": {"tabSize": 4, "insertSpaces": true}})
            } else if method == "textDocument/rename" {
                serde_json::json!({"textDocument": {"uri": uri}, "position": position.clone().unwrap(), "newName": invocation.arguments["new_name"]})
            } else if method == "textDocument/references" {
                serde_json::json!({"textDocument": {"uri": uri}, "position": position.clone().unwrap(), "context": {"includeDeclaration": true}})
            } else if method == "textDocument/codeAction" {
                serde_json::json!({"textDocument": {"uri": uri}, "range": {"start": position.clone().unwrap(), "end": position.unwrap()}, "context": {"diagnostics": []}})
            } else {
                serde_json::json!({"textDocument": {"uri": uri}, "position": position.unwrap()})
            };
            let request = lsp_request(request_id, method, params);
            let did_open = lsp_notification(
                "textDocument/didOpen",
                serde_json::json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": lsp_language_id(path),
                        "version": 1,
                        "text": state["document_text"].as_str().unwrap_or("")
                    }
                }),
            );
            let name = state["process_name"]
                .as_str()
                .unwrap_or(&process_name)
                .to_owned();
            Ok((
                lsp_state_response(format!("Querying {server}."), state),
                vec![
                    process_request(
                        "send",
                        serde_json::json!({"name": name, "data": frame_jsonrpc(&lsp_notification("initialized", serde_json::json!({})))}),
                    ),
                    process_request(
                        "send",
                        serde_json::json!({"name": name, "data": frame_jsonrpc(&did_open)}),
                    ),
                    process_request(
                        "send",
                        serde_json::json!({"name": name, "data": frame_jsonrpc(&request)}),
                    ),
                    process_request(
                        "recv",
                        serde_json::json!({"name": name, "framing": "content-length", "timeout_ms": 30_000}),
                    ),
                ],
            ))
        }
        "requesting" => {
            let message = match receive_lsp_reply(invocation, &mut state)? {
                LspReply::Complete(message) => message,
                LspReply::Pending(requests) => return Ok((lsp_state_response("Waiting for LSP response.", state), requests)),
            };
            let result = message
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if invocation.name == "lsp_format" {
                let path = state["path"]
                    .as_str()
                    .ok_or_else(|| Response::error("Missing LSP document path."))?;
                let plan = serde_json::json!({
                    "version": 1,
                    "kind": "lsp_workspace_edit_plan",
                    "operation": "format",
                    "files": [{"path": path, "text_edits": result}],
                    "requires_atomic_apply": true
                });
                state["phase"] = serde_json::Value::String("ready".into());
                let mut response = Response::ok(plan.to_string());
                response.state = Some(state);
                return Ok((response, vec![]));
            }
            if invocation.name == "lsp_rename_file" {
                let from = invocation.arguments["old_path"]
                    .as_str()
                    .ok_or_else(|| Response::error("'old_path' parameter is required."))?;
                let to = invocation.arguments["new_path"]
                    .as_str()
                    .ok_or_else(|| Response::error("'new_path' parameter is required."))?;
                if let Ok(changes) = workspace_edit_changes(&result) {
                    let mut pending = serde_json::Map::new();
                    for (uri, edits) in &changes {
                        pending.insert(
                            workspace_relative_path(&state, uri).map_err(Response::error)?,
                            edits.clone(),
                        );
                    }
                    state["pending_edits"] = serde_json::Value::Object(pending);
                    state["phase"] = serde_json::Value::String("reading_file_rename".into());
                    let requests = state["pending_edits"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .map(|path| fs_request("read_text", serde_json::json!({"path": path})))
                        .collect();
                    return Ok((
                        lsp_state_response("Reading files for willRenameFiles edits.", state),
                        requests,
                    ));
                }
                state["phase"] = serde_json::Value::String("moving_file".into());
                return Ok((
                    lsp_state_response("Moving file after LSP willRenameFiles.", state),
                    vec![fs_request(
                        "rename",
                        serde_json::json!({"from": from, "to": to}),
                    )],
                ));
            }
            if invocation.name == "lsp_rename" {
                let changes = workspace_edit_changes(&result).map_err(Response::error)?;
                let mut files = Vec::with_capacity(changes.len());
                for (uri, edits) in changes {
                    files.push(serde_json::json!({
                        "path": workspace_relative_path(&state, &uri).map_err(Response::error)?,
                        "text_edits": edits
                    }));
                }
                let plan = serde_json::json!({
                    "version": 1,
                    "kind": "lsp_workspace_edit_plan",
                    "operation": "rename",
                    "files": files,
                    "requires_atomic_apply": true
                });
                state["phase"] = serde_json::Value::String("ready".into());
                let mut response = Response::ok(plan.to_string());
                response.state = Some(state);
                return Ok((response, vec![]));
            }
            state["phase"] = serde_json::Value::String("ready".into());
            let mut response = Response::ok(format!("{} result:\n{}", invocation.name, result));
            response.state = Some(state);
            Ok((response, vec![]))
        }
        "writing" => {
            state["phase"] = serde_json::Value::String("ready".into());
            let mut response = Response::ok("Applied LSP formatting edits.");
            response.state = Some(state);
            Ok((response, vec![]))
        }
        "moving_file" => {
            let root = state["workspace_root"].as_str().unwrap_or("");
            let from = invocation.arguments["old_path"]
                .as_str()
                .ok_or_else(|| Response::error("'old_path' parameter is required."))?;
            let to = invocation.arguments["new_path"]
                .as_str()
                .ok_or_else(|| Response::error("'new_path' parameter is required."))?;
            let name = state["process_name"]
                .as_str()
                .unwrap_or(&process_name)
                .to_owned();
            let notification = lsp_notification(
                "workspace/didRenameFiles",
                serde_json::json!({
                    "files": [{"oldUri": file_uri(&format!("{root}/{from}")), "newUri": file_uri(&format!("{root}/{to}"))}]
                }),
            );
            state["phase"] = serde_json::Value::String("notifying_file_move".into());
            Ok((
                lsp_state_response("Notifying LSP server of file move.", state),
                vec![process_request(
                    "send",
                    serde_json::json!({"name": name, "data": frame_jsonrpc(&notification)}),
                )],
            ))
        }
        "reading_file_rename" => {
            let pending = state["pending_edits"]
                .as_object()
                .ok_or_else(|| Response::error("Missing willRenameFiles edits."))?;
            let mut writes = Vec::new();
            for event in &invocation.events {
                let Some(Ok(text)) = fs_message(event, "read_text") else {
                    continue;
                };
                let Some(path) = event
                    .payload
                    .get("arguments")
                    .and_then(|value| value.get("path"))
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                let edits = pending
                    .get(path)
                    .ok_or_else(|| Response::error("Unexpected willRenameFiles read."))?;
                writes.push(fs_request(
                    "write_text",
                    serde_json::json!({
                        "path": path,
                        "content": apply_text_edits(&text, edits).map_err(Response::error)?,
                    }),
                ));
            }
            if writes.len() != pending.len() {
                return Err(Response::error(
                    "willRenameFiles did not read every edited file.",
                ));
            }
            state["phase"] = serde_json::Value::String("ready_to_move_file".into());
            Ok((
                lsp_state_response("Applying willRenameFiles edits.", state),
                writes,
            ))
        }
        "reading_workspace_rename" => {
            let pending = state["pending_edits"]
                .as_object()
                .ok_or_else(|| Response::error("Missing LSP rename edits."))?;
            let mut writes = Vec::new();
            for event in &invocation.events {
                let Some(Ok(text)) = fs_message(event, "read_text") else {
                    continue;
                };
                let Some(path) = event
                    .payload
                    .get("arguments")
                    .and_then(|value| value.get("path"))
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                let edits = pending
                    .get(path)
                    .ok_or_else(|| Response::error("Unexpected LSP rename read."))?;
                writes.push(fs_request(
                    "write_text",
                    serde_json::json!({
                        "path": path,
                        "content": apply_text_edits(&text, edits).map_err(Response::error)?,
                    }),
                ));
            }
            if writes.len() != pending.len() {
                return Err(Response::error(
                    "LSP rename did not read every edited file.",
                ));
            }
            state["phase"] = serde_json::Value::String("writing_workspace_rename".into());
            Ok((
                lsp_state_response("Applying LSP workspace rename edits.", state),
                writes,
            ))
        }
        "writing_workspace_rename" => {
            state
                .as_object_mut()
                .map(|state| state.remove("pending_edits"));
            state["phase"] = serde_json::Value::String("ready".into());
            let mut response = Response::ok("Applied LSP workspace rename edits.");
            response.state = Some(state);
            Ok((response, vec![]))
        }
        "ready_to_move_file" => {
            state
                .as_object_mut()
                .map(|state| state.remove("pending_edits"));
            let from = invocation.arguments["old_path"]
                .as_str()
                .ok_or_else(|| Response::error("'old_path' parameter is required."))?;
            let to = invocation.arguments["new_path"]
                .as_str()
                .ok_or_else(|| Response::error("'new_path' parameter is required."))?;
            state["phase"] = serde_json::Value::String("moving_file".into());
            Ok((
                lsp_state_response("Moving file after willRenameFiles edits.", state),
                vec![fs_request(
                    "rename",
                    serde_json::json!({"from": from, "to": to}),
                )],
            ))
        }
        "notifying_file_move" => {
            state["phase"] = serde_json::Value::String("ready".into());
            let mut response =
                Response::ok("Applied LSP file rename and notified the language server.");
            response.state = Some(state);
            Ok((response, vec![]))
        }
        "ready" | "format_refreshed" => {
            let refreshed_format = phase == "format_refreshed";
            if invocation.name == "lsp_format" && !refreshed_format {
                state["phase"] = serde_json::Value::String("refreshing_format".into());
                let path = state["path"]
                    .as_str()
                    .ok_or_else(|| Response::error("Missing LSP document path."))?
                    .to_owned();
                return Ok((
                    lsp_state_response("Refreshing document before formatting.", state),
                    vec![fs_request("read_text", serde_json::json!({"path": path}))],
                ));
            }
            state["phase"] = serde_json::Value::String("requesting".into());
            let request_id = state["next_request_id"].as_u64().unwrap_or(2);
            state["next_request_id"] = serde_json::json!(request_id + 1);
            state["pending_request_id"] = serde_json::json!(request_id);
            let method = match invocation.name.as_str() {
                "lsp_definition" => "textDocument/definition",
                "lsp_type_definition" => "textDocument/typeDefinition",
                "lsp_implementation" => "textDocument/implementation",
                "lsp_references" => "textDocument/references",
                "lsp_hover" => "textDocument/hover",
                "lsp_code_actions" => "textDocument/codeAction",
                "lsp_symbols" => "textDocument/documentSymbol",
                "lsp_format" => "textDocument/formatting",
                "lsp_rename" => "textDocument/rename",
                "lsp_rename_file" => "workspace/willRenameFiles",
                _ => {
                    return Err(Response::error(format!(
                        "Unsupported LSP request {}",
                        invocation.name
                    )))
                }
            };
            let uri = state["uri"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| file_uri(path));
            let position = if matches!(
                method,
                "textDocument/documentSymbol"
                    | "textDocument/formatting"
                    | "workspace/willRenameFiles"
            ) {
                None
            } else {
                Some(lsp_position(&invocation.arguments)?)
            };
            let params = if method == "workspace/willRenameFiles" {
                let root = state["workspace_root"].as_str().unwrap_or("");
                let from = invocation.arguments["old_path"]
                    .as_str()
                    .ok_or_else(|| Response::error("'old_path' parameter is required."))?;
                let to = invocation.arguments["new_path"]
                    .as_str()
                    .ok_or_else(|| Response::error("'new_path' parameter is required."))?;
                serde_json::json!({"files": [{"oldUri": file_uri(&format!("{root}/{from}")), "newUri": file_uri(&format!("{root}/{to}"))}]})
            } else if method == "textDocument/documentSymbol" {
                serde_json::json!({"textDocument": {"uri": uri}})
            } else if method == "textDocument/formatting" {
                serde_json::json!({"textDocument": {"uri": uri}, "options": {"tabSize": 4, "insertSpaces": true}})
            } else if method == "textDocument/rename" {
                serde_json::json!({"textDocument": {"uri": uri}, "position": position.clone().unwrap(), "newName": invocation.arguments["new_name"]})
            } else if method == "textDocument/references" {
                serde_json::json!({"textDocument": {"uri": uri}, "position": position.clone().unwrap(), "context": {"includeDeclaration": true}})
            } else if method == "textDocument/codeAction" {
                serde_json::json!({"textDocument": {"uri": uri}, "range": {"start": position.clone().unwrap(), "end": position.unwrap()}, "context": {"diagnostics": []}})
            } else {
                serde_json::json!({"textDocument": {"uri": uri}, "position": position.unwrap()})
            };
            let request = lsp_request(request_id, method, params);
            let name = state["process_name"]
                .as_str()
                .unwrap_or(&process_name)
                .to_owned();
            let mut requests = Vec::new();
            if refreshed_format {
                // Request ids increase across queries, so they also provide a
                // monotonically increasing document version after didOpen(1).
                let did_change = lsp_notification(
                    "textDocument/didChange",
                    serde_json::json!({
                        "textDocument": {"uri": uri, "version": request_id},
                        "contentChanges": [{"text": state["document_text"]}],
                    }),
                );
                requests.push(process_request(
                    "send",
                    serde_json::json!({"name": name, "data": frame_jsonrpc(&did_change)}),
                ));
            }
            requests.extend([
                process_request(
                    "send",
                    serde_json::json!({"name": name, "data": frame_jsonrpc(&request)}),
                ),
                process_request(
                    "recv",
                    serde_json::json!({"name": name, "framing": "content-length", "timeout_ms": 30_000}),
                ),
            ]);
            Ok((
                lsp_state_response(format!("Querying {server}."), state),
                requests,
            ))
        }
        "refreshing_format" => {
            let text = match invocation
                .events
                .iter()
                .find_map(|event| fs_message(event, "read_text"))
            {
                Some(Ok(text)) => text,
                Some(Err(error)) => return Err(Response::error(error)),
                None => return Err(Response::error("Missing fs/read_text broker response.")),
            };
            state["document_text"] = serde_json::Value::String(text);
            state["phase"] = serde_json::Value::String("format_refreshed".into());
            prepare_lsp(&Invocation {
                name: invocation.name.clone(),
                arguments: invocation.arguments.clone(),
                state,
                events: vec![],
            })
        }
        _ => Err(Response::error(format!("Unknown LSP state phase {phase}"))),
    }
}

fn send_broker_request(request: &BrokerRequest) {
    #[cfg(target_arch = "wasm32")]
    {
        let request = serde_json::to_vec(request).expect("broker request must serialize");
        let request_ptr = alloc(request.len() as i32);
        let response_ptr = alloc(8192);
        unsafe {
            std::ptr::copy_nonoverlapping(request.as_ptr(), request_ptr as *mut u8, request.len());
            let _ = broker_request(request_ptr, request.len() as i32, response_ptr, 8192);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = request;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "threadlane_host")]
extern "C" {
    #[link_name = "request"]
    fn broker_request(
        request_ptr: i32,
        request_len: i32,
        response_ptr: i32,
        response_capacity: i32,
    ) -> i32;
}

static OUTPUT_BUF: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let mut buf = vec![0u8; size as usize];
    let ptr = buf.as_mut_ptr() as i32;
    std::mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn extension_info() -> u64 {
    write_output(&extension_manifest())
}

fn extension_manifest() -> WasiExtensionManifest {
    WasiExtensionManifest {
        api_version: 2,
        name: "lsp_ext".into(),
        version: "0.1.0".into(),
        description: "IDE Language Server Protocol (LSP) extension for threadlane".into(),
        capabilities: vec!["process".into(), "fs".into()],
        tools: vec![
            WasiToolDefinition {
                name: "lsp_definition".into(),
                description: "Locate definition/declaration of a symbol at file path, line, character using LSP.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Target file path" },
                        "line": { "type": "integer", "description": "1-indexed line number" },
                        "character": { "type": "integer", "description": "1-indexed character column offset" }
                    },
                    "required": ["path", "line", "character"]
                }),
            },
            WasiToolDefinition {
                name: "lsp_references".into(),
                description: "Find all workspace usages/references of a symbol at file path, line, character using LSP.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Target file path" },
                        "line": { "type": "integer", "description": "1-indexed line number" },
                        "character": { "type": "integer", "description": "1-indexed character column offset" }
                    },
                    "required": ["path", "line", "character"]
                }),
            },
            WasiToolDefinition {
                name: "lsp_type_definition".into(),
                description: "Locate the type definition of a symbol using LSP.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "line": { "type": "integer" },
                        "character": { "type": "integer" }
                    },
                    "required": ["path", "line", "character"]
                }),
            },
            WasiToolDefinition {
                name: "lsp_implementation".into(),
                description: "Locate implementations of a symbol using LSP.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "line": { "type": "integer" },
                        "character": { "type": "integer" }
                    },
                    "required": ["path", "line", "character"]
                }),
            },
            WasiToolDefinition {
                name: "lsp_hover".into(),
                description: "Show hover documentation and type information using LSP.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "line": { "type": "integer" },
                        "character": { "type": "integer" }
                    },
                    "required": ["path", "line", "character"]
                }),
            },
            WasiToolDefinition {
                name: "lsp_code_actions".into(),
                description: "List fixes and refactorings available at a location using LSP.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "line": { "type": "integer" },
                        "character": { "type": "integer" }
                    },
                    "required": ["path", "line", "character"]
                }),
            },
            WasiToolDefinition {
                name: "lsp_rename".into(),
                description: "Compute a structured workspace-edit plan to rename a symbol using LSP. The plan does not mutate files and must be applied atomically by the host.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Target file path" },
                        "line": { "type": "integer", "description": "1-indexed line number" },
                        "character": { "type": "integer", "description": "1-indexed character column offset" },
                        "new_name": { "type": "string", "description": "New symbol name" }
                    },
                    "required": ["path", "line", "character", "new_name"]
                }),
            },
            WasiToolDefinition {
                name: "lsp_diagnostics".into(),
                description: "Run cargo check and return Rust compiler errors/warnings for a target file.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Target file path" }
                    },
                    "required": ["path"]
                }),
            },
            WasiToolDefinition {
                name: "lsp_status".into(),
                description: "Report the active LSP server and protocol state.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
            WasiToolDefinition {
                name: "lsp_symbols".into(),
                description: "List document symbols using LSP.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            },
            WasiToolDefinition {
                name: "lsp_format".into(),
                description: "Return a structured non-mutating workspace-edit plan for LSP document formatting.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            },
            WasiToolDefinition {
                name: "lsp_rename_file".into(),
                description: "Apply LSP workspace rename edits before moving a file, then notify the server.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"old_path": {"type": "string"}, "new_path": {"type": "string"}}, "required": ["old_path", "new_path"]}),
            },
        ],
        commands: vec![serde_json::json!({
            "name": "editor_lsp",
            "description": "Serve ephemeral LSP requests for the active unsaved editor buffer."
        })],
        hooks: vec!["after_tool_call".into()],
    }
}

fn detect_lsp_server(file_path: &str) -> &'static str {
    if file_path.ends_with(".rs") {
        "rust-analyzer"
    } else if file_path.ends_with(".ts")
        || file_path.ends_with(".tsx")
        || file_path.ends_with(".js")
        || file_path.ends_with(".jsx")
    {
        "typescript-language-server"
    } else if file_path.ends_with(".go") {
        "gopls"
    } else if file_path.ends_with(".py") {
        "pyright"
    } else {
        "lsp-server"
    }
}

#[no_mangle]
pub extern "C" fn execute_tool(ptr: i32, len: i32) -> u64 {
    let payload = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let invocation: Invocation = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(e) => return write_output(&Response::error(format!("Invalid invocation JSON: {e}"))),
    };

    let response = handle_invocation(&invocation);
    write_output(&response)
}

#[no_mangle]
pub extern "C" fn execute_command(ptr: i32, len: i32) -> u64 {
    let payload = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let invocation: Invocation = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(e) => return write_output(&Response::error(format!("Invalid invocation JSON: {e}"))),
    };

    let response = handle_invocation(&invocation);
    write_output(&response)
}

#[no_mangle]
pub extern "C" fn handle_hook(ptr: i32, len: i32) -> u64 {
    let payload = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let invocation: Invocation = match serde_json::from_slice(payload) {
        Ok(invocation) => invocation,
        Err(error) => return write_output(&Response::error(format!("Invalid hook JSON: {error}"))),
    };
    let tool = invocation
        .arguments
        .get("tool_name")
        .and_then(serde_json::Value::as_str);
    let path = invocation
        .arguments
        .get("tool_arguments")
        .and_then(|arguments| arguments.get("path"))
        .and_then(serde_json::Value::as_str);
    let state_path = invocation
        .state
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let state_uri = invocation
        .state
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if !matches!(
        tool,
        Some(
            "write_file"
                | "edit_file"
                | "edit_file_hashline"
                | "edit_files_hashline"
                | "apply_workspace_edit_plan"
        )
    ) || invocation
        .arguments
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        || !path.is_some_and(|path| path_matches_uri(path, state_uri, state_path))
    {
        return write_output(&Response::ok(String::new()));
    }
    let Some(name) = invocation
        .state
        .get("process_name")
        .and_then(serde_json::Value::as_str)
    else {
        return write_output(&Response::ok(String::new()));
    };
    let Some(uri) = invocation
        .state
        .get("uri")
        .and_then(serde_json::Value::as_str)
    else {
        return write_output(&Response::ok(String::new()));
    };
    send_broker_request(&process_request(
        "send",
        serde_json::json!({
            "name": name,
            "data": frame_jsonrpc(&lsp_notification("textDocument/didSave", serde_json::json!({
                "textDocument": {"uri": uri}
            }))),
        }),
    ));
    send_broker_request(&process_request(
        "recv",
        serde_json::json!({
            "name": name,
            "framing": "content-length",
            "timeout_ms": 250,
        }),
    ));
    write_output(&Response::ok(String::new()))
}

fn handle_invocation(invocation: &Invocation) -> Response {
    match invocation.name.as_str() {
        "editor_lsp" => editor::handle(invocation),
        "lsp_definition"
        | "lsp_type_definition"
        | "lsp_implementation"
        | "lsp_references"
        | "lsp_hover"
        | "lsp_code_actions"
        | "lsp_symbols"
        | "lsp_format"
        | "lsp_rename"
        | "lsp_rename_file" => match prepare_lsp(invocation) {
            Ok((response, requests)) => {
                for request in &requests {
                    send_broker_request(request);
                }
                response
            }
            Err(response) => response,
        },
        "lsp_diagnostics" => {
            let file_path = invocation
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if file_path.is_empty() {
                Response::error("'path' parameter is required.")
            } else {
                handle_diagnostics(invocation, file_path)
            }
        }
        "lsp_status" => {
            let server = invocation
                .state
                .get("server")
                .and_then(|value| value.as_str())
                .unwrap_or("none");
            let phase = invocation
                .state
                .get("phase")
                .and_then(|value| value.as_str())
                .unwrap_or("idle");
            Response::ok(format!("LSP status: {server} ({phase})"))
        }
        unknown => Response::error(format!("Unknown tool '{unknown}'")),
    }
}

fn write_output<T: Serialize>(value: &T) -> u64 {
    let json = match serde_json::to_vec(value) {
        Ok(bytes) => bytes,
        Err(_) => b"{\"error\":\"Failed to serialize response\"}".to_vec(),
    };
    let mut buffer = OUTPUT_BUF.lock().expect("output buffer lock poisoned");
    *buffer = json;
    let ptr = buffer.as_ptr() as u64;
    let len = buffer.len() as u64;
    (ptr << 32) | len
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_response_event(
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
    ) -> ExtensionEvent {
        let message = serde_json::json!({
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
        })
        .to_string();
        ExtensionEvent {
            topic: "broker_response".into(),
            payload: serde_json::json!({
                "api_version": 2,
                "capability": "process",
                "operation": "run",
                "ok": true,
                "value": {"message": message},
            }),
        }
    }

    fn diagnostics_invocation(path: &str, events: Vec<ExtensionEvent>) -> Invocation {
        Invocation {
            name: "lsp_diagnostics".into(),
            arguments: serde_json::json!({"path": path}),
            state: serde_json::json!({}),
            events,
        }
    }

    fn process_recv_event(message: serde_json::Value) -> ExtensionEvent {
        ExtensionEvent {
            topic: "broker_response".into(),
            payload: serde_json::json!({"api_version": 2, "capability": "process", "operation": "recv", "ok": true, "value": {"message": {"data": message.to_string(), "eof": false}}}),
        }
    }

    #[test]
    fn process_recv_unwraps_the_jsonrpc_data_envelope() {
        let event =
            process_recv_event(serde_json::json!({"jsonrpc": "2.0", "id": 42, "result": {}}));
        assert_eq!(
            broker_message(&event, "recv").unwrap().unwrap(),
            serde_json::json!({"jsonrpc": "2.0", "id": 42, "result": {}})
        );
    }

    #[test]
    fn lsp_waits_through_notifications_and_server_requests_with_a_budget() {
        for phase in ["initializing", "requesting"] {
            let mut invocation = Invocation {
                name: "lsp_symbols".into(),
                arguments: serde_json::json!({"path": "src/lib.rs"}),
                state: serde_json::json!({"phase": phase, "server": "rust-analyzer", "process_name": "lsp-rust-analyzer", "path": "src/lib.rs", "uri": "file:///workspace/src/lib.rs", "pending_request_id": 7, "next_request_id": 8}),
                events: vec![process_recv_event(
                    serde_json::json!({"jsonrpc":"2.0", "method":"textDocument/publishDiagnostics", "params":{}}),
                )],
            };
            let (response, requests) = prepare_lsp(&invocation).unwrap();
            assert_eq!(response.state.as_ref().unwrap()["phase"], phase);
            assert_eq!(requests.len(), 1, "notifications need no response");
            assert_eq!(requests[0].operation, "recv");
            invocation.state = response.state.unwrap();
            // A server request can use the same numeric id as our request.
            invocation.events = vec![process_recv_event(
                serde_json::json!({"jsonrpc":"2.0", "id":7, "method":"server/unknown"}),
            )];
            let (response, requests) = prepare_lsp(&invocation).unwrap();
            assert_eq!(response.state.as_ref().unwrap()["phase"], phase);
            assert_eq!(requests.len(), 2);
            let frame = requests[0].arguments["data"].as_str().unwrap();
            let reply: serde_json::Value =
                serde_json::from_str(frame.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(reply["id"], 7);
            assert_eq!(reply["error"]["code"], -32601);
            invocation.state = response.state.unwrap();
            invocation.events = vec![process_recv_event(
                serde_json::json!({"jsonrpc":"2.0", "id":6, "result":[]}),
            )];
            let (response, requests) = prepare_lsp(&invocation).unwrap();
            assert_eq!(response.state.as_ref().unwrap()["phase"], phase);
            assert_eq!(
                requests[0].operation, "recv",
                "stale replies must not satisfy this request"
            );
            invocation.state = response.state.unwrap();
            invocation.events = vec![process_recv_event(
                serde_json::json!({"jsonrpc":"2.0", "id":7, "result":[]}),
            )];
            let (response, _) = prepare_lsp(&invocation).unwrap();
            assert_eq!(
                response.state.as_ref().unwrap()["phase"],
                if phase == "initializing" {
                    "requesting"
                } else {
                    "ready"
                }
            );

            invocation.state["pump_steps"] = serde_json::json!(200);
            invocation.events = vec![process_recv_event(
                serde_json::json!({"jsonrpc":"2.0", "method":"$/progress"}),
            )];
            let error = prepare_lsp(&invocation).unwrap_err();
            assert!(error.message.contains("budget"));
            let next = Invocation {
                state: error.state.unwrap(),
                events: vec![],
                ..invocation
            };
            let (_, requests) = prepare_lsp(&next).unwrap();
            assert_eq!(requests[0].operation, "kill");
            assert_eq!(requests[1].operation, "spawn");
        }
    }

    #[test]
    fn lsp_reports_rpc_errors_and_rejects_malformed_responses() {
        let mut invocation = Invocation {
            name: "lsp_symbols".into(),
            arguments: serde_json::json!({"path":"src/lib.rs"}),
            state: serde_json::json!({"phase":"requesting", "process_name":"lsp-rust-analyzer", "pending_request_id":7, "next_request_id":8}),
            events: vec![process_recv_event(
                serde_json::json!({"jsonrpc":"2.0", "id":7, "error":{"code":-32601,"message":"not supported"}}),
            )],
        };
        let error = prepare_lsp(&invocation).unwrap_err();
        assert!(error.message.contains("not supported"));
        assert_eq!(error.state.unwrap()["phase"], "ready");
        invocation.events = vec![process_recv_event(
            serde_json::json!({"jsonrpc":"2.0", "id":7}),
        )];
        let error = prepare_lsp(&invocation).unwrap_err();
        assert_eq!(error.state.unwrap()["phase"], "failed");
        invocation.events = vec![ExtensionEvent {
            topic: "broker_response".into(),
            payload: serde_json::json!({"capability":"process", "operation":"recv", "ok":true, "value":{"message":{"data":"", "eof":true}}}),
        }];
        let error = prepare_lsp(&invocation).unwrap_err();
        assert!(error.message.contains("EOF"));
        assert_eq!(error.state.unwrap()["phase"], "failed");
    }

    #[test]
    fn formatting_refresh_sends_a_request_instead_of_reading_again() {
        let invocation = Invocation {
            name: "lsp_format".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
            state: serde_json::json!({"phase": "ready", "server": "rust-analyzer", "process_name": "lsp-rust-analyzer", "uri": "file:///workspace/src/lib.rs", "path": "src/lib.rs", "document_text": "old", "next_request_id": 8}),
            events: vec![],
        };
        let (response, requests) = prepare_lsp(&invocation).unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].capability, "fs");
        let refreshed = Invocation {
            state: response.state.unwrap(),
            events: vec![ExtensionEvent {
                topic: "broker_response".into(),
                payload: serde_json::json!({"capability": "fs", "operation": "read_text", "ok": true, "value": {"message": "new text"}}),
            }],
            ..invocation
        };
        let (response, requests) = prepare_lsp(&refreshed).unwrap();
        assert_eq!(response.state.as_ref().unwrap()["phase"], "requesting");
        assert_eq!(requests.len(), 3);
        assert!(requests[0].arguments["data"]
            .as_str()
            .unwrap()
            .contains("textDocument/didChange"));
        assert!(requests[0].arguments["data"]
            .as_str()
            .unwrap()
            .contains("new text"));
        assert!(requests[1].arguments["data"]
            .as_str()
            .unwrap()
            .contains("textDocument/formatting"));
        assert_eq!(requests[2].operation, "recv");
        let completed = Invocation {
            state: response.state.unwrap(),
            events: vec![process_recv_event(serde_json::json!({
                "jsonrpc": "2.0", "id": 8, "result": [],
            }))],
            ..refreshed
        };
        let (response, requests) = prepare_lsp(&completed).unwrap();
        assert_eq!(response.state.as_ref().unwrap()["phase"], "ready");
        assert!(!response.continue_after_broker);
        assert!(requests.is_empty(), "formatting only returns an edit plan");
        let plan: serde_json::Value = serde_json::from_str(&response.message).unwrap();
        assert_eq!(plan["kind"], "lsp_workspace_edit_plan");
        // The next independent call must refresh again, rather than reuse the
        // previous contents or leave the session in a transient phase.
        let next = Invocation {
            state: response.state.unwrap(),
            events: vec![],
            ..completed
        };
        let (response, requests) = prepare_lsp(&next).unwrap();
        assert_eq!(
            response.state.as_ref().unwrap()["phase"],
            "refreshing_format"
        );
        assert_eq!(requests[0].operation, "read_text");
    }

    #[test]
    fn initializing_accepts_its_persisted_request_id() {
        let invocation = Invocation {
            name: "lsp_definition".into(),
            arguments: serde_json::json!({"path": "src/app.ts", "line": 1, "character": 1}),
            state: serde_json::json!({"phase": "initializing", "server": "typescript-language-server", "process_name": "lsp-typescript-language-server", "uri": "file:///workspace/src/app.ts", "document_text": "const x = 1;\n", "next_request_id": 43, "pending_request_id": 42}),
            events: vec![process_recv_event(
                serde_json::json!({"jsonrpc": "2.0", "id": 42, "result": {}}),
            )],
        };
        let (response, requests) = prepare_lsp(&invocation).unwrap();
        assert_eq!(response.state.as_ref().unwrap()["phase"], "requesting");
        assert!(requests[1].arguments["data"]
            .as_str()
            .unwrap()
            .contains("languageId\":\"typescript"));
        assert!(requests[2].arguments["data"]
            .as_str()
            .unwrap()
            .contains("\"id\":43"));
    }

    #[test]
    fn rename_response_returns_plan_without_filesystem_writes() {
        let data = serde_json::json!({"jsonrpc":"2.0","id":3,"result":{"changes":{"file:///workspace/src/lib.rs":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"newText":"renamed"}]}}}).to_string();
        let invocation = Invocation {
            name: "lsp_rename".into(),
            arguments: serde_json::json!({"path":"src/lib.rs","line":1,"character":1,"new_name":"renamed"}),
            state: serde_json::json!({"phase":"requesting","server":"rust-analyzer","process_name":"lsp-rust-analyzer","workspace_root":"/workspace","path":"src/lib.rs","document_text":"old\n","request_id":3,"pending_request_id":3}),
            events: vec![ExtensionEvent {
                topic: "broker_response".into(),
                payload: serde_json::json!({"capability":"process","operation":"recv","ok":true,"value":{"message":{"data":data}}}),
            }],
        };
        let (response, requests) = prepare_lsp(&invocation).unwrap();
        assert!(requests.is_empty());
        let plan: serde_json::Value = serde_json::from_str(&response.message).unwrap();
        assert_eq!(plan["kind"], "lsp_workspace_edit_plan");
        assert_eq!(plan["files"][0]["path"], "src/lib.rs");
        assert_eq!(response.state.unwrap()["phase"], "ready");
    }

    #[test]
    fn format_response_returns_plan_without_filesystem_writes() {
        let data = serde_json::json!({"jsonrpc":"2.0","id":4,"result":[]}).to_string();
        let invocation = Invocation {
            name: "lsp_format".into(),
            arguments: serde_json::json!({"path":"src/lib.rs"}),
            state: serde_json::json!({"phase":"requesting","server":"rust-analyzer","process_name":"lsp-rust-analyzer","workspace_root":"/workspace","path":"src/lib.rs","document_text":"old\n","request_id":4,"pending_request_id":4}),
            events: vec![ExtensionEvent {
                topic: "broker_response".into(),
                payload: serde_json::json!({"capability":"process","operation":"recv","ok":true,"value":{"message":{"data":data}}}),
            }],
        };
        let (response, requests) = prepare_lsp(&invocation).unwrap();
        assert!(requests.is_empty());
        let plan: serde_json::Value = serde_json::from_str(&response.message).unwrap();
        assert_eq!(plan["operation"], "format");
        assert_eq!(response.state.unwrap()["phase"], "ready");
    }

    #[test]
    fn test_manifest_structure() {
        let manifest = extension_manifest();
        assert_eq!(manifest.name, "lsp_ext");
        assert_eq!(manifest.tools.len(), 12);
    }

    #[test]
    fn test_detect_lsp_server() {
        assert_eq!(detect_lsp_server("main.rs"), "rust-analyzer");
        assert_eq!(detect_lsp_server("app.ts"), "typescript-language-server");
        assert_eq!(detect_lsp_server("main.go"), "gopls");
        assert_eq!(detect_lsp_server("script.py"), "pyright");
    }

    #[test]
    fn test_handle_invocation_definition() {
        let inv = Invocation {
            name: "lsp_definition".into(),
            arguments: serde_json::json!({
                "path": "src/main.rs",
                "line": 10,
                "character": 5
            }),
            state: serde_json::json!({}),
            events: vec![],
        };
        let resp = handle_invocation(&inv);
        assert!(resp.error.is_none());
        assert!(resp.continue_after_broker);
        assert_eq!(resp.state.unwrap()["phase"], "spawning");
    }

    #[test]
    fn first_lsp_request_spawns_the_detected_server() {
        let invocation = Invocation {
            name: "lsp_definition".into(),
            arguments: serde_json::json!({
                "path": "src/main.rs",
                "line": 10,
                "character": 5
            }),
            state: serde_json::json!({}),
            events: vec![],
        };

        let (response, requests) = prepare_lsp(&invocation).unwrap();

        assert!(response.continue_after_broker);
        assert_eq!(response.state.unwrap()["phase"], "spawning");
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].operation, "spawn");
        assert_eq!(requests[0].arguments["program"], "rust-analyzer");
        assert_eq!(requests[1].capability, "fs");
        assert_eq!(requests[2].operation, "absolute_path");
    }

    #[test]
    fn lsp_status_reports_persisted_server_state() {
        let response = handle_invocation(&Invocation {
            name: "lsp_status".into(),
            arguments: serde_json::json!({}),
            state: serde_json::json!({"server": "rust-analyzer", "phase": "ready"}),
            events: vec![],
        });
        assert_eq!(response.message, "LSP status: rust-analyzer (ready)");
    }

    #[test]
    fn applies_lsp_text_edits_from_the_end_of_the_document() {
        let text = "let old = 1;\nold\n";
        let edits = serde_json::json!([
            {"range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 7}}, "newText": "new"},
            {"range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 3}}, "newText": "new"}
        ]);
        assert_eq!(
            apply_text_edits(text, &edits).unwrap(),
            "let new = 1;\nnew\n"
        );
    }

    #[test]
    fn normalizes_document_changes_workspace_edits() {
        let changes = workspace_edit_changes(&serde_json::json!({
            "documentChanges": [{
                "textDocument": {"uri": "file:///workspace/src/lib.rs", "version": 1},
                "edits": []
            }]
        }))
        .unwrap();
        assert!(changes.contains_key("file:///workspace/src/lib.rs"));
    }

    #[test]
    fn hook_matches_absolute_path_against_lsp_uri() {
        assert!(path_matches_uri(
            "/workspace/src/lib.rs",
            "file:///workspace/src/lib.rs",
            "src/lib.rs",
        ));
    }

    #[test]
    fn test_parse_cargo_diagnostics() {
        let json_sample = r#"{"reason":"compiler-message","message":{"level":"error","message":"expected one of `!` or `::`, found `#`","spans":[{"file_name":"extensions/lsp_ext/src/lib.rs","line_start":22,"column_start":1}]}}"#;
        let (errors, warnings, msgs) =
            parse_cargo_diagnostics(json_sample, "extensions/lsp_ext/src/lib.rs");
        assert_eq!(errors, 1);
        assert_eq!(warnings, 0);
        assert_eq!(
            msgs,
            vec!["- [ERROR] extensions/lsp_ext/src/lib.rs:22:1: expected one of `!` or `::`, found `#`"]
        );
    }

    #[test]
    fn test_diagnostics_success_filters_requested_path() {
        let other_diagnostic = r#"{"reason":"compiler-message","message":{"level":"warning","message":"warning in another file","spans":[{"file_name":"src/other.rs","line_start":3,"column_start":2}]}}"#;
        let target_diagnostic = r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","spans":[{"file_name":"src/lib.rs","line_start":12,"column_start":7}]}}"#;
        let stdout = format!("{other_diagnostic}\n{target_diagnostic}");
        let invocation = diagnostics_invocation(
            "src/lib.rs",
            vec![process_response_event(Some(101), &stdout, "")],
        );

        let response = handle_invocation(&invocation);

        assert!(response.error.is_none());
        assert!(!response.continue_after_broker);
        assert!(response.message.contains("1 error, 0 warnings"));
        assert!(response
            .message
            .contains("src/lib.rs:12:7: mismatched types"));
        assert!(!response.message.contains("warning in another file"));
    }

    #[test]
    fn test_diagnostics_no_diagnostics() {
        let invocation = diagnostics_invocation(
            "src/lib.rs",
            vec![process_response_event(
                Some(0),
                r#"{"reason":"build-finished","success":true}"#,
                "Finished dev profile",
            )],
        );

        let response = handle_invocation(&invocation);

        assert_eq!(
            response.message,
            "No Rust errors or warnings found for 'src/lib.rs'."
        );
        assert!(response.error.is_none());
        assert!(!response.continue_after_broker);
    }

    #[test]
    fn test_diagnostics_broker_error() {
        let invocation = diagnostics_invocation(
            "src/lib.rs",
            vec![ExtensionEvent {
                topic: "broker_response".into(),
                payload: serde_json::json!({
                    "api_version": 2,
                    "capability": "process",
                    "operation": "run",
                    "ok": false,
                    "error": {
                        "code": "capability_denied",
                        "message": "process execution denied",
                    },
                }),
            }],
        );

        let (response, request) = prepare_diagnostics(&invocation, "src/lib.rs");

        assert!(request.is_none());
        assert_eq!(
            response.error.as_deref(),
            Some(
                "Rust diagnostics failed: process/run broker error `capability_denied`: process execution denied"
            )
        );
        assert!(!response.continue_after_broker);
    }

    #[test]
    fn test_diagnostics_request_and_continuation_shape() {
        let initial = diagnostics_invocation("src/lib.rs", vec![]);
        let (initial_response, request) = prepare_diagnostics(&initial, "src/lib.rs");
        let request = request.expect("the first invocation must queue cargo check");

        assert!(initial_response.continue_after_broker);
        assert_eq!(
            serde_json::to_value(&initial_response).unwrap()["continue_after_broker"],
            true
        );
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "api_version": 2,
                "capability": "process",
                "operation": "run",
                "arguments": {
                    "program": "cargo",
                    "args": ["check", "--message-format=json"],
                    "timeout_ms": CARGO_CHECK_TIMEOUT_MS,
                    "max_output_bytes": CARGO_CHECK_MAX_OUTPUT_BYTES,
                },
            })
        );

        let process_message = serde_json::json!({
            "exit_code": 0,
            "stdout": "",
            "stderr": "",
        })
        .to_string();
        let continuation: Invocation = serde_json::from_value(serde_json::json!({
            "name": "lsp_diagnostics",
            "arguments": {"path": "src/lib.rs"},
            "events": [{
                "topic": "broker_response",
                "payload": {
                    "api_version": 2,
                    "capability": "process",
                    "operation": "run",
                    "ok": true,
                    "value": {"message": process_message},
                },
            }],
        }))
        .unwrap();
        let (continuation_response, repeated_request) =
            prepare_diagnostics(&continuation, "src/lib.rs");

        assert!(repeated_request.is_none());
        assert!(!continuation_response.continue_after_broker);
        assert!(serde_json::to_value(&continuation_response)
            .unwrap()
            .get("continue_after_broker")
            .is_none());
    }

    #[test]
    fn test_diagnostics_rejects_non_rust_paths_without_request() {
        let invocation = diagnostics_invocation("src/app.ts", vec![]);

        let (response, request) = prepare_diagnostics(&invocation, "src/app.ts");

        assert!(request.is_none());
        assert!(response.error.is_some());
        assert!(response.message.contains("supports only Rust (.rs) files"));
        assert!(response.message.contains("not generic LSP support"));
        assert!(!response.continue_after_broker);
    }
}
