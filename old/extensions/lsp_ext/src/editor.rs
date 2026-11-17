use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

use super::{
    broker_message, broker_recv_message, frame_jsonrpc, fs_message, fs_request,
    lsp_notification, lsp_request, process_request, send_broker_request, BrokerRequest,
    Invocation, ProcessRecvMessage, Response,
};

const MAX_EDITOR_LSP_BYTES: usize = 1024 * 1024;
const MAX_EDITOR_PUMP_STEPS: u64 = 16;
const EDITOR_RECV_TIMEOUT_MS: u64 = 500;
const EDITOR_STARTUP_TIMEOUT_MS: u64 = 20_000;
const EDITOR_QUERY_TIMEOUT_MS: u64 = 8_000;
const MAX_STARTUP_EMPTY_READS: u16 =
    (EDITOR_STARTUP_TIMEOUT_MS / EDITOR_RECV_TIMEOUT_MS) as u16;
const MAX_QUERY_EMPTY_READS: u16 = (EDITOR_QUERY_TIMEOUT_MS / EDITOR_RECV_TIMEOUT_MS) as u16;
const MAX_CACHED_DIAGNOSTICS: usize = 256;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct EditorState {
    phase: String,
    server: String,
    process_name: String,
    language_id: String,
    workspace_root: String,
    root_uri: String,
    candidate_uri: String,
    document: Option<DocumentState>,
    server_capabilities: Value,
    last_server_version: i32,
    next_request_id: u64,
    pending_request_id: u64,
    pending_method: String,
    pending_operation: String,
    resume_phase: String,
    pump_steps: u64,
    timeout_reads: u16,
    failure_message: String,
    diagnostics: Option<Vec<Value>>,
    diagnostics_version: Option<i32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DocumentState {
    path: String,
    uri: String,
    document_id: u64,
    version: u64,
    server_version: i32,
    language_id: String,
}

struct EditorRequest {
    operation: String,
    path: String,
    document_id: u64,
    version: u64,
    text: String,
    position: Value,
    range: Option<Value>,
    trigger_character: Option<String>,
}

struct Server {
    name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
    language_id: &'static str,
}

fn editor_file_uri(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = if let Some(path) = normalized.strip_prefix("//?/") {
        if path
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC/"))
        {
            format!("//{}", &path[4..])
        } else if path
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && path.as_bytes().get(1) == Some(&b':')
        {
            path.to_owned()
        } else {
            format!("//?/{path}")
        }
    } else {
        normalized
    };
    if let Some(unc_path) = normalized.strip_prefix("//") {
        let (host, path) = unc_path.split_once('/').unwrap_or((unc_path, ""));
        format!(
            "file://{}/{}",
            encode_uri_path(host),
            encode_uri_path(path)
        )
    } else if normalized.starts_with('/') {
        format!("file://{}", encode_uri_path(&normalized))
    } else {
        format!("file:///{}", encode_uri_path(&normalized))
    }
}

fn encode_uri_path(path: &str) -> String {
    let mut encoded = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

pub(super) fn handle(invocation: &Invocation) -> Response {
    let request = match parse_request(&invocation.arguments) {
        Ok(request) => request,
        Err(error) => return Response::error(error),
    };
    let mut state: EditorState = invocation
        .state
        .get("editor")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();

    if state.phase.is_empty() || state.phase == "ready" {
        start_or_query(invocation, &request, &mut state)
    } else {
        resume(invocation, &request, &mut state)
    }
}

fn parse_request(arguments: &Value) -> Result<EditorRequest, String> {
    let operation = arguments
        .get("operation")
        .and_then(Value::as_str)
        .ok_or("Missing editor LSP operation")?;
    if !matches!(
        operation,
        "completion" | "hover" | "definition" | "diagnostics" | "code_actions" | "close"
    ) {
        return Err(format!("Unsupported editor LSP operation `{operation}`"));
    }
    let path = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or("Editor LSP path must not be empty")?
        .to_owned();
    let document_id = arguments
        .get("document_id")
        .and_then(Value::as_u64)
        .ok_or("Missing editor LSP document ID")?;
    let version = arguments
        .get("version")
        .and_then(Value::as_u64)
        .ok_or("Missing editor LSP document version")?;
    let text = arguments
        .get("text")
        .and_then(Value::as_str)
        .ok_or("Missing editor LSP buffer snapshot")?
        .to_owned();
    if text.len() > MAX_EDITOR_LSP_BYTES {
        return Err("Editor LSP buffer snapshot exceeds the 1 MiB limit".into());
    }
    let position = arguments
        .get("position")
        .cloned()
        .unwrap_or_else(|| json!({"line":0,"character":0}));
    for key in ["line", "character"] {
        if position.get(key).and_then(Value::as_u64).is_none() {
            return Err(format!("Editor LSP position is missing `{key}`"));
        }
    }
    let range = arguments.get("range").cloned();
    if operation == "code_actions" && range.is_none() {
        return Err("Code actions require an LSP range".into());
    }
    Ok(EditorRequest {
        operation: operation.into(),
        path,
        document_id,
        version,
        text,
        position,
        range,
        trigger_character: arguments
            .get("trigger_character")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn server_for(path: &str) -> Option<Server> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    let (name, program, args, language_id) = match extension.as_str() {
        "rs" => ("rust-analyzer", "rust-analyzer", &[][..], "rust"),
        "js" => (
            "typescript-language-server",
            "typescript-language-server",
            &["--stdio"][..],
            "javascript",
        ),
        "jsx" => (
            "typescript-language-server",
            "typescript-language-server",
            &["--stdio"][..],
            "javascriptreact",
        ),
        "ts" => (
            "typescript-language-server",
            "typescript-language-server",
            &["--stdio"][..],
            "typescript",
        ),
        "tsx" => (
            "typescript-language-server",
            "typescript-language-server",
            &["--stdio"][..],
            "typescriptreact",
        ),
        "go" => ("gopls", "gopls", &[][..], "go"),
        "py" | "pyi" => (
            "pyright-langserver",
            "pyright-langserver",
            &["--stdio"][..],
            "python",
        ),
        _ => return None,
    };
    Some(Server {
        name,
        program,
        args,
        language_id,
    })
}

fn start_or_query(
    invocation: &Invocation,
    request: &EditorRequest,
    state: &mut EditorState,
) -> Response {
    if request.operation == "close" {
        return close_document(invocation, request, state);
    }
    let Some(server) = server_for(&request.path) else {
        return with_state(
            invocation,
            state,
            Response::error(format!(
                "No editor language server is configured for `{}`",
                request.path
            )),
        );
    };
    if state.phase == "ready" && state.server == server.name && !state.process_name.is_empty() {
        if let Some(document) = state.document.as_ref() {
            if document.path == request.path && document.document_id == request.document_id {
                if request.version < document.version {
                    return with_state(
                        invocation,
                        state,
                        Response::error("Stale editor LSP document version"),
                    );
                }
                let mut notifications = Vec::new();
                if request.version != document.version {
                    let Some(version) = state.last_server_version.checked_add(1) else {
                        return fail(
                            invocation,
                            state,
                            "Editor language-server document version exhausted".into(),
                        );
                    };
                    notifications.push(change_notification(document, version, &request.text));
                    state.last_server_version = version;
                    state.document = Some(DocumentState {
                        version: request.version,
                        server_version: version,
                        ..document.clone()
                    });
                    invalidate_diagnostics(state);
                }
                return start_query(invocation, request, state, notifications);
            }
        }
        state.phase = "resolving_switch".into();
        state.server = server.name.into();
        state.language_id = server.language_id.into();
        return continue_with(
            invocation,
            state,
            "Resolving editor document URI.",
            vec![fs_request("absolute_path", json!({"path": request.path}))],
        );
    }

    let old_process = state.process_name.clone();
    *state = EditorState {
        phase: "resolving_initial".into(),
        server: server.name.into(),
        process_name: format!("lsp-editor-{}", server.name),
        language_id: server.language_id.into(),
        next_request_id: 1,
        ..EditorState::default()
    };
    let mut requests = editor_processes_to_kill(&old_process, &state.process_name)
        .into_iter()
        .map(|name| process_request("kill", json!({"name": name})))
        .collect::<Vec<_>>();
    requests.push(fs_request("absolute_path", json!({"path": request.path})));
    requests.push(fs_request("absolute_path", json!({"path": "."})));
    continue_with(
        invocation,
        state,
        "Resolving editor workspace and document.",
        requests,
    )
}

fn editor_processes_to_kill(previous: &str, next: &str) -> Vec<String> {
    let mut names = Vec::new();
    if previous.starts_with("lsp-editor-") {
        names.push(previous.to_owned());
    }
    if next.starts_with("lsp-editor-") && !names.iter().any(|name| name == next) {
        names.push(next.to_owned());
    }
    names
}

fn close_document(
    invocation: &Invocation,
    request: &EditorRequest,
    state: &mut EditorState,
) -> Response {
    let matches = state
        .document
        .as_ref()
        .is_some_and(|document| document_matches(document, request));
    if !matches || state.process_name.is_empty() {
        return result_response_with_diagnostics(invocation, state, Value::Null, None, None);
    }
    let document = state.document.take().expect("matching document exists");
    invalidate_diagnostics(state);
    state.phase = "closing".into();
    continue_with(
        invocation,
        state,
        "Closing editor document.",
        vec![process_request(
            "send",
            json!({
                "name": state.process_name,
                "data": frame_jsonrpc(&close_notification(&document)),
            }),
        )],
    )
}

fn resume(invocation: &Invocation, request: &EditorRequest, state: &mut EditorState) -> Response {
    match state.phase.as_str() {
        "resolving_initial" => {
            let paths = invocation
                .events
                .iter()
                .filter_map(|event| fs_message(event, "absolute_path"))
                .collect::<Vec<_>>();
            if paths.len() < 2 {
                return fail(
                    invocation,
                    state,
                    "Missing editor workspace path response".into(),
                );
            }
            let file_path = match paths[0].as_ref() {
                Ok(path) => path.clone(),
                Err(error) => return reset_error(invocation, state, error.clone()),
            };
            let workspace_root = match paths[1].as_ref() {
                Ok(path) => path.clone(),
                Err(error) => return reset_error(invocation, state, error.clone()),
            };
            state.candidate_uri = editor_file_uri(&file_path);
            state.workspace_root = workspace_root.clone();
            state.root_uri = editor_file_uri(&workspace_root);
            state.phase = "spawning".into();
            let Some(server) = server_for(&request.path) else {
                return reset_error(
                    invocation,
                    state,
                    "Unsupported editor file extension".into(),
                );
            };
            continue_with(
                invocation,
                state,
                "Starting editor language server.",
                vec![process_request(
                    "spawn",
                    json!({
                        "name": state.process_name,
                        "program": server.program,
                        "args": server.args,
                    }),
                )],
            )
        }
        "resolving_switch" => {
            let Some(path) = invocation
                .events
                .iter()
                .find_map(|event| fs_message(event, "absolute_path"))
            else {
                return fail(
                    invocation,
                    state,
                    "Missing editor document path response".into(),
                );
            };
            let path = match path {
                Ok(path) => path,
                Err(error) => {
                    state.phase = "ready".into();
                    return with_state(invocation, state, Response::error(error));
                }
            };
            state.candidate_uri = editor_file_uri(&path);
            open_document(invocation, request, state, true)
        }
        "spawning" => {
            if let Some(Err(error)) = process_result(invocation, "spawn") {
                state.phase.clear();
                state.process_name.clear();
                state.document = None;
                return with_state(invocation, state, Response::error(error));
            }
            if process_result(invocation, "spawn").is_none() {
                return fail(
                    invocation,
                    state,
                    "Missing language-server spawn response".into(),
                );
            }
            begin_initialize(invocation, state)
        }
        "sending_initialize" => match process_result(invocation, "send") {
            Some(Ok(_)) => receive_next(invocation, state, "initializing"),
            Some(Err(error)) => fail(invocation, state, error),
            None => fail(
                invocation,
                state,
                "Missing language-server send response".into(),
            ),
        },
        "initializing" | "querying" => receive_message(invocation, request, state),
        "sending_query" => match process_result(invocation, "send") {
            Some(Ok(_)) => receive_next(invocation, state, "querying"),
            Some(Err(error)) => fail(invocation, state, error),
            None => fail(
                invocation,
                state,
                "Missing language-server send response".into(),
            ),
        },
        "closing" => match process_result(invocation, "send") {
            Some(Ok(_)) => {
                state.phase = "ready".into();
                result_response(invocation, state, Value::Null)
            }
            Some(Err(error)) => fail(invocation, state, error),
            None => fail(
                invocation,
                state,
                "Missing language-server close notification response".into(),
            ),
        },
        "sending_server_reply" => match process_result(invocation, "send") {
            Some(Ok(_)) => {
                let phase = state.resume_phase.clone();
                receive_next(invocation, state, &phase)
            }
            Some(Err(error)) => fail(invocation, state, error),
            None => fail(
                invocation,
                state,
                "Missing language-server reply response".into(),
            ),
        },
        "stopping" => {
            let error = if state.failure_message.is_empty() {
                "Editor language-server transport failed; its process was stopped.".into()
            } else {
                std::mem::take(&mut state.failure_message)
            };
            *state = EditorState::default();
            with_state(invocation, state, Response::error(error))
        }
        phase => reset_error(
            invocation,
            state,
            format!("Unknown editor LSP state phase `{phase}`"),
        ),
    }
}

fn begin_initialize(invocation: &Invocation, state: &mut EditorState) -> Response {
    let id = take_request_id(state);
    state.pending_request_id = id;
    state.pending_method = "initialize".into();
    state.phase = "sending_initialize".into();
    state.timeout_reads = 0;
    state.pump_steps = 0;
    let params = initialize_params(state);
    continue_with(
        invocation,
        state,
        "Initializing editor language server.",
        vec![send_frame(state, &lsp_request(id, "initialize", params))],
    )
}

fn initialize_params(state: &EditorState) -> Value {
    let root_name = Path::new(&state.workspace_root)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    json!({
        "processId": null,
        "rootPath": state.workspace_root,
        "rootUri": state.root_uri,
        "workspaceFolders": [{"uri":state.root_uri,"name":root_name}],
        "capabilities": {
            "general": {"positionEncodings":["utf-16"]},
            "workspace": {"workspaceFolders":true},
            "textDocument": {
                "completion": {"completionItem":{"snippetSupport":false}},
                "diagnostic": {"dynamicRegistration":false},
                "codeAction": {
                    "codeActionLiteralSupport": {
                        "codeActionKind": {
                            "valueSet": ["quickfix", "refactor", "source"],
                        },
                    },
                },
            },
        },
    })
}

fn receive_message(
    invocation: &Invocation,
    request: &EditorRequest,
    state: &mut EditorState,
) -> Response {
    let phase = state.phase.clone();
    let Some(message) = invocation
        .events
        .iter()
        .find_map(broker_recv_message)
    else {
        return fail(
            invocation,
            state,
            "Missing editor language-server response".into(),
        );
    };
    let message = match message {
        Ok(ProcessRecvMessage::Message(message)) => message,
        Ok(ProcessRecvMessage::Timeout) => {
            state.timeout_reads = state.timeout_reads.saturating_add(1);
            let (limit, phase_name) = if phase == "initializing" {
                (MAX_STARTUP_EMPTY_READS, "startup")
            } else {
                (MAX_QUERY_EMPTY_READS, "query")
            };
            if state.timeout_reads >= limit {
                return fail(
                    invocation,
                    state,
                    format!("Timed out waiting for language-server {phase_name} response"),
                );
            }
            return receive_next(invocation, state, &phase);
        }
        Ok(ProcessRecvMessage::Eof) => {
            return fail(
                invocation,
                state,
                "process/recv reached EOF before a JSON-RPC response".into(),
            )
        }
        Err(error) => return fail(invocation, state, error),
    };
    if message["jsonrpc"] != "2.0" {
        return fail(
            invocation,
            state,
            "Language server returned an invalid JSON-RPC version".into(),
        );
    }
    if message.get("method").and_then(Value::as_str).is_some() {
        cache_published_diagnostics(state, &message);
        if message.get("id").is_some() {
            return answer_server_request(invocation, state, &message, &phase);
        }
        return pump_again(invocation, state, &phase);
    }
    let Some(id) = message.get("id").and_then(Value::as_u64) else {
        return fail(
            invocation,
            state,
            "Language server returned an invalid JSON-RPC response".into(),
        );
    };
    if id != state.pending_request_id {
        return pump_again(invocation, state, &phase);
    }
    state.timeout_reads = 0;
    if message.get("error").is_some() {
        let detail = message["error"]["message"]
            .as_str()
            .unwrap_or("Language server request failed");
        if phase == "initializing" {
            return fail(invocation, state, detail.to_owned());
        }
        if state.pending_operation == "diagnostics" {
            state.phase = "ready".into();
            state.pending_method.clear();
            return result_response(invocation, state, Value::Null);
        }
        state.phase = "ready".into();
        return with_state(invocation, state, Response::error(detail.to_owned()));
    }
    if message.get("result").is_none() {
        return fail(
            invocation,
            state,
            "Language server response is missing a result".into(),
        );
    }
    if phase == "initializing" {
        let capabilities = &message["result"]["capabilities"];
        let encoding = capabilities
            .get("positionEncoding")
            .and_then(Value::as_str)
            .unwrap_or("utf-16");
        if encoding != "utf-16" {
            return fail(
                invocation,
                state,
                format!("Language server selected unsupported position encoding `{encoding}`"),
            );
        }
        state.server_capabilities = json!({
            "pull_diagnostics": capabilities.get("diagnosticProvider").is_some_and(|value| !value.is_null() && value != false),
        });
        return open_document(invocation, request, state, false);
    }

    let result = message.get("result").cloned().unwrap_or(Value::Null);
    if state.pending_operation == "diagnostics"
        && state.pending_method == "textDocument/diagnostic"
        && result["kind"] == "full"
    {
        if let Some(items) = result.get("items").and_then(Value::as_array) {
            if bounded_diagnostics(items) {
                state.diagnostics = Some(items.clone());
                state.diagnostics_version = state.document.as_ref().map(|doc| doc.server_version);
            }
        }
    }
    state.phase = "ready".into();
    state.pending_method.clear();
    state.pump_steps = 0;
    result_response(invocation, state, result)
}

fn answer_server_request(
    invocation: &Invocation,
    state: &mut EditorState,
    message: &Value,
    resume_phase: &str,
) -> Response {
    if !advance_pump(state) {
        return fail(
            invocation,
            state,
            "Editor language-server response budget exhausted".into(),
        );
    }
    let Some(id) = message.get("id") else {
        return fail(invocation, state, "Invalid language-server request".into());
    };
    let method = message["method"].as_str().unwrap_or("");
    let response = match method {
        "workspace/configuration" => {
            let count = message["params"]["items"].as_array().map_or(0, Vec::len);
            json!({"jsonrpc":"2.0","id":id,"result":vec![Value::Null;count]})
        }
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability" => {
            json!({"jsonrpc":"2.0","id":id,"result":null})
        }
        "workspace/workspaceFolders" => {
            json!({"jsonrpc":"2.0","id":id,"result":[{"uri":state.root_uri,"name":Path::new(&state.workspace_root).file_name().and_then(|name|name.to_str()).unwrap_or("workspace")} ]})
        }
        "workspace/applyEdit" => json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32601,"message":"Editor LSP does not apply workspace edits"},
        }),
        _ => json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32601,"message":"Client method not supported"},
        }),
    };
    state.phase = "sending_server_reply".into();
    state.resume_phase = resume_phase.into();
    continue_with(
        invocation,
        state,
        "Responding to language-server request.",
        vec![send_frame(state, &response)],
    )
}

fn pump_again(invocation: &Invocation, state: &mut EditorState, phase: &str) -> Response {
    if !advance_pump(state) {
        return fail(
            invocation,
            state,
            "Editor language-server response budget exhausted".into(),
        );
    }
    receive_next(invocation, state, phase)
}

fn advance_pump(state: &mut EditorState) -> bool {
    state.pump_steps = state.pump_steps.saturating_add(1);
    state.pump_steps <= MAX_EDITOR_PUMP_STEPS
}

fn receive_next(invocation: &Invocation, state: &mut EditorState, phase: &str) -> Response {
    state.phase = phase.into();
    continue_with(
        invocation,
        state,
        "Waiting for editor language server.",
        vec![process_request(
            "recv",
            json!({
                "name": state.process_name,
                "framing": "content-length",
                "timeout_ms": EDITOR_RECV_TIMEOUT_MS,
            }),
        )],
    )
}

fn open_document(
    invocation: &Invocation,
    request: &EditorRequest,
    state: &mut EditorState,
    switch: bool,
) -> Response {
    let Some(server_version) = state.last_server_version.checked_add(1) else {
        return fail(
            invocation,
            state,
            "Editor language-server document version exhausted".into(),
        );
    };
    let uri = state.candidate_uri.clone();
    let document = DocumentState {
        path: request.path.clone(),
        uri,
        document_id: request.document_id,
        version: request.version,
        server_version,
        language_id: state.language_id.clone(),
    };
    let frames = open_notifications(
        if switch {
            state.document.as_ref()
        } else {
            None
        },
        &document,
        &request.text,
        !switch,
    );
    state.last_server_version = server_version;
    state.document = Some(document);
    invalidate_diagnostics(state);
    start_query(invocation, request, state, frames)
}

fn open_notifications(
    previous: Option<&DocumentState>,
    document: &DocumentState,
    text: &str,
    initialized: bool,
) -> Vec<Value> {
    let mut frames = Vec::new();
    if initialized {
        frames.push(lsp_notification("initialized", json!({})));
    } else if let Some(previous) = previous {
        frames.push(lsp_notification(
            "textDocument/didClose",
            json!({"textDocument":{"uri":previous.uri}}),
        ));
    }
    frames.push(lsp_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": document.uri,
                "languageId": document.language_id,
                "version": document.server_version,
                "text": text,
            },
        }),
    ));
    frames
}

fn change_notification(document: &DocumentState, version: i32, text: &str) -> Value {
    lsp_notification(
        "textDocument/didChange",
        json!({
            "textDocument": {"uri":document.uri,"version":version},
            "contentChanges": [{"text":text}],
        }),
    )
}

fn close_notification(document: &DocumentState) -> Value {
    lsp_notification(
        "textDocument/didClose",
        json!({"textDocument":{"uri":document.uri}}),
    )
}

fn document_matches(document: &DocumentState, request: &EditorRequest) -> bool {
    document.document_id == request.document_id && document.path == request.path
}

fn start_query(
    invocation: &Invocation,
    request: &EditorRequest,
    state: &mut EditorState,
    mut frames: Vec<Value>,
) -> Response {
    state.timeout_reads = 0;
    state.pump_steps = 0;
    let Some(document) = state.document.as_ref() else {
        return with_state(
            invocation,
            state,
            Response::error("Editor LSP document is not open"),
        );
    };
    let (method, params) = match request.operation.as_str() {
        "completion" => {
            let context = request.trigger_character.as_ref().map_or_else(
                || json!({"triggerKind":1}),
                |character| json!({"triggerKind":2,"triggerCharacter":character}),
            );
            (
                "textDocument/completion",
                json!({
                    "textDocument":{"uri":document.uri},
                    "position":request.position,
                    "context":context,
                }),
            )
        }
        "hover" => (
            "textDocument/hover",
            json!({"textDocument":{"uri":document.uri},"position":request.position}),
        ),
        "definition" => (
            "textDocument/definition",
            json!({"textDocument":{"uri":document.uri},"position":request.position}),
        ),
        "code_actions" => (
            "textDocument/codeAction",
            json!({
                "textDocument":{"uri":document.uri},
                "range":request.range,
                "context":{"diagnostics":current_diagnostics(state)},
            }),
        ),
        "diagnostics" if state.server_capabilities["pull_diagnostics"] == true => (
            "textDocument/diagnostic",
            json!({"textDocument":{"uri":document.uri}}),
        ),
        "diagnostics" => (
            "textDocument/documentSymbol",
            json!({"textDocument":{"uri":document.uri}}),
        ),
        _ => {
            return with_state(
                invocation,
                state,
                Response::error("Unsupported editor LSP operation"),
            )
        }
    };
    let id = take_request_id(state);
    state.pending_request_id = id;
    state.pending_method = method.into();
    state.pending_operation = request.operation.clone();
    state.phase = "sending_query".into();
    frames.push(lsp_request(id, method, params));
    let data = frames
        .iter()
        .map(frame_jsonrpc)
        .collect::<Vec<_>>()
        .join("");
    continue_with(
        invocation,
        state,
        format!("Querying {}.", state.server),
        vec![process_request(
            "send",
            json!({"name":state.process_name,"data":data}),
        )],
    )
}

fn close_document_response(state: &mut EditorState) {
    state.document = None;
    state.phase = "ready".into();
    state.pending_method.clear();
    invalidate_diagnostics(state);
}

fn current_diagnostics(state: &EditorState) -> Vec<Value> {
    match (state.diagnostics_version, state.document.as_ref()) {
        (Some(version), Some(document)) if version == document.server_version => {
            state.diagnostics.clone().unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn cache_published_diagnostics(state: &mut EditorState, message: &Value) {
    if message["method"] != "textDocument/publishDiagnostics" {
        return;
    }
    let Some(document) = state.document.as_ref() else {
        return;
    };
    if message["params"]["uri"].as_str() != Some(document.uri.as_str())
        || message["params"]["version"]
            .as_i64()
            .and_then(|version| i32::try_from(version).ok())
            != Some(document.server_version)
    {
        return;
    }
    let Some(items) = message["params"]["diagnostics"].as_array() else {
        return;
    };
    if bounded_diagnostics(items) {
        state.diagnostics = Some(items.clone());
        state.diagnostics_version = Some(document.server_version);
    }
}

fn bounded_diagnostics(items: &[Value]) -> bool {
    items.len() <= MAX_CACHED_DIAGNOSTICS
        && serde_json::to_vec(items).is_ok_and(|bytes| bytes.len() <= MAX_EDITOR_LSP_BYTES)
}

fn invalidate_diagnostics(state: &mut EditorState) {
    state.diagnostics = None;
    state.diagnostics_version = None;
}

fn take_request_id(state: &mut EditorState) -> u64 {
    let id = state.next_request_id.max(1);
    state.next_request_id = id.saturating_add(1);
    id
}

fn send_frame(state: &EditorState, message: &Value) -> BrokerRequest {
    process_request(
        "send",
        json!({"name":state.process_name,"data":frame_jsonrpc(message)}),
    )
}

fn process_result(invocation: &Invocation, operation: &str) -> Option<Result<Value, String>> {
    invocation
        .events
        .iter()
        .find_map(|event| broker_message(event, operation))
}

fn continue_with(
    invocation: &Invocation,
    state: &EditorState,
    message: impl Into<String>,
    requests: Vec<BrokerRequest>,
) -> Response {
    for request in &requests {
        send_broker_request(request);
    }
    let mut response = Response::continue_after_broker(message);
    response.state = Some(outer_state(invocation, state));
    response
}

fn result_response(invocation: &Invocation, state: &mut EditorState, result: Value) -> Response {
    if state.phase == "closing" {
        close_document_response(state);
    }
    let diagnostics = if state
        .document
        .as_ref()
        .is_some_and(|document| state.diagnostics_version == Some(document.server_version))
    {
        state.diagnostics.clone()
    } else {
        None
    };
    let server_document_version = state
        .document
        .as_ref()
        .map(|document| document.server_version);
    result_response_with_diagnostics(
        invocation,
        state,
        result,
        diagnostics,
        server_document_version,
    )
}

fn result_response_with_diagnostics(
    invocation: &Invocation,
    state: &EditorState,
    result: Value,
    diagnostics: Option<Vec<Value>>,
    server_document_version: Option<i32>,
) -> Response {
    let response = json!({
        "server": if state.server.is_empty() {
            "none"
        } else {
            state.server.as_str()
        },
        "result": result,
        "diagnostics": diagnostics,
        "server_document_version": server_document_version,
    });
    if serde_json::to_vec(&response).map_or(true, |bytes| bytes.len() > MAX_EDITOR_LSP_BYTES) {
        return with_state(
            invocation,
            state,
            Response::error("Editor LSP response exceeds the 1 MiB limit"),
        );
    }
    let mut response = Response::ok(response.to_string());
    response.state = Some(outer_state(invocation, state));
    response
}

fn outer_state(invocation: &Invocation, state: &EditorState) -> Value {
    let mut outer = invocation.state.as_object().cloned().unwrap_or_default();
    outer.insert(
        "editor".into(),
        serde_json::to_value(state).unwrap_or_else(|_| json!({})),
    );
    Value::Object(outer)
}

fn with_state(invocation: &Invocation, state: &EditorState, mut response: Response) -> Response {
    response.state = Some(outer_state(invocation, state));
    response
}

fn reset_error(invocation: &Invocation, state: &mut EditorState, error: String) -> Response {
    *state = EditorState::default();
    with_state(invocation, state, Response::error(error))
}

fn fail(invocation: &Invocation, state: &mut EditorState, error: String) -> Response {
    if state.process_name.is_empty() {
        return reset_error(invocation, state, error);
    }
    state.failure_message = error;
    state.phase = "stopping".into();
    continue_with(
        invocation,
        state,
        "Stopping failed editor language server.",
        vec![process_request("kill", json!({"name":state.process_name}))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor_invocation(state: Value, events: Vec<super::super::ExtensionEvent>) -> Invocation {
        Invocation {
            name: "editor_lsp".into(),
            arguments: json!({
                "operation":"completion",
                "path":"src/main.rs",
                "document_id":7,
                "version":1,
                "text":"fn main() {}",
                "position":{"line":0,"character":0},
            }),
            state,
            events,
        }
    }

    fn extension_state(state: &EditorState) -> Value {
        json!({"editor":serde_json::to_value(state).unwrap()})
    }

    fn process_event(operation: &str, message: Value) -> super::super::ExtensionEvent {
        super::super::ExtensionEvent {
            topic: "broker_response".into(),
            payload: json!({
            "api_version":2,
            "receipt_id":1,
            "scope":"editor-session",
            "capability":"process",
                "operation":operation,
                "ok":true,
                "value":{"message":message.to_string()},
            }),
        }
    }

    fn recv_event(data: &str, eof: bool) -> super::super::ExtensionEvent {
        process_event("recv", json!({"data":data,"eof":eof}))
    }

    fn process_error_event(
        operation: &str,
        code: &str,
        message: &str,
    ) -> super::super::ExtensionEvent {
        super::super::ExtensionEvent {
            topic: "broker_response".into(),
            payload: json!({
            "api_version":2,
            "receipt_id":1,
            "scope":"editor-session",
            "capability":"process",
                "operation":operation,
                "ok":false,
                "error":{"code":code,"message":message},
            }),
        }
    }

    fn initializing_state() -> EditorState {
        EditorState {
            phase: "initializing".into(),
            server: "rust-analyzer".into(),
            process_name: "lsp-editor-rust-analyzer".into(),
            language_id: "rust".into(),
            workspace_root: "/workspace".into(),
            root_uri: "file:///workspace".into(),
            candidate_uri: "file:///workspace/src/main.rs".into(),
            pending_request_id: 1,
            next_request_id: 2,
            pending_method: "initialize".into(),
            ..EditorState::default()
        }
    }

    #[test]
    fn editor_server_map_is_fixed_and_uses_language_specific_ids() {
        assert_eq!(server_for("src/main.rs").unwrap().language_id, "rust");
        assert_eq!(
            server_for("src/view.jsx").unwrap().language_id,
            "javascriptreact"
        );
        assert_eq!(
            server_for("src/view.tsx").unwrap().language_id,
            "typescriptreact"
        );
        assert_eq!(server_for("src/view.js").unwrap().language_id, "javascript");
        assert_eq!(server_for("src/view.ts").unwrap().language_id, "typescript");
        assert_eq!(server_for("src/main.go").unwrap().program, "gopls");
        assert_eq!(
            server_for("src/main.pyi").unwrap().program,
            "pyright-langserver"
        );
        assert!(server_for("src/main.rb").is_none());
    }

    #[test]
    fn editor_checkpoint_contains_metadata_but_never_buffer_contents() {
        let source = "unsaved-secret-source";
        let state = EditorState {
            phase: "ready".into(),
            server: "rust-analyzer".into(),
            process_name: "lsp-editor-rust-analyzer".into(),
            document: Some(DocumentState {
                path: "src/lib.rs".into(),
                uri: "file:///workspace/src/lib.rs".into(),
                document_id: 7,
                version: 3,
                server_version: 2,
                language_id: "rust".into(),
            }),
            ..EditorState::default()
        };
        let serialized = serde_json::to_string(&state).unwrap();
        assert!(serialized.contains("document_id"));
        assert!(!serialized.contains(source));
    }

    #[test]
    fn open_change_and_switch_notifications_carry_only_current_unsaved_text() {
        let previous = DocumentState {
            path: "src/old.rs".into(),
            uri: "file:///workspace/src/old.rs".into(),
            document_id: 4,
            version: 3,
            server_version: 5,
            language_id: "rust".into(),
        };
        let current = DocumentState {
            path: "src/new.rs".into(),
            uri: "file:///workspace/src/new.rs".into(),
            document_id: 9,
            version: 1,
            server_version: 6,
            language_id: "rust".into(),
        };
        let source = "unsaved source";
        let opened = open_notifications(None, &current, source, true);
        assert_eq!(opened[0]["method"], "initialized");
        assert_eq!(opened[1]["method"], "textDocument/didOpen");
        assert_eq!(opened[1]["params"]["textDocument"]["text"], source);
        let changed = change_notification(&current, 7, source);
        assert_eq!(changed["method"], "textDocument/didChange");
        assert_eq!(changed["params"]["textDocument"]["version"], 7);
        assert_eq!(changed["params"]["contentChanges"][0]["text"], source);

        let switched = open_notifications(Some(&previous), &current, source, false);
        assert_eq!(switched[0]["method"], "textDocument/didClose");
        assert_eq!(switched[0]["params"]["textDocument"]["uri"], previous.uri);
        assert_eq!(switched[1]["method"], "textDocument/didOpen");
        assert_eq!(switched[1]["params"]["textDocument"]["uri"], current.uri);
        assert!(document_matches(
            &current,
            &EditorRequest {
                operation: "close".into(),
                path: "src/new.rs".into(),
                document_id: 9,
                version: 1,
                text: String::new(),
                position: json!({"line":0,"character":0}),
                range: None,
                trigger_character: None,
            }
        ));
        assert!(!document_matches(
            &current,
            &EditorRequest {
                operation: "close".into(),
                path: "src/new.rs".into(),
                document_id: 4,
                version: 1,
                text: String::new(),
                position: json!({"line":0,"character":0}),
                range: None,
                trigger_character: None,
            }
        ));
    }

    #[test]
    fn stale_close_preserves_the_current_document_without_echoing_its_diagnostics() {
        let invocation = Invocation {
            name: "editor_lsp".into(),
            arguments: json!({}),
            state: json!({}),
            events: vec![],
        };
        let document = DocumentState {
            path: "src/current.rs".into(),
            uri: "file:///workspace/src/current.rs".into(),
            document_id: 9,
            version: 4,
            server_version: 6,
            language_id: "rust".into(),
        };
        let mut state = EditorState {
            phase: "ready".into(),
            server: "rust-analyzer".into(),
            process_name: "lsp-editor-rust-analyzer".into(),
            document: Some(document.clone()),
            diagnostics: Some(vec![json!({"message":"current buffer only"})]),
            diagnostics_version: Some(document.server_version),
            ..EditorState::default()
        };
        let response = close_document(
            &invocation,
            &EditorRequest {
                operation: "close".into(),
                path: document.path.clone(),
                document_id: 8,
                version: 3,
                text: String::new(),
                position: json!({"line":0,"character":0}),
                range: None,
                trigger_character: None,
            },
            &mut state,
        );
        let body: Value = serde_json::from_str(&response.message).unwrap();
        assert_eq!(body["result"], Value::Null);
        assert_eq!(body["diagnostics"], Value::Null);
        assert_eq!(body["server_document_version"], Value::Null);
        assert_eq!(state.document.as_ref().unwrap().document_id, 9);
        assert_eq!(state.diagnostics_version, Some(document.server_version));
    }

    #[test]
    fn server_requests_are_bounded_by_the_pump_budget() {
        let invocation = Invocation {
            name: "editor_lsp".into(),
            arguments: json!({}),
            state: json!({}),
            events: vec![],
        };
        let mut state = EditorState {
            process_name: "lsp-editor-rust-analyzer".into(),
            pump_steps: MAX_EDITOR_PUMP_STEPS,
            ..EditorState::default()
        };
        let response = answer_server_request(
            &invocation,
            &mut state,
            &json!({
                "id":1,
                "method":"workspace/configuration",
                "params":{"items":[]},
            }),
            "querying",
        );
        assert!(response.continue_after_broker);
        assert_eq!(state.phase, "stopping");
    }

    #[test]
    fn push_diagnostics_require_matching_uri_and_server_version() {
        let mut state = EditorState {
            document: Some(DocumentState {
                path: "src/lib.rs".into(),
                uri: "file:///workspace/src/lib.rs".into(),
                document_id: 7,
                version: 3,
                server_version: 4,
                language_id: "rust".into(),
            }),
            ..EditorState::default()
        };
        let message = json!({
            "method":"textDocument/publishDiagnostics",
            "params":{
                "uri":"file:///workspace/src/lib.rs",
                "version":3,
                "diagnostics":[{"message":"stale"}],
            },
        });
        cache_published_diagnostics(&mut state, &message);
        assert!(state.diagnostics.is_none());
        let mut current = message;
        current["params"]["version"] = json!(4);
        cache_published_diagnostics(&mut state, &current);
        assert_eq!(current_diagnostics(&state).len(), 1);
        current["params"]["uri"] = json!("file:///workspace/other.rs");
        current["params"]["diagnostics"] = json!([{"message":"wrong-uri"}]);
        cache_published_diagnostics(&mut state, &current);
        assert_eq!(current_diagnostics(&state).len(), 1);
    }

    #[test]
    fn editor_state_is_nested_without_discarding_other_extension_state() {
        let invocation = Invocation {
            name: "editor_lsp".into(),
            arguments: json!({}),
            state: json!({"server":"agent-lsp","other":{"kept":true}}),
            events: vec![],
        };
        let state = EditorState {
            phase: "ready".into(),
            ..EditorState::default()
        };
        let outer = outer_state(&invocation, &state);
        assert_eq!(outer["server"], "agent-lsp");
        assert_eq!(outer["other"]["kept"], true);
        assert_eq!(outer["editor"]["phase"], "ready");
    }

    #[test]
    fn editor_file_uris_encode_unix_windows_and_unc_paths() {
        assert_eq!(
            editor_file_uri("/tmp/a b/é#.rs"),
            "file:///tmp/a%20b/%C3%A9%23.rs"
        );
        assert_eq!(
            editor_file_uri(r"C:\Users\dev\file name.ts"),
            "file:///C:/Users/dev/file%20name.ts"
        );
        assert_eq!(
            editor_file_uri(r"\\server\share\folder name\file.rs"),
            "file://server/share/folder%20name/file.rs"
        );
        assert_eq!(
            editor_file_uri(r"\\?\C:\repo\file name.rs"),
            "file:///C:/repo/file%20name.rs"
        );
        assert_eq!(
            editor_file_uri(r"\\?\UNC\server\share\file name.rs"),
            "file://server/share/file%20name.rs"
        );
    }

    #[test]
    fn initialization_advertises_safe_code_action_literals_only() {
        let params = initialize_params(&initializing_state());
        let capabilities = &params["capabilities"];
        assert_eq!(
            capabilities["textDocument"]["codeAction"]["codeActionLiteralSupport"]
                ["codeActionKind"]["valueSet"],
            json!(["quickfix", "refactor", "source"])
        );
        assert!(capabilities["workspace"].get("applyEdit").is_none());
        assert!(
            capabilities["textDocument"]["codeAction"]
                .get("resolveSupport")
                .is_none()
        );
        assert!(capabilities["workspace"].get("executeCommand").is_none());
    }

    #[test]
    fn absent_editor_metadata_cleans_only_editor_processes() {
        assert_eq!(
            editor_processes_to_kill("", "lsp-editor-rust-analyzer"),
            vec!["lsp-editor-rust-analyzer"]
        );
        assert_eq!(
            editor_processes_to_kill("lsp-rust-analyzer", "lsp-editor-rust-analyzer"),
            vec!["lsp-editor-rust-analyzer"]
        );
        assert_eq!(
            editor_processes_to_kill(
                "lsp-editor-typescript-language-server",
                "lsp-editor-rust-analyzer",
            ),
            vec![
                "lsp-editor-typescript-language-server",
                "lsp-editor-rust-analyzer",
            ]
        );
    }

    #[test]
    fn empty_live_read_retries_and_initialization_can_complete() {
        let state = extension_state(&initializing_state());
        let response = super::handle(&editor_invocation(
            state,
            vec![recv_event("", false)],
        ));
        assert!(response.continue_after_broker);
        let state = response.state.unwrap();
        let editor: EditorState = serde_json::from_value(state["editor"].clone()).unwrap();
        assert_eq!(editor.phase, "initializing");
        assert_eq!(editor.timeout_reads, 1);

        let initialize = json!({
            "jsonrpc":"2.0",
            "id":1,
            "result":{"capabilities":{"positionEncoding":"utf-16"}},
        });
        let response = super::handle(&editor_invocation(
            state,
            vec![recv_event(&initialize.to_string(), false)],
        ));
        assert!(response.continue_after_broker);
        let editor: EditorState =
            serde_json::from_value(response.state.unwrap()["editor"].clone()).unwrap();
        assert_eq!(editor.phase, "sending_query");
        assert_eq!(editor.timeout_reads, 0);
        assert_eq!(editor.document.unwrap().server_version, 1);
    }

    #[test]
    fn startup_and_query_timeout_budgets_fail_with_actionable_errors() {
        for (phase, expected_limit, expected_error) in [
            (
                "initializing",
                MAX_STARTUP_EMPTY_READS,
                "Timed out waiting for language-server startup response",
            ),
            (
                "querying",
                MAX_QUERY_EMPTY_READS,
                "Timed out waiting for language-server query response",
            ),
        ] {
            let mut state = EditorState {
                phase: phase.into(),
                server: "rust-analyzer".into(),
                process_name: "lsp-editor-rust-analyzer".into(),
                pending_request_id: 3,
                pending_method: "textDocument/completion".into(),
                pending_operation: "completion".into(),
                ..EditorState::default()
            };
            if phase == "querying" {
                state.document = Some(DocumentState {
                    path: "src/main.rs".into(),
                    uri: "file:///workspace/src/main.rs".into(),
                    document_id: 7,
                    version: 1,
                    server_version: 2,
                    language_id: "rust".into(),
                });
                state.last_server_version = 2;
            }
            let mut state = extension_state(&state);
            for _ in 0..expected_limit {
                let response = super::handle(&editor_invocation(state, vec![recv_event("", false)]));
                assert!(response.continue_after_broker);
                state = response.state.unwrap();
            }
            let editor: EditorState =
                serde_json::from_value(state["editor"].clone()).unwrap();
            assert_eq!(editor.phase, "stopping");
            assert_eq!(editor.timeout_reads, expected_limit);

            let stopped = super::handle(&editor_invocation(state, vec![]));
            assert_eq!(stopped.error.as_deref(), Some(expected_error));
        }
    }

    #[test]
    fn eof_fails_without_spending_the_timeout_budget() {
        let state = extension_state(&initializing_state());
        let response =
            super::handle(&editor_invocation(state, vec![recv_event("", true)]));
        let state = response.state.unwrap();
        let editor: EditorState = serde_json::from_value(state["editor"].clone()).unwrap();
        assert_eq!(editor.phase, "stopping");
        assert_eq!(editor.timeout_reads, 0);
        assert_eq!(
            editor.failure_message,
            "process/recv reached EOF before a JSON-RPC response"
        );
    }

    #[test]
    fn query_completes_after_notifications_and_empty_reads() {
        let document = DocumentState {
            path: "src/main.rs".into(),
            uri: "file:///workspace/src/main.rs".into(),
            document_id: 7,
            version: 2,
            server_version: 4,
            language_id: "rust".into(),
        };
        let state = EditorState {
            phase: "querying".into(),
            server: "rust-analyzer".into(),
            process_name: "lsp-editor-rust-analyzer".into(),
            document: Some(document.clone()),
            last_server_version: document.server_version,
            pending_request_id: 9,
            pending_method: "textDocument/completion".into(),
            pending_operation: "completion".into(),
            ..EditorState::default()
        };
        let notification = json!({
            "jsonrpc":"2.0",
            "method":"textDocument/publishDiagnostics",
            "params":{
                "uri":document.uri,
                "version":document.server_version,
                "diagnostics":[{"message":"warning"}],
            },
        });
        let response = super::handle(&editor_invocation(
            extension_state(&state),
            vec![recv_event(&notification.to_string(), false)],
        ));
        let state = response.state.unwrap();
        let editor: EditorState = serde_json::from_value(state["editor"].clone()).unwrap();
        assert_eq!(editor.pump_steps, 1);
        assert_eq!(editor.timeout_reads, 0);
        assert_eq!(current_diagnostics(&editor).len(), 1);

        let response = super::handle(&editor_invocation(state, vec![recv_event("", false)]));
        let state = response.state.unwrap();
        let editor: EditorState = serde_json::from_value(state["editor"].clone()).unwrap();
        assert_eq!(editor.phase, "querying");
        assert_eq!(editor.pump_steps, 1);
        assert_eq!(editor.timeout_reads, 1);

        let completion = json!({"jsonrpc":"2.0","id":9,"result":{"items":[]}});
        let response = super::handle(&editor_invocation(
            state,
            vec![recv_event(&completion.to_string(), false)],
        ));
        let body: Value = serde_json::from_str(&response.message).unwrap();
        assert_eq!(body["server_document_version"], 4);
        assert_eq!(body["diagnostics"].as_array().unwrap().len(), 1);
        let editor: EditorState =
            serde_json::from_value(response.state.unwrap()["editor"].clone()).unwrap();
        assert_eq!(editor.phase, "ready");
        assert_eq!(editor.timeout_reads, 0);
        assert_eq!(editor.pump_steps, 0);
    }

    #[test]
    fn close_send_failure_stops_transport_and_preserves_the_broker_error() {
        let state = EditorState {
            phase: "closing".into(),
            server: "rust-analyzer".into(),
            process_name: "lsp-editor-rust-analyzer".into(),
            ..EditorState::default()
        };
        let response = super::handle(&editor_invocation(
            extension_state(&state),
            vec![process_error_event("send", "permission_denied", "close denied")],
        ));
        let state = response.state.unwrap();
        let editor: EditorState = serde_json::from_value(state["editor"].clone()).unwrap();
        assert_eq!(editor.phase, "stopping");
        assert!(editor
            .failure_message
            .contains("process/send broker error permission_denied: close denied"));

        let response = super::handle(&editor_invocation(state, vec![]));
        assert_eq!(
            response.error.as_deref(),
            Some("process/send broker error permission_denied: close denied")
        );
    }

    #[test]
    fn server_document_version_stops_before_i32_overflow() {
        let document = DocumentState {
            path: "src/main.rs".into(),
            uri: "file:///workspace/src/main.rs".into(),
            document_id: 7,
            version: 1,
            server_version: i32::MAX,
            language_id: "rust".into(),
        };
        let state = EditorState {
            phase: "ready".into(),
            server: "rust-analyzer".into(),
            process_name: "lsp-editor-rust-analyzer".into(),
            document: Some(document),
            last_server_version: i32::MAX,
            ..EditorState::default()
        };
        let mut invocation = editor_invocation(
            extension_state(&state),
            vec![],
        );
        invocation.arguments["version"] = json!(2);
        let response = super::handle(&invocation);
        let editor: EditorState =
            serde_json::from_value(response.state.unwrap()["editor"].clone()).unwrap();
        assert_eq!(editor.phase, "stopping");
        assert_eq!(editor.last_server_version, i32::MAX);
        assert_eq!(
            editor.failure_message,
            "Editor language-server document version exhausted"
        );
    }
}
