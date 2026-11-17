//! Debug Adapter Protocol (DAP) extension for Threadlane.
//!
//! Threadlane acts as a DAP *client*: it launches a debug adapter through the
//! host's brokered `process` capability and speaks DAP over the adapter's stdio
//! pipes. DAP uses the same `Content-Length` framing as LSP, so the broker's
//! `content-length` recv framing applies unchanged.
//!
//! The broker answers asynchronously: a tool call issues broker requests, sets
//! `continue_after_broker`, and is re-invoked with `broker_response` events and
//! whatever `state` it saved. Every tool here is therefore a phase machine
//! driven by that continuation, exactly like `lsp_ext`.
//!
//! The debug adapter itself is a *named managed process*, so it outlives a
//! single tool call. That is what lets `debug_run` stop at a breakpoint and a
//! later `debug_continue` resume the very same program.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Extension ABI types
// ---------------------------------------------------------------------------

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

#[derive(Debug, Deserialize, Default)]
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

#[derive(Debug, Serialize, PartialEq)]
struct BrokerRequest {
    api_version: u32,
    capability: String,
    operation: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize, PartialEq, Default)]
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
    fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            error: Some(message.clone()),
            message,
            ..Default::default()
        }
    }

    fn pending(message: impl Into<String>, state: serde_json::Value) -> Self {
        Self {
            message: message.into(),
            error: None,
            continue_after_broker: true,
            state: Some(state),
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

const BROKER_API_VERSION: u32 = 2;
const ADAPTER_PROCESS_NAME: &str = "dap-adapter";
const RECV_TIMEOUT_MS: u64 = 30_000;

/// Upper bound on continuation round trips for one tool call.
///
/// Every DAP message costs one host round trip, and an adapter that streams
/// output events could otherwise keep a tool call alive indefinitely. Reaching
/// this limit reports a timeout instead of spinning.
const MAX_PUMP_STEPS: u64 = 200;

// ---------------------------------------------------------------------------
// Adapter selection
// ---------------------------------------------------------------------------

/// A debug adapter that speaks DAP over stdio.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Adapter {
    /// Executable to spawn.
    program: String,
    /// Arguments that put the executable into stdio DAP mode.
    args: Vec<String>,
    /// DAP `launch` request `type`, which adapters use to pick a debug mode.
    request_type: String,
}

impl Adapter {
    fn new(program: &str, args: &[&str], request_type: &str) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            request_type: request_type.to_string(),
        }
    }
}

/// Picks a debug adapter from the program under test.
///
/// Only adapters that speak DAP over stdio are usable here; anything requiring
/// a TCP port would need a capability the broker does not expose.
fn detect_adapter(program: &str) -> Adapter {
    let lowered = program.to_ascii_lowercase();
    if lowered.ends_with(".py") {
        // debugpy's adapter is a module, so it is launched through Python.
        Adapter::new("python3", &["-m", "debugpy.adapter"], "python")
    } else if lowered.ends_with(".go") || lowered.ends_with("go.mod") {
        Adapter::new("dlv", &["dap"], "go")
    } else if lowered.ends_with(".js") || lowered.ends_with(".mjs") || lowered.ends_with(".cjs") {
        Adapter::new("js-debug-adapter", &[], "pwa-node")
    } else {
        // Native executables (Rust, C, C++, Zig, …). `lldb-dap` ships with LLVM
        // and speaks DAP on stdio directly.
        Adapter::new("lldb-dap", &[], "lldb")
    }
}

/// Overrides detection when the caller names an adapter explicitly.
fn resolve_adapter(arguments: &serde_json::Value, program: &str) -> Adapter {
    let mut adapter = detect_adapter(program);
    if let Some(custom) = arguments
        .get("adapter")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|adapter| !adapter.is_empty())
    {
        let mut parts = custom.split_whitespace().map(str::to_string);
        if let Some(command) = parts.next() {
            adapter.program = command;
            adapter.args = parts.collect();
        }
    }
    if let Some(request_type) = arguments
        .get("adapter_type")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        adapter.request_type = request_type.to_string();
    }
    adapter
}

// ---------------------------------------------------------------------------
// DAP wire helpers
// ---------------------------------------------------------------------------

fn dap_request(seq: u64, command: &str, arguments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "seq": seq,
        "type": "request",
        "command": command,
        "arguments": arguments,
    })
}

/// DAP reuses LSP's header framing.
fn frame_dap(payload: &serde_json::Value) -> String {
    let body = serde_json::to_string(payload).expect("DAP payload must serialize");
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

fn process_request(operation: &str, arguments: serde_json::Value) -> BrokerRequest {
    BrokerRequest {
        api_version: BROKER_API_VERSION,
        capability: "process".into(),
        operation: operation.into(),
        arguments,
    }
}

fn send_frame(payload: &serde_json::Value) -> BrokerRequest {
    process_request(
        "send",
        serde_json::json!({ "name": ADAPTER_PROCESS_NAME, "data": frame_dap(payload) }),
    )
}

fn recv_frame() -> BrokerRequest {
    process_request(
        "recv",
        serde_json::json!({
            "name": ADAPTER_PROCESS_NAME,
            "framing": "content-length",
            "timeout_ms": RECV_TIMEOUT_MS,
        }),
    )
}

/// Extracts a `process/<operation>` broker result from the replayed events.
fn broker_message(
    event: &ExtensionEvent,
    operation: &str,
) -> Option<Result<serde_json::Value, String>> {
    if event.topic != "broker_response" {
        return None;
    }
    let payload: BrokerResponsePayload = serde_json::from_value(event.payload.clone()).ok()?;
    if payload.capability != "process" || payload.operation != operation {
        return None;
    }
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
    if operation != "recv" {
        return Some(Ok(message));
    }

    // A recv carries either a framed DAP message or an end-of-stream marker.
    // An adapter that dies mid-session must not look like a silent success.
    let data = message.get("data").and_then(serde_json::Value::as_str);
    match data {
        Some(data) if !data.is_empty() => Some(
            serde_json::from_str(data)
                .map_err(|error| format!("Invalid DAP payload from adapter: {error}")),
        ),
        _ if message.get("eof").and_then(serde_json::Value::as_bool) == Some(true) => {
            Some(Err("Debug adapter closed its output stream.".into()))
        }
        _ => Some(Err(
            "Debug adapter did not respond within the timeout.".into()
        )),
    }
}

/// One decoded DAP message from the adapter.
#[derive(Debug, PartialEq)]
enum DapMessage {
    Response {
        request_seq: u64,
        command: String,
        success: bool,
        body: serde_json::Value,
        message: Option<String>,
    },
    Event {
        event: String,
        body: serde_json::Value,
    },
    Other,
}

fn classify(message: &serde_json::Value) -> DapMessage {
    match message.get("type").and_then(serde_json::Value::as_str) {
        Some("response") => DapMessage::Response {
            request_seq: message
                .get("request_seq")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
            command: message
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            success: message
                .get("success")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            body: message
                .get("body")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            message: message
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        },
        Some("event") => DapMessage::Event {
            event: message
                .get("event")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            body: message
                .get("body")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        },
        _ => DapMessage::Other,
    }
}

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

/// Continuation state carried across broker round trips.
///
/// `phase` names what the extension is waiting for, so a re-invocation can pick
/// up exactly where the previous one stopped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct SessionState {
    #[serde(default)]
    phase: String,
    #[serde(default)]
    next_seq: u64,
    #[serde(default)]
    pending_seq: u64,
    #[serde(default)]
    pump_steps: u64,
    #[serde(default)]
    program: String,
    #[serde(default)]
    adapter: String,
    #[serde(default)]
    request_type: String,
    #[serde(default)]
    breakpoints: serde_json::Value,
    /// Files whose breakpoints have not been sent yet.
    #[serde(default)]
    pending_breakpoint_files: Vec<String>,
    #[serde(default)]
    thread_id: u64,
    #[serde(default)]
    stop_reason: String,
    #[serde(default)]
    exit_message: String,
    #[serde(default)]
    expression: String,
    /// True between a stop and the program exiting.
    ///
    /// The host persists one state slot per extension across tool calls, so a
    /// later `debug_continue` has to be able to tell "stopped at a breakpoint"
    /// from "nothing is running" without re-deriving it from the adapter.
    #[serde(default)]
    session_active: bool,
}

impl SessionState {
    fn take_seq(&mut self) -> u64 {
        if self.next_seq == 0 {
            self.next_seq = 1;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending_seq = seq;
        seq
    }

    /// Counts a continuation step, returning `false` once the budget is spent.
    fn charge_pump(&mut self) -> bool {
        self.pump_steps += 1;
        self.pump_steps <= MAX_PUMP_STEPS
    }

    fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

fn load_state(invocation: &Invocation) -> SessionState {
    serde_json::from_value(invocation.state.clone()).unwrap_or_default()
}

/// True when this invocation is the host resuming a tool call after answering
/// broker requests, rather than the agent starting a new one.
///
/// Phase alone cannot tell these apart: extension state is persisted per
/// extension, so a fresh `debug_continue` still arrives carrying whatever phase
/// the previous `debug_run` left behind.
fn is_continuation(invocation: &Invocation) -> bool {
    invocation
        .events
        .iter()
        .any(|event| event.topic == "broker_response")
}

/// Loads state for a newly started tool call.
///
/// Continuation bookkeeping is cleared, but the DAP sequence counter and the
/// stopped-thread id carry over: the adapter is the same live process, so its
/// sequence numbers have to keep increasing.
fn begin_state(invocation: &Invocation) -> SessionState {
    let mut state = load_state(invocation);
    state.phase.clear();
    state.pump_steps = 0;
    state.pending_seq = 0;
    state
}

/// Builds a terminal response that persists `state`.
///
/// Every terminal path must go through this. Returning a response without state
/// leaves the previous phase persisted, and the next tool call then starts in a
/// transient phase it cannot handle.
fn finish(mut state: SessionState, message: impl Into<String>) -> (Response, Vec<BrokerRequest>) {
    state.pump_steps = 0;
    state.pending_seq = 0;
    (
        Response {
            message: message.into(),
            error: None,
            continue_after_broker: false,
            state: Some(state.to_value()),
        },
        Vec::new(),
    )
}

/// Terminal response for a session that is stopped and can be resumed.
fn finish_stopped(
    mut state: SessionState,
    message: impl Into<String>,
) -> (Response, Vec<BrokerRequest>) {
    state.phase = "stopped".into();
    state.session_active = true;
    finish(state, message)
}

/// Terminal response for a session that is over; the next call starts clean.
fn finish_ended(message: impl Into<String>) -> (Response, Vec<BrokerRequest>) {
    finish(SessionState::default(), message)
}

/// The single DAP message delivered by this continuation, if any.
fn incoming(invocation: &Invocation) -> Option<Result<serde_json::Value, String>> {
    invocation
        .events
        .iter()
        .find_map(|event| broker_message(event, "recv"))
}

// ---------------------------------------------------------------------------
// Breakpoints
// ---------------------------------------------------------------------------

/// Groups requested breakpoints by source file.
///
/// DAP replaces all breakpoints for a source on every `setBreakpoints`, so they
/// have to be sent one file at a time with every line for that file included.
fn group_breakpoints(arguments: &serde_json::Value) -> serde_json::Value {
    let mut grouped = serde_json::Map::new();
    let Some(entries) = arguments.get("breakpoints").and_then(|v| v.as_array()) else {
        return serde_json::Value::Object(grouped);
    };
    for entry in entries {
        let Some(path) = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        let Some(line) = entry.get("line").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        grouped
            .entry(path.to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
            .expect("breakpoint group is an array")
            .push(serde_json::json!({ "line": line }));
    }
    serde_json::Value::Object(grouped)
}

fn set_breakpoints_request(seq: u64, path: &str, lines: &serde_json::Value) -> serde_json::Value {
    let name = path.rsplit('/').next().unwrap_or(path);
    dap_request(
        seq,
        "setBreakpoints",
        serde_json::json!({
            "source": { "path": path, "name": name },
            "breakpoints": lines,
        }),
    )
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

fn format_stop(reason: &str, frames: &serde_json::Value) -> String {
    let mut out = format!("Stopped ({reason}).");
    let Some(frames) = frames.get("stackFrames").and_then(|v| v.as_array()) else {
        return out;
    };
    if frames.is_empty() {
        return out;
    }
    out.push_str("\n\nStack:");
    for (index, frame) in frames.iter().take(20).enumerate() {
        let name = frame
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<unknown>");
        let line = frame
            .get("line")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let source = frame
            .get("source")
            .and_then(|source| source.get("path").or_else(|| source.get("name")))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if source.is_empty() {
            out.push_str(&format!("\n  #{index} {name}"));
        } else {
            out.push_str(&format!("\n  #{index} {name} at {source}:{line}"));
        }
    }
    if frames.len() > 20 {
        out.push_str(&format!("\n  ... {} more frames", frames.len() - 20));
    }
    out
}

fn format_exit(body: &serde_json::Value) -> String {
    match body.get("exitCode").and_then(serde_json::Value::as_i64) {
        Some(code) => format!("Program exited with code {code}."),
        None => "Program terminated.".to_string(),
    }
}

fn format_evaluate(expression: &str, body: &serde_json::Value) -> String {
    let result = body
        .get("result")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no value>");
    match body.get("type").and_then(serde_json::Value::as_str) {
        Some(kind) => format!("{expression} = {result} ({kind})"),
        None => format!("{expression} = {result}"),
    }
}

// ---------------------------------------------------------------------------
// Tool: debug_run
// ---------------------------------------------------------------------------

/// Drives launch through to the first stop.
///
/// Phases: spawn -> initialize -> launch -> breakpoints -> configure -> run.
fn run_debug(invocation: &Invocation) -> Result<(Response, Vec<BrokerRequest>), Response> {
    // A fresh tool call starts a new run whatever phase the previous one left
    // persisted; only a continuation resumes the phase machine.
    let mut state = if is_continuation(invocation) {
        load_state(invocation)
    } else {
        SessionState::default()
    };
    if !state.charge_pump() {
        return Err(Response::error(
            "Debug session did not reach a stop within the step budget.",
        ));
    }

    match state.phase.as_str() {
        "" => {
            let program = invocation
                .arguments
                .get("program")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|program| !program.is_empty())
                .ok_or_else(|| Response::error("'program' parameter is required."))?;
            let adapter = resolve_adapter(&invocation.arguments, program);

            state.program = program.to_string();
            state.adapter = adapter.program.clone();
            state.request_type = adapter.request_type.clone();
            state.breakpoints = group_breakpoints(&invocation.arguments);
            state.phase = "initialize".into();
            let seq = state.take_seq();

            let initialize = dap_request(
                seq,
                "initialize",
                serde_json::json!({
                    "clientID": "threadlane",
                    "clientName": "Threadlane",
                    "adapterID": adapter.request_type,
                    "pathFormat": "path",
                    "linesStartAt1": true,
                    "columnsStartAt1": true,
                    "supportsRunInTerminalRequest": false,
                }),
            );

            Ok((
                Response::pending(
                    format!("Starting {} for {program}.", adapter.program),
                    state.to_value(),
                ),
                vec![
                    // Spawn is idempotent and would otherwise re-attach to an
                    // adapter left mid-session by a previous run. The broker
                    // dispatches these in order, so the kill lands first; it
                    // fails harmlessly when nothing is running.
                    process_request("kill", serde_json::json!({ "name": ADAPTER_PROCESS_NAME })),
                    process_request(
                        "spawn",
                        serde_json::json!({
                            "name": ADAPTER_PROCESS_NAME,
                            "program": adapter.program,
                            "args": adapter.args,
                        }),
                    ),
                    send_frame(&initialize),
                    recv_frame(),
                ],
            ))
        }

        "initialize" => {
            let message = expect_message(invocation)?;
            match classify(&message) {
                DapMessage::Response {
                    command, success, ..
                } if command == "initialize" => {
                    if !success {
                        return Err(Response::error("Debug adapter rejected initialize."));
                    }
                }
                // Adapters may emit events before the initialize response.
                _ => {
                    return Ok((
                        pump(&mut state, "Initializing debug adapter."),
                        vec![recv_frame()],
                    ))
                }
            }

            let program_args = invocation
                .arguments
                .get("args")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
            state.phase = "launch".into();
            let seq = state.take_seq();
            let launch = dap_request(
                seq,
                "launch",
                serde_json::json!({
                    "type": state.request_type,
                    "request": "launch",
                    "program": state.program,
                    "args": program_args,
                    "stopOnEntry": invocation
                        .arguments
                        .get("stop_on_entry")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                }),
            );
            Ok((
                Response::pending("Launching program under the debugger.", state.to_value()),
                vec![send_frame(&launch), recv_frame()],
            ))
        }

        // Wait for the `initialized` event, which is what authorizes
        // configuration requests such as setBreakpoints.
        "launch" => {
            let message = expect_message(invocation)?;
            match classify(&message) {
                DapMessage::Event { event, .. } if event == "initialized" => {
                    state.pending_breakpoint_files = state
                        .breakpoints
                        .as_object()
                        .map(|files| files.keys().cloned().collect())
                        .unwrap_or_default();
                    state.phase = "breakpoints".into();
                    Ok(next_breakpoint_step(state))
                }
                DapMessage::Response {
                    command,
                    success: false,
                    message,
                    ..
                } if command == "launch" => Err(Response::error(format!(
                    "Debug adapter rejected launch: {}",
                    message.unwrap_or_else(|| "no reason given".into())
                ))),
                _ => Ok((
                    pump(&mut state, "Waiting for the adapter to initialize."),
                    vec![recv_frame()],
                )),
            }
        }

        "breakpoints" => {
            let message = expect_message(invocation)?;
            match classify(&message) {
                DapMessage::Response {
                    command, success, ..
                } if command == "setBreakpoints" => {
                    if !success {
                        return Err(Response::error("Debug adapter rejected setBreakpoints."));
                    }
                    Ok(next_breakpoint_step(state))
                }
                _ => Ok((pump(&mut state, "Setting breakpoints."), vec![recv_frame()])),
            }
        }

        "configure" => {
            let message = expect_message(invocation)?;
            match classify(&message) {
                DapMessage::Response {
                    command, success, ..
                } if command == "configurationDone" => {
                    // Some adapters reject configurationDone when they do not
                    // support it; the session is still usable.
                    let _ = success;
                    state.phase = "running".into();
                    Ok((
                        Response::pending("Running to the first stop.", state.to_value()),
                        vec![recv_frame()],
                    ))
                }
                other => finish_or_pump(state, other, "Running to the first stop."),
            }
        }

        "running" => {
            let message = expect_message(invocation)?;
            finish_or_pump(state, classify(&message), "Running to the first stop.")
        }

        "stack" => {
            let message = expect_message(invocation)?;
            match classify(&message) {
                DapMessage::Response {
                    command,
                    success,
                    body,
                    ..
                } if command == "stackTrace" => {
                    let message = if success {
                        format_stop(&state.stop_reason, &body)
                    } else {
                        format!("Stopped ({}).", state.stop_reason)
                    };
                    Ok(finish_stopped(state, message))
                }
                _ => Ok((
                    pump(&mut state, "Collecting the stack trace."),
                    vec![recv_frame()],
                )),
            }
        }

        unknown => Err(Response::error(format!(
            "Debug session is in an unexpected phase '{unknown}'."
        ))),
    }
}

/// Sends the next pending file's breakpoints, or moves on to configurationDone.
fn next_breakpoint_step(mut state: SessionState) -> (Response, Vec<BrokerRequest>) {
    if let Some(path) = state.pending_breakpoint_files.first().cloned() {
        state.pending_breakpoint_files.remove(0);
        let lines = state
            .breakpoints
            .get(&path)
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
        let seq = state.take_seq();
        let request = set_breakpoints_request(seq, &path, &lines);
        return (
            Response::pending(format!("Setting breakpoints in {path}."), state.to_value()),
            vec![send_frame(&request), recv_frame()],
        );
    }

    state.phase = "configure".into();
    let seq = state.take_seq();
    let done = dap_request(seq, "configurationDone", serde_json::json!({}));
    (
        Response::pending("Finishing debug configuration.", state.to_value()),
        vec![send_frame(&done), recv_frame()],
    )
}

/// Resolves a run into a stop, an exit, or another pump step.
fn finish_or_pump(
    mut state: SessionState,
    message: DapMessage,
    waiting_for: &str,
) -> Result<(Response, Vec<BrokerRequest>), Response> {
    match message {
        DapMessage::Event { event, body } if event == "stopped" => {
            state.thread_id = body
                .get("threadId")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1);
            state.stop_reason = body
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("breakpoint")
                .to_string();
            state.phase = "stack".into();
            let seq = state.take_seq();
            let stack = dap_request(
                seq,
                "stackTrace",
                serde_json::json!({ "threadId": state.thread_id, "levels": 20 }),
            );
            Ok((
                Response::pending("Reading the stack trace.", state.to_value()),
                vec![send_frame(&stack), recv_frame()],
            ))
        }
        DapMessage::Event { event, body } if event == "exited" || event == "terminated" => {
            Ok(finish_ended(format_exit(&body)))
        }
        _ => Ok((pump(&mut state, waiting_for), vec![recv_frame()])),
    }
}

/// Builds a "keep reading" continuation without changing phase.
fn pump(state: &mut SessionState, message: &str) -> Response {
    Response::pending(message, state.to_value())
}

fn expect_message(invocation: &Invocation) -> Result<serde_json::Value, Response> {
    match incoming(invocation) {
        Some(Ok(message)) => Ok(message),
        Some(Err(error)) => Err(Response::error(error)),
        None => Err(Response::error(
            "Missing process/recv response from the debug adapter.",
        )),
    }
}

// ---------------------------------------------------------------------------
// Tool: debug_continue
// ---------------------------------------------------------------------------

fn continue_command(mode: &str) -> Result<&'static str, Response> {
    match mode {
        "" | "continue" => Ok("continue"),
        "next" | "over" | "step_over" => Ok("next"),
        "step_in" | "into" => Ok("stepIn"),
        "step_out" | "out" => Ok("stepOut"),
        other => Err(Response::error(format!(
            "Unknown mode '{other}'. Use continue, next, step_in, or step_out."
        ))),
    }
}

fn resume_debug(invocation: &Invocation) -> Result<(Response, Vec<BrokerRequest>), Response> {
    let mut state = if is_continuation(invocation) {
        load_state(invocation)
    } else {
        let state = begin_state(invocation);
        if !state.session_active {
            return Err(Response::error(
                "No debug session is stopped. Run debug_run first.",
            ));
        }
        state
    };
    if !state.charge_pump() {
        return Err(Response::error(
            "Debug session did not reach a stop within the step budget.",
        ));
    }

    match state.phase.as_str() {
        "" => {
            let mode = invocation
                .arguments
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("continue");
            let command = continue_command(mode)?;
            // Default to the thread the session actually stopped on, which is
            // what the tool description promises.
            let thread_id = invocation
                .arguments
                .get("thread_id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(if state.thread_id == 0 {
                    1
                } else {
                    state.thread_id
                });
            state.thread_id = thread_id;
            state.phase = "running".into();
            let seq = state.take_seq();
            let request = dap_request(
                seq,
                command,
                serde_json::json!({ "threadId": thread_id, "singleThread": false }),
            );
            Ok((
                Response::pending(format!("Resuming ({mode})."), state.to_value()),
                vec![send_frame(&request), recv_frame()],
            ))
        }
        "running" => {
            let message = expect_message(invocation)?;
            finish_or_pump(state, classify(&message), "Waiting for the next stop.")
        }
        "stack" => {
            let message = expect_message(invocation)?;
            match classify(&message) {
                DapMessage::Response {
                    command,
                    success,
                    body,
                    ..
                } if command == "stackTrace" && success => {
                    let message = format_stop(&state.stop_reason, &body);
                    Ok(finish_stopped(state, message))
                }
                _ => Ok((
                    pump(&mut state, "Collecting the stack trace."),
                    vec![recv_frame()],
                )),
            }
        }
        unknown => Err(Response::error(format!(
            "Debug session is in an unexpected phase '{unknown}'."
        ))),
    }
}

// ---------------------------------------------------------------------------
// Tool: debug_eval
// ---------------------------------------------------------------------------

fn eval_debug(invocation: &Invocation) -> Result<(Response, Vec<BrokerRequest>), Response> {
    let mut state = if is_continuation(invocation) {
        load_state(invocation)
    } else {
        let state = begin_state(invocation);
        if !state.session_active {
            return Err(Response::error(
                "No debug session is stopped. Run debug_run first.",
            ));
        }
        state
    };
    if !state.charge_pump() {
        return Err(Response::error(
            "Debug adapter did not answer the evaluate request in time.",
        ));
    }

    match state.phase.as_str() {
        "" => {
            let expression = invocation
                .arguments
                .get("expression")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|expression| !expression.is_empty())
                .ok_or_else(|| Response::error("'expression' parameter is required."))?;
            let frame_id = invocation
                .arguments
                .get("frame_id")
                .and_then(serde_json::Value::as_u64);

            state.expression = expression.to_string();
            state.phase = "evaluating".into();
            let seq = state.take_seq();
            let mut arguments = serde_json::json!({
                "expression": expression,
                "context": "repl",
            });
            if let Some(frame_id) = frame_id {
                arguments["frameId"] = serde_json::json!(frame_id);
            }
            let request = dap_request(seq, "evaluate", arguments);
            Ok((
                Response::pending(format!("Evaluating {expression}."), state.to_value()),
                vec![send_frame(&request), recv_frame()],
            ))
        }
        "evaluating" => {
            let message = expect_message(invocation)?;
            match classify(&message) {
                DapMessage::Response {
                    command,
                    success,
                    body,
                    message,
                    ..
                } if command == "evaluate" => {
                    let text = if success {
                        format_evaluate(&state.expression, &body)
                    } else {
                        format!(
                            "Could not evaluate {}: {}",
                            state.expression,
                            message.unwrap_or_else(|| "no reason given".into())
                        )
                    };
                    // The program is still stopped either way, so the session
                    // stays resumable.
                    Ok(finish_stopped(state, text))
                }
                _ => Ok((
                    pump(&mut state, "Waiting for the evaluate response."),
                    vec![recv_frame()],
                )),
            }
        }
        unknown => Err(Response::error(format!(
            "Debug session is in an unexpected phase '{unknown}'."
        ))),
    }
}

// ---------------------------------------------------------------------------
// Tool: debug_stop
// ---------------------------------------------------------------------------

fn stop_debug() -> (Response, Vec<BrokerRequest>) {
    // Clear the persisted session so the next tool call starts clean.
    let (response, _) = finish_ended("Debug session stopped.");
    (
        response,
        vec![process_request(
            "kill",
            serde_json::json!({ "name": ADAPTER_PROCESS_NAME }),
        )],
    )
}

// ---------------------------------------------------------------------------
// Manifest and exports
// ---------------------------------------------------------------------------

fn extension_manifest() -> WasiExtensionManifest {
    WasiExtensionManifest {
        api_version: BROKER_API_VERSION,
        name: "debug_ext".into(),
        version: "0.1.0".into(),
        description: "Debug Adapter Protocol (DAP) debugging extension for threadlane".into(),
        capabilities: vec!["process".into()],
        tools: vec![
            WasiToolDefinition {
                name: "debug_run".into(),
                description: "Run a program under a debugger with breakpoints and report where it stops, including the stack trace. Use this instead of adding print statements when a failure needs to be observed at runtime.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "program": {
                            "type": "string",
                            "description": "Program to debug: an executable path, or a script such as main.py"
                        },
                        "args": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Arguments passed to the program"
                        },
                        "breakpoints": {
                            "type": "array",
                            "description": "Breakpoints to set before launching",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": { "type": "string", "description": "Source file path" },
                                    "line": { "type": "integer", "description": "1-indexed line number" }
                                },
                                "required": ["path", "line"]
                            }
                        },
                        "stop_on_entry": {
                            "type": "boolean",
                            "description": "Stop at the first line instead of running to a breakpoint"
                        },
                        "adapter": {
                            "type": "string",
                            "description": "Override the debug adapter command, e.g. 'lldb-dap' or 'dlv dap'"
                        },
                        "adapter_type": {
                            "type": "string",
                            "description": "Override the DAP launch type, e.g. 'lldb', 'python', 'go'"
                        }
                    },
                    "required": ["program"]
                }),
            },
            WasiToolDefinition {
                name: "debug_continue".into(),
                description: "Resume a stopped debug session and report the next stop. Modes: continue, next, step_in, step_out.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "mode": {
                            "type": "string",
                            "enum": ["continue", "next", "step_in", "step_out"],
                            "description": "How to resume; defaults to continue"
                        },
                        "thread_id": {
                            "type": "integer",
                            "description": "Thread to resume; defaults to the stopped thread"
                        }
                    }
                }),
            },
            WasiToolDefinition {
                name: "debug_eval".into(),
                description: "Evaluate an expression in the stopped program to inspect a variable or call a function.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "expression": { "type": "string", "description": "Expression to evaluate" },
                        "frame_id": {
                            "type": "integer",
                            "description": "Stack frame to evaluate in; defaults to the top frame"
                        }
                    },
                    "required": ["expression"]
                }),
            },
            WasiToolDefinition {
                name: "debug_stop".into(),
                description: "Terminate the running debug session and its adapter.".into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
        ],
        commands: Vec::new(),
        hooks: Vec::new(),
    }
}

fn handle_invocation(invocation: &Invocation) -> Response {
    let outcome = match invocation.name.as_str() {
        "debug_run" => run_debug(invocation),
        "debug_continue" => resume_debug(invocation),
        "debug_eval" => eval_debug(invocation),
        "debug_stop" => Ok(stop_debug()),
        unknown => Err(Response::error(format!("Unknown tool '{unknown}'"))),
    };

    match outcome {
        Ok((response, requests)) => {
            for request in &requests {
                send_broker_request(request);
            }
            response
        }
        Err(response) => response,
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

#[no_mangle]
pub extern "C" fn execute_tool(ptr: i32, len: i32) -> u64 {
    let payload = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let invocation: Invocation = match serde_json::from_slice(payload) {
        Ok(invocation) => invocation,
        Err(error) => {
            return write_output(&Response::error(format!(
                "Invalid invocation JSON: {error}"
            )))
        }
    };
    write_output(&handle_invocation(&invocation))
}

#[no_mangle]
pub extern "C" fn execute_command(ptr: i32, len: i32) -> u64 {
    execute_tool(ptr, len)
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
    use serde_json::json;

    fn invocation(name: &str, arguments: serde_json::Value) -> Invocation {
        Invocation {
            name: name.to_string(),
            arguments,
            ..Default::default()
        }
    }

    /// A fresh tool call carrying the state a finished `debug_run` leaves behind.
    fn stopped_invocation(name: &str, arguments: serde_json::Value) -> Invocation {
        Invocation {
            name: name.to_string(),
            arguments,
            state: SessionState {
                phase: "stopped".into(),
                session_active: true,
                next_seq: 9,
                thread_id: 7,
                stop_reason: "breakpoint".into(),
                ..Default::default()
            }
            .to_value(),
            events: Vec::new(),
        }
    }

    fn with_message(name: &str, state: &SessionState, message: serde_json::Value) -> Invocation {
        Invocation {
            name: name.to_string(),
            arguments: json!({}),
            state: state.to_value(),
            events: vec![ExtensionEvent {
                topic: "broker_response".into(),
                payload: json!({
                    "capability": "process",
                    "operation": "recv",
                    "ok": true,
                    "value": { "message": json!({
                        "data": serde_json::to_string(&message).unwrap(),
                        "eof": false,
                    }).to_string() },
                }),
            }],
        }
    }

    #[test]
    fn manifest_declares_only_the_process_capability() {
        let manifest = extension_manifest();
        assert_eq!(manifest.name, "debug_ext");
        assert_eq!(manifest.api_version, 2);
        // Debugging never needs direct filesystem access: paths are handed to
        // the adapter, which reads sources itself.
        assert_eq!(manifest.capabilities, vec!["process".to_string()]);
        let names: Vec<&str> = manifest
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["debug_run", "debug_continue", "debug_eval", "debug_stop"]
        );
    }

    #[test]
    fn adapter_detection_covers_supported_languages() {
        assert_eq!(detect_adapter("main.py").program, "python3");
        assert_eq!(detect_adapter("main.py").request_type, "python");
        assert_eq!(detect_adapter("cmd/main.go").program, "dlv");
        assert_eq!(detect_adapter("server.js").request_type, "pwa-node");
        // Native binaries have no distinguishing extension.
        assert_eq!(
            detect_adapter("target/debug/threadlane").program,
            "lldb-dap"
        );
    }

    #[test]
    fn explicit_adapter_overrides_detection() {
        let adapter = resolve_adapter(
            &json!({ "adapter": "codelldb --stdio", "adapter_type": "lldb" }),
            "main.py",
        );
        assert_eq!(adapter.program, "codelldb");
        assert_eq!(adapter.args, vec!["--stdio".to_string()]);
        assert_eq!(adapter.request_type, "lldb");
    }

    #[test]
    fn dap_frames_use_content_length_headers() {
        let framed = frame_dap(&json!({ "seq": 1 }));
        let (header, body) = framed.split_once("\r\n\r\n").expect("framed message");
        assert_eq!(header, format!("Content-Length: {}", body.len()));
        assert_eq!(body, r#"{"seq":1}"#);
    }

    #[test]
    fn breakpoints_group_by_source_file() {
        let grouped = group_breakpoints(&json!({
            "breakpoints": [
                { "path": "src/main.rs", "line": 10 },
                { "path": "src/lib.rs", "line": 4 },
                { "path": "src/main.rs", "line": 22 },
                { "path": "", "line": 1 },
                { "path": "src/skip.rs" },
            ]
        }));
        assert_eq!(
            grouped["src/main.rs"],
            json!([{ "line": 10 }, { "line": 22 }])
        );
        assert_eq!(grouped["src/lib.rs"], json!([{ "line": 4 }]));
        // Entries without a usable path or line are dropped rather than sent.
        assert_eq!(grouped.as_object().unwrap().len(), 2);
    }

    #[test]
    fn set_breakpoints_request_names_the_source() {
        let request = set_breakpoints_request(7, "src/main.rs", &json!([{ "line": 3 }]));
        assert_eq!(request["command"], "setBreakpoints");
        assert_eq!(request["seq"], 7);
        assert_eq!(request["arguments"]["source"]["path"], "src/main.rs");
        assert_eq!(request["arguments"]["source"]["name"], "main.rs");
    }

    #[test]
    fn classify_distinguishes_responses_events_and_noise() {
        assert_eq!(
            classify(&json!({
                "type": "response", "request_seq": 3, "command": "launch", "success": true
            })),
            DapMessage::Response {
                request_seq: 3,
                command: "launch".into(),
                success: true,
                body: serde_json::Value::Null,
                message: None,
            }
        );
        assert_eq!(
            classify(
                &json!({ "type": "event", "event": "stopped", "body": { "reason": "breakpoint" } })
            ),
            DapMessage::Event {
                event: "stopped".into(),
                body: json!({ "reason": "breakpoint" }),
            }
        );
        assert_eq!(classify(&json!({ "type": "request" })), DapMessage::Other);
    }

    #[test]
    fn continue_modes_map_to_dap_commands() {
        assert_eq!(continue_command("").unwrap(), "continue");
        assert_eq!(continue_command("continue").unwrap(), "continue");
        assert_eq!(continue_command("next").unwrap(), "next");
        assert_eq!(continue_command("step_in").unwrap(), "stepIn");
        assert_eq!(continue_command("step_out").unwrap(), "stepOut");
        assert!(continue_command("teleport").is_err());
    }

    #[test]
    fn run_requires_a_program() {
        let error = run_debug(&invocation("debug_run", json!({}))).unwrap_err();
        assert!(error.error.unwrap().contains("'program' parameter"));
    }

    #[test]
    fn run_spawns_the_adapter_and_sends_initialize() {
        let (response, requests) = run_debug(&invocation(
            "debug_run",
            json!({ "program": "target/debug/app" }),
        ))
        .unwrap();

        assert!(response.continue_after_broker);
        assert_eq!(requests.len(), 4);
        // A stale adapter is killed first; spawn is idempotent and would
        // otherwise re-attach to a process left mid-session.
        assert_eq!(requests[0].operation, "kill");
        assert_eq!(requests[1].operation, "spawn");
        assert_eq!(requests[1].arguments["program"], "lldb-dap");
        assert_eq!(requests[1].arguments["name"], ADAPTER_PROCESS_NAME);
        assert_eq!(requests[2].operation, "send");
        assert!(requests[2].arguments["data"]
            .as_str()
            .unwrap()
            .contains("\"command\":\"initialize\""));
        assert_eq!(requests[3].operation, "recv");
        assert_eq!(requests[3].arguments["framing"], "content-length");

        let state: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(state.phase, "initialize");
        assert_eq!(state.program, "target/debug/app");
    }

    #[test]
    fn initialize_response_triggers_launch() {
        let state = SessionState {
            phase: "initialize".into(),
            next_seq: 2,
            program: "app".into(),
            request_type: "lldb".into(),
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({ "type": "response", "command": "initialize", "success": true }),
        );

        let (response, requests) = run_debug(&invocation).unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next.phase, "launch");
        assert!(requests[0].arguments["data"]
            .as_str()
            .unwrap()
            .contains("\"command\":\"launch\""));
    }

    #[test]
    fn initialized_event_starts_breakpoint_configuration() {
        let state = SessionState {
            phase: "launch".into(),
            next_seq: 3,
            breakpoints: json!({ "src/main.rs": [{ "line": 12 }] }),
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({ "type": "event", "event": "initialized" }),
        );

        let (response, requests) = run_debug(&invocation).unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next.phase, "breakpoints");
        assert!(next.pending_breakpoint_files.is_empty());
        assert!(requests[0].arguments["data"]
            .as_str()
            .unwrap()
            .contains("setBreakpoints"));
    }

    #[test]
    fn configuration_done_follows_the_last_breakpoint_file() {
        let state = SessionState {
            phase: "breakpoints".into(),
            next_seq: 4,
            breakpoints: json!({ "src/main.rs": [{ "line": 12 }] }),
            pending_breakpoint_files: Vec::new(),
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({ "type": "response", "command": "setBreakpoints", "success": true }),
        );

        let (response, requests) = run_debug(&invocation).unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next.phase, "configure");
        assert!(requests[0].arguments["data"]
            .as_str()
            .unwrap()
            .contains("configurationDone"));
    }

    #[test]
    fn a_stopped_event_requests_the_stack_trace() {
        let state = SessionState {
            phase: "running".into(),
            next_seq: 5,
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({
                "type": "event",
                "event": "stopped",
                "body": { "reason": "breakpoint", "threadId": 7 }
            }),
        );

        let (response, requests) = run_debug(&invocation).unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next.phase, "stack");
        assert_eq!(next.thread_id, 7);
        assert_eq!(next.stop_reason, "breakpoint");
        assert!(requests[0].arguments["data"]
            .as_str()
            .unwrap()
            .contains("stackTrace"));
    }

    #[test]
    fn a_terminated_event_ends_the_run_without_more_requests() {
        let state = SessionState {
            phase: "running".into(),
            next_seq: 5,
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({ "type": "event", "event": "exited", "body": { "exitCode": 3 } }),
        );

        let (response, requests) = run_debug(&invocation).unwrap();
        assert!(requests.is_empty());
        assert!(!response.continue_after_broker);
        assert_eq!(response.message, "Program exited with code 3.");
    }

    #[test]
    fn unrelated_messages_keep_pumping_without_changing_phase() {
        let state = SessionState {
            phase: "running".into(),
            next_seq: 5,
            pump_steps: 2,
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({ "type": "event", "event": "output", "body": { "output": "hello\n" } }),
        );

        let (response, requests) = run_debug(&invocation).unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next.phase, "running");
        assert_eq!(next.pump_steps, 3);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].operation, "recv");
    }

    #[test]
    fn the_pump_budget_stops_a_runaway_adapter() {
        let state = SessionState {
            phase: "running".into(),
            pump_steps: MAX_PUMP_STEPS,
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({ "type": "event", "event": "output" }),
        );

        let error = run_debug(&invocation).unwrap_err();
        assert!(error.error.unwrap().contains("step budget"));
    }

    #[test]
    fn stack_trace_response_formats_the_stop() {
        let state = SessionState {
            phase: "stack".into(),
            stop_reason: "breakpoint".into(),
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({
                "type": "response",
                "command": "stackTrace",
                "success": true,
                "body": { "stackFrames": [
                    { "name": "main", "line": 12, "source": { "path": "src/main.rs" } },
                    { "name": "start", "line": 3, "source": { "path": "src/lib.rs" } }
                ]}
            }),
        );

        let (response, requests) = run_debug(&invocation).unwrap();
        assert!(requests.is_empty());
        assert!(response.message.starts_with("Stopped (breakpoint)."));
        assert!(response.message.contains("#0 main at src/main.rs:12"));
        assert!(response.message.contains("#1 start at src/lib.rs:3"));
    }

    #[test]
    fn an_adapter_that_closes_its_stream_reports_an_error() {
        let state = SessionState {
            phase: "initialize".into(),
            ..Default::default()
        };
        let invocation = Invocation {
            name: "debug_run".into(),
            state: state.to_value(),
            events: vec![ExtensionEvent {
                topic: "broker_response".into(),
                payload: json!({
                    "capability": "process",
                    "operation": "recv",
                    "ok": true,
                    "value": { "message": json!({ "data": "", "eof": true }).to_string() },
                }),
            }],
            ..Default::default()
        };

        let error = run_debug(&invocation).unwrap_err();
        assert!(error.error.unwrap().contains("closed its output stream"));
    }

    #[test]
    fn broker_failures_surface_as_tool_errors() {
        let state = SessionState {
            phase: "initialize".into(),
            ..Default::default()
        };
        let invocation = Invocation {
            name: "debug_run".into(),
            state: state.to_value(),
            events: vec![ExtensionEvent {
                topic: "broker_response".into(),
                payload: json!({
                    "capability": "process",
                    "operation": "recv",
                    "ok": false,
                    "error": { "code": "not_found", "message": "No managed process named `dap-adapter`" },
                }),
            }],
            ..Default::default()
        };

        let error = run_debug(&invocation).unwrap_err();
        assert!(error.error.unwrap().contains("No managed process"));
    }

    #[test]
    fn resume_sends_the_requested_step_command() {
        let (response, requests) = resume_debug(&stopped_invocation(
            "debug_continue",
            json!({ "mode": "step_in" }),
        ))
        .unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next.phase, "running");
        assert!(requests[0].arguments["data"]
            .as_str()
            .unwrap()
            .contains("\"command\":\"stepIn\""));
    }

    #[test]
    fn resume_rejects_an_unknown_mode() {
        let error = resume_debug(&stopped_invocation(
            "debug_continue",
            json!({ "mode": "sideways" }),
        ))
        .unwrap_err();
        assert!(error.error.unwrap().contains("Unknown mode"));
    }

    #[test]
    fn eval_sends_an_evaluate_request_and_formats_the_result() {
        let (response, requests) = eval_debug(&stopped_invocation(
            "debug_eval",
            json!({ "expression": "counter", "frame_id": 4 }),
        ))
        .unwrap();
        assert!(requests[0].arguments["data"]
            .as_str()
            .unwrap()
            .contains("\"command\":\"evaluate\""));
        let state: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(state.phase, "evaluating");
        assert_eq!(state.expression, "counter");

        let invocation = with_message(
            "debug_eval",
            &state,
            json!({
                "type": "response",
                "command": "evaluate",
                "success": true,
                "body": { "result": "42", "type": "i32" }
            }),
        );
        let (response, requests) = eval_debug(&invocation).unwrap();
        assert!(requests.is_empty());
        assert_eq!(response.message, "counter = 42 (i32)");
    }

    #[test]
    fn a_failed_evaluate_reports_the_adapter_reason_without_erroring() {
        let state = SessionState {
            phase: "evaluating".into(),
            expression: "missing".into(),
            session_active: true,
            ..Default::default()
        };
        let invocation = with_message(
            "debug_eval",
            &state,
            json!({
                "type": "response",
                "command": "evaluate",
                "success": false,
                "message": "no symbol named 'missing'"
            }),
        );

        let (response, _) = eval_debug(&invocation).unwrap();
        assert!(response.error.is_none());
        assert!(response.message.contains("no symbol named 'missing'"));
    }

    #[test]
    fn stop_kills_the_adapter_process() {
        let (response, requests) = stop_debug();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].operation, "kill");
        assert_eq!(requests[0].arguments["name"], ADAPTER_PROCESS_NAME);
        assert!(!response.continue_after_broker);
    }

    #[test]
    fn unknown_tools_are_rejected() {
        let response = handle_invocation(&invocation("debug_teleport", json!({})));
        assert!(response.error.unwrap().contains("Unknown tool"));
    }

    #[test]
    fn a_new_tool_call_ignores_the_phase_left_by_the_previous_one() {
        // The host persists one state slot per extension, so a finished
        // `debug_run` leaves `phase` behind. Dispatching on phase alone would
        // wedge every later call in "unexpected phase".
        let stale = SessionState {
            phase: "stack".into(),
            session_active: true,
            next_seq: 12,
            thread_id: 3,
            ..Default::default()
        };
        let call = Invocation {
            name: "debug_continue".into(),
            arguments: json!({}),
            state: stale.to_value(),
            events: Vec::new(),
        };

        let (response, requests) = resume_debug(&call).unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next.phase, "running");
        assert!(requests[0].arguments["data"]
            .as_str()
            .unwrap()
            .contains("\"command\":\"continue\""));
    }

    #[test]
    fn resume_defaults_to_the_thread_the_session_stopped_on() {
        let (_, requests) = resume_debug(&stopped_invocation("debug_continue", json!({}))).unwrap();
        let sent: serde_json::Value = serde_json::from_str(
            requests[0].arguments["data"]
                .as_str()
                .unwrap()
                .split("\r\n\r\n")
                .nth(1)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(sent["arguments"]["threadId"], 7);
    }

    #[test]
    fn resume_and_eval_refuse_to_run_without_a_stopped_session() {
        let resume = resume_debug(&invocation("debug_continue", json!({}))).unwrap_err();
        assert!(resume
            .error
            .unwrap()
            .contains("No debug session is stopped"));

        let eval = eval_debug(&invocation("debug_eval", json!({ "expression": "x" }))).unwrap_err();
        assert!(eval.error.unwrap().contains("No debug session is stopped"));
    }

    #[test]
    fn a_finished_stack_trace_persists_a_resumable_session() {
        let state = SessionState {
            phase: "stack".into(),
            stop_reason: "breakpoint".into(),
            thread_id: 4,
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({
                "type": "response",
                "command": "stackTrace",
                "success": true,
                "body": { "stackFrames": [{ "name": "main", "line": 1 }] }
            }),
        );

        let (response, _) = run_debug(&invocation).unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next.phase, "stopped");
        assert!(next.session_active);
        assert_eq!(next.thread_id, 4);
        assert_eq!(next.pump_steps, 0, "the budget resets for the next call");
    }

    #[test]
    fn an_exiting_program_clears_the_persisted_session() {
        let state = SessionState {
            phase: "running".into(),
            session_active: true,
            ..Default::default()
        };
        let invocation = with_message(
            "debug_run",
            &state,
            json!({ "type": "event", "event": "terminated" }),
        );

        let (response, _) = run_debug(&invocation).unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next, SessionState::default());
        assert!(!next.session_active);
    }

    #[test]
    fn a_finished_evaluate_leaves_the_session_resumable() {
        let state = SessionState {
            phase: "evaluating".into(),
            expression: "counter".into(),
            session_active: true,
            ..Default::default()
        };
        let invocation = with_message(
            "debug_eval",
            &state,
            json!({
                "type": "response",
                "command": "evaluate",
                "success": true,
                "body": { "result": "1" }
            }),
        );

        let (response, _) = eval_debug(&invocation).unwrap();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next.phase, "stopped");
        assert!(next.session_active);
    }

    #[test]
    fn stopping_clears_the_persisted_session() {
        let (response, _) = stop_debug();
        let next: SessionState = serde_json::from_value(response.state.unwrap()).unwrap();
        assert_eq!(next, SessionState::default());
    }

    #[test]
    fn sequence_numbers_increase_across_requests() {
        let mut state = SessionState::default();
        assert_eq!(state.take_seq(), 1);
        assert_eq!(state.take_seq(), 2);
        assert_eq!(state.pending_seq, 2);
    }
}
