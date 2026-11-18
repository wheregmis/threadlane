//! Serves the system's tools to a running backend over MCP (Plan.md §3.3 "Tool server").
//!
//! See `docs/milestone-1.md` §4. One [`McpServer`] serves one run: it listens
//! on a free loopback port, requires a random bearer token on every request,
//! and answers every `POST` with plain `application/json` (never SSE).

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use serde_json::{Value as Json, json};
use threadlane_core::tools::{ToolDefinition, ToolEndpoint};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// The name backends see the server under.
pub const SERVER_NAME: &str = "threadlane";

/// The path the server answers on.
const PATH: &str = "/mcp";

/// The protocol version answered when the client does not ask for one.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

/// The `_meta` key under which Claude sends its `tool_use` id.
const TOOL_USE_ID_META: &str = "claudecode/toolUseId";

/// How long [`McpServer::shutdown`] lets in-flight requests finish before
/// abandoning them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// JSON-RPC error codes (JSON-RPC 2.0 §5.1).
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Why the tool server could not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// Binding the loopback listener failed.
    #[error("could not listen on loopback: {0}")]
    Bind(#[source] std::io::Error),
}

/// A [`ToolEndpoint`] served over MCP on loopback HTTP until shut down or dropped.
pub struct McpServer {
    url: String,
    token: String,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl fmt::Debug for McpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServer")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .field("running", &self.task.is_some())
            .finish()
    }
}

impl McpServer {
    /// Binds 127.0.0.1 on a free port and serves `endpoint` until shut down or dropped.
    ///
    /// Must be called within a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`McpError::Bind`] if the loopback listener cannot be opened.
    pub async fn start(endpoint: Arc<dyn ToolEndpoint>) -> Result<Self, McpError> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .map_err(McpError::Bind)?;
        let addr = listener.local_addr().map_err(McpError::Bind)?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let shared = Arc::new(Shared {
            endpoint,
            expected_token: token.clone(),
        });
        let app = Router::new().route(PATH, any(handle)).with_state(shared);
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let serving = axum::serve(listener, app).with_graceful_shutdown(async {
                // A dropped sender also means stop.
                let _ = stopped.await;
            });
            if let Err(error) = serving.await {
                tracing::warn!(%error, "tool server stopped with an error");
            }
        });
        tracing::debug!(%addr, "tool server listening");
        Ok(Self {
            url: format!("http://{addr}{PATH}"),
            token,
            stop: Some(stop),
            task: Some(task),
        })
    }

    /// The URL to configure the backend with: `http://127.0.0.1:<port>/mcp`.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The secret every request must carry as `Authorization: Bearer <token>`.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Stops accepting connections at once and waits a short grace period
    /// for in-flight requests to finish. The listener is closed when this
    /// returns, even if a request is still running.
    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(SHUTDOWN_GRACE, &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct Shared {
    endpoint: Arc<dyn ToolEndpoint>,
    expected_token: String,
}

async fn handle(
    State(shared): State<Arc<Shared>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&headers, &shared.expected_token) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
        )
            .into_response();
    }
    match method {
        Method::POST => handle_post(&shared, &body).await,
        Method::DELETE => StatusCode::OK.into_response(),
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "POST, DELETE")],
        )
            .into_response(),
    }
}

fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };
    scheme.eq_ignore_ascii_case("bearer") && constant_time_eq(token.trim(), expected)
}

/// Compares without an early exit so response timing does not reveal how
/// much of a guessed token was right.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn handle_post(shared: &Shared, body: &[u8]) -> Response {
    let message: Json = match serde_json::from_slice(body) {
        Ok(message) => message,
        Err(error) => {
            tracing::debug!(%error, "tool server got a body that is not JSON");
            return rpc_failure(
                StatusCode::BAD_REQUEST,
                Json::Null,
                PARSE_ERROR,
                "parse error",
            );
        }
    };
    let Some(object) = message.as_object() else {
        return rpc_failure(
            StatusCode::BAD_REQUEST,
            Json::Null,
            INVALID_REQUEST,
            "invalid request",
        );
    };
    let id = object.get("id").cloned();
    let Some(method) = object.get("method").and_then(Json::as_str) else {
        // A JSON-RPC response from the client: this server sends no requests,
        // so there is nothing to match it to.
        if id.is_some() && (object.contains_key("result") || object.contains_key("error")) {
            return StatusCode::ACCEPTED.into_response();
        }
        return rpc_failure(
            StatusCode::BAD_REQUEST,
            id.unwrap_or(Json::Null),
            INVALID_REQUEST,
            "invalid request",
        );
    };
    let Some(id) = id else {
        tracing::debug!(method, "tool server got a notification");
        return StatusCode::ACCEPTED.into_response();
    };
    let params = object.get("params").cloned().unwrap_or(Json::Null);
    tracing::debug!(method, "tool server got a request");
    match answer(shared, method, &params).await {
        Ok(result) => rpc_response(json!({"jsonrpc": "2.0", "id": id, "result": result})),
        Err((code, message)) => rpc_failure(StatusCode::OK, id, code, &message),
    }
}

async fn answer(shared: &Shared, method: &str, params: &Json) -> Result<Json, (i64, String)> {
    match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(Json::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            let tools: Vec<Json> = shared
                .endpoint
                .definitions()
                .iter()
                .map(tool_entry)
                .collect();
            Ok(json!({"tools": tools}))
        }
        "tools/call" => call_tool(shared, params).await,
        other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
    }
}

fn tool_entry(definition: &ToolDefinition) -> Json {
    json!({
        "name": definition.name,
        "description": definition.description,
        "inputSchema": definition.input_schema,
    })
}

async fn call_tool(shared: &Shared, params: &Json) -> Result<Json, (i64, String)> {
    let Some(name) = params.get("name").and_then(Json::as_str) else {
        return Err((INVALID_PARAMS, "tools/call needs a tool name".to_owned()));
    };
    let arguments = match params.get("arguments") {
        None | Some(Json::Null) => json!({}),
        Some(arguments) => arguments.clone(),
    };
    let call_id = params
        .get("_meta")
        .and_then(|meta| meta.get(TOOL_USE_ID_META))
        .and_then(Json::as_str);
    let (text, is_error) = match shared.endpoint.call(call_id, name, arguments).await {
        Ok(result) => (result.to_string(), false),
        Err(error) => (error.to_string(), true),
    };
    Ok(json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    }))
}

fn rpc_response(body: Json) -> Response {
    (StatusCode::OK, axum::Json(body)).into_response()
}

fn rpc_failure(status: StatusCode, id: Json, code: i64, message: &str) -> Response {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    });
    (status, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use async_trait::async_trait;
    use threadlane_core::tools::{Tier, ToolError};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    type Call = (Option<String>, String, Json);

    #[derive(Debug, Default)]
    struct TestEndpoint {
        calls: Mutex<Vec<Call>>,
    }

    impl TestEndpoint {
        fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("lock poisoned").clone()
        }
    }

    #[async_trait]
    impl ToolEndpoint for TestEndpoint {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "echo".to_owned(),
                description: "Echoes its input".to_owned(),
                input_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}}),
                tier: Tier::Read,
                safe_to_repeat: true,
            }]
        }

        async fn call(
            &self,
            call_id: Option<&str>,
            name: &str,
            arguments: Json,
        ) -> Result<Json, ToolError> {
            self.calls.lock().expect("lock poisoned").push((
                call_id.map(str::to_owned),
                name.to_owned(),
                arguments.clone(),
            ));
            match name {
                "echo" => Ok(json!({"echo": arguments})),
                "blocked" => Err(ToolError::Blocked("not allowed here".to_owned())),
                other => Err(ToolError::UnknownTool(other.to_owned())),
            }
        }
    }

    struct Reply {
        status: u16,
        headers: String,
        body: String,
    }

    impl Reply {
        fn json(&self) -> Json {
            serde_json::from_str(&self.body).expect("reply body is JSON")
        }
    }

    async fn start() -> (Arc<TestEndpoint>, McpServer) {
        let endpoint = Arc::new(TestEndpoint::default());
        let server = McpServer::start(endpoint.clone())
            .await
            .expect("server starts");
        (endpoint, server)
    }

    fn address(server: &McpServer) -> String {
        server
            .url()
            .strip_prefix("http://")
            .and_then(|rest| rest.strip_suffix(PATH))
            .expect("url has the documented shape")
            .to_owned()
    }

    async fn send(address: &str, method: &str, authorization: Option<&str>, body: &str) -> Reply {
        let mut stream = TcpStream::connect(address).await.expect("connects");
        let mut request = format!("{method} {PATH} HTTP/1.1\r\nHost: {address}\r\n");
        if let Some(authorization) = authorization {
            request.push_str(&format!("Authorization: {authorization}\r\n"));
        }
        request.push_str(&format!(
            "Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        stream
            .write_all(request.as_bytes())
            .await
            .expect("writes request");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.expect("reads reply");
        let raw = String::from_utf8(raw).expect("reply is UTF-8");
        let (head, body) = raw.split_once("\r\n\r\n").expect("reply has a head");
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("status line has a code");
        Reply {
            status,
            headers: head.to_ascii_lowercase(),
            body: body.to_owned(),
        }
    }

    async fn post(server: &McpServer, body: &Json) -> Reply {
        let authorization = format!("Bearer {}", server.token());
        send(
            &address(server),
            "POST",
            Some(&authorization),
            &body.to_string(),
        )
        .await
    }

    #[tokio::test]
    async fn url_and_token_have_the_documented_shape() {
        let (_endpoint, server) = start().await;
        assert!(server.url().starts_with("http://127.0.0.1:"));
        assert!(server.url().ends_with("/mcp"));
        assert_eq!(server.token().len(), 64);
        assert!(!format!("{server:?}").contains(server.token()));
    }

    #[tokio::test]
    async fn requests_without_the_right_token_are_refused() {
        let (endpoint, server) = start().await;
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "echo", "arguments": {}}}).to_string();
        let address = address(&server);

        let missing = send(&address, "POST", None, &call).await;
        assert_eq!(missing.status, 401);

        let wrong = send(&address, "POST", Some("Bearer not-the-token"), &call).await;
        assert_eq!(wrong.status, 401);

        let prefix = &server.token()[..10];
        let partial = send(&address, "POST", Some(&format!("Bearer {prefix}")), &call).await;
        assert_eq!(partial.status, 401);

        let get = send(&address, "GET", Some("Bearer nope"), "").await;
        assert_eq!(get.status, 401);

        assert!(endpoint.calls().is_empty());
    }

    #[tokio::test]
    async fn initialize_echoes_the_requested_protocol_version() {
        let (_endpoint, server) = start().await;
        let reply = post(
            &server,
            &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "claude-code", "version": "2.1.325"},
            }}),
        )
        .await;
        assert_eq!(reply.status, 200);
        assert!(reply.headers.contains("content-type: application/json"));
        let body = reply.json();
        assert_eq!(body["id"], 0);
        assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(body["result"]["capabilities"], json!({"tools": {}}));
        assert_eq!(body["result"]["serverInfo"]["name"], SERVER_NAME);

        let older = post(
            &server,
            &json!({"jsonrpc": "2.0", "id": "a", "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
        )
        .await;
        assert_eq!(older.json()["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(older.json()["id"], "a");
    }

    #[tokio::test]
    async fn a_notification_is_accepted_with_no_body() {
        let (_endpoint, server) = start().await;
        let reply = post(
            &server,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        assert_eq!(reply.status, 202);
        assert!(reply.body.is_empty());
    }

    #[tokio::test]
    async fn ping_answers_an_empty_object() {
        let (_endpoint, server) = start().await;
        let reply = post(
            &server,
            &json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}),
        )
        .await;
        assert_eq!(reply.json()["result"], json!({}));
    }

    #[tokio::test]
    async fn tools_list_comes_from_the_endpoint() {
        let (_endpoint, server) = start().await;
        let reply = post(
            &server,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        )
        .await;
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.json()["result"],
            json!({"tools": [{
                "name": "echo",
                "description": "Echoes its input",
                "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
            }]})
        );
    }

    #[tokio::test]
    async fn tools_call_passes_the_claude_tool_use_id() {
        let (endpoint, server) = start().await;
        let reply = post(
            &server,
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                "name": "echo",
                "arguments": {"text": "hi"},
                "_meta": {"claudecode/toolUseId": "toolu_01ABC", "progressToken": 3},
            }}),
        )
        .await;
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.json()["result"],
            json!({
                "content": [{"type": "text", "text": r#"{"echo":{"text":"hi"}}"#}],
                "isError": false,
            })
        );
        assert_eq!(
            endpoint.calls(),
            vec![(
                Some("toolu_01ABC".to_owned()),
                "echo".to_owned(),
                json!({"text": "hi"})
            )]
        );
    }

    #[tokio::test]
    async fn tools_call_without_meta_has_no_call_id() {
        let (endpoint, server) = start().await;
        let reply = post(
            &server,
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "echo"}}),
        )
        .await;
        assert_eq!(reply.json()["result"]["isError"], false);
        assert_eq!(endpoint.calls(), vec![(None, "echo".to_owned(), json!({}))]);
    }

    #[tokio::test]
    async fn a_tool_error_is_a_result_the_model_can_read() {
        let (_endpoint, server) = start().await;
        let reply = post(
            &server,
            &json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "blocked", "arguments": {}}}),
        )
        .await;
        assert_eq!(reply.status, 200);
        let body = reply.json();
        assert!(body.get("error").is_none());
        assert_eq!(
            body["result"],
            json!({
                "content": [{"type": "text", "text": "blocked: not allowed here"}],
                "isError": true,
            })
        );
    }

    #[tokio::test]
    async fn an_unknown_method_is_method_not_found() {
        let (_endpoint, server) = start().await;
        let reply = post(
            &server,
            &json!({"jsonrpc": "2.0", "id": 0, "method": "server/discover", "params": {}}),
        )
        .await;
        assert_eq!(reply.status, 200);
        let body = reply.json();
        assert_eq!(body["id"], 0);
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn malformed_bodies_are_json_rpc_errors() {
        let (endpoint, server) = start().await;
        let authorization = format!("Bearer {}", server.token());
        let address = address(&server);

        let not_json = send(&address, "POST", Some(&authorization), "{not json").await;
        assert_eq!(not_json.status, 400);
        assert_eq!(not_json.json()["error"]["code"], -32700);

        let not_object = send(&address, "POST", Some(&authorization), "[1, 2]").await;
        assert_eq!(not_object.status, 400);
        assert_eq!(not_object.json()["error"]["code"], -32600);

        let no_method = send(
            &address,
            "POST",
            Some(&authorization),
            r#"{"jsonrpc":"2.0","id":9}"#,
        )
        .await;
        assert_eq!(no_method.status, 400);
        assert_eq!(no_method.json()["error"]["code"], -32600);
        assert_eq!(no_method.json()["id"], 9);

        let no_name = post(
            &server,
            &json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {}}),
        )
        .await;
        assert_eq!(no_name.json()["error"]["code"], -32602);

        assert!(endpoint.calls().is_empty());
    }

    #[tokio::test]
    async fn get_is_not_allowed_and_delete_is_accepted() {
        let (_endpoint, server) = start().await;
        let authorization = format!("Bearer {}", server.token());
        let address = address(&server);
        assert_eq!(
            send(&address, "GET", Some(&authorization), "").await.status,
            405
        );
        assert_eq!(
            send(&address, "DELETE", Some(&authorization), "")
                .await
                .status,
            200
        );
    }

    #[tokio::test]
    async fn shutdown_stops_the_listener() {
        let (_endpoint, server) = start().await;
        let address = address(&server);
        assert_eq!(
            post(
                &server,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})
            )
            .await
            .status,
            200
        );
        server.shutdown().await;
        assert!(TcpStream::connect(&address).await.is_err());
    }

    #[tokio::test]
    async fn dropping_stops_the_listener() {
        let (_endpoint, server) = start().await;
        let address = address(&server);
        drop(server);
        // Abort takes effect when the runtime next polls the server task.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if TcpStream::connect(&address).await.is_err() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "listener still accepting after drop"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
