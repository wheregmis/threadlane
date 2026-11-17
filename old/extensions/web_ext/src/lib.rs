use serde::{Deserialize, Serialize};

const SEARCH_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const MAX_OUTPUT_CHARS: usize = 48_000;

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

#[derive(Debug, Deserialize)]
struct HttpResponse {
    url: String,
    status: u16,
    #[serde(default)]
    content_type: String,
    #[serde(default)]
    location: Option<String>,
    body: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WebState {
    #[serde(default)]
    phase: String,
    #[serde(default)]
    tool: String,
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
    state: serde_json::Value,
}

impl Response {
    fn pending(tool: &str) -> Self {
        Self {
            message: format!("Requesting web content for `{tool}`."),
            error: None,
            continue_after_broker: true,
            state: serde_json::to_value(WebState {
                phase: "waiting_http".into(),
                tool: tool.into(),
            })
            .expect("web state serializes"),
        }
    }

    fn ok(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error: None,
            continue_after_broker: false,
            state: serde_json::to_value(WebState::default()).expect("web state serializes"),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            message: message.clone(),
            error: Some(message),
            continue_after_broker: false,
            state: serde_json::to_value(WebState::default()).expect("web state serializes"),
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn extension_manifest() -> WasiExtensionManifest {
    WasiExtensionManifest {
        api_version: 2,
        name: "web_ext".into(),
        version: "0.1.0".into(),
        description: "Permission-gated web page retrieval and DuckDuckGo search".into(),
        capabilities: vec!["network".into()],
        tools: vec![
            WasiToolDefinition {
                name: "fetch".into(),
                description: "Fetch an HTTP or HTTPS URL and return readable text. New hosts require user approval.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "Absolute http:// or https:// URL to retrieve."
                        }
                    },
                    "required": ["url"]
                }),
            },
            WasiToolDefinition {
                name: "web_search".into(),
                description: "Search the public web through DuckDuckGo and return result titles, snippets, and links. Access to html.duckduckgo.com requires approval.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Concise web search query."
                        }
                    },
                    "required": ["query"]
                }),
            },
        ],
        commands: Vec::new(),
        hooks: Vec::new(),
    }
}

fn network_request(url: String) -> BrokerRequest {
    BrokerRequest {
        api_version: 2,
        capability: "network".into(),
        operation: "http".into(),
        arguments: serde_json::json!({
            "url": url,
            "method": "GET",
            "body": "",
        }),
    }
}

fn is_continuation(invocation: &Invocation) -> bool {
    invocation
        .events
        .iter()
        .any(|event| event.topic == "broker_response")
}

fn begin(invocation: &Invocation) -> Result<(Response, BrokerRequest), Response> {
    match invocation.name.as_str() {
        "fetch" => {
            let url = required_argument(&invocation.arguments, "url")?;
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(Response::error("`url` must start with http:// or https://"));
            }
            Ok((Response::pending("fetch"), network_request(url.to_owned())))
        }
        "web_search" => {
            let query = required_argument(&invocation.arguments, "query")?;
            let query = query.trim();
            if query.is_empty() {
                return Err(Response::error("`query` must not be empty"));
            }
            let url = format!("{SEARCH_ENDPOINT}?q={}", percent_encode(query));
            Ok((Response::pending("web_search"), network_request(url)))
        }
        unknown => Err(Response::error(format!("Unknown tool `{unknown}`"))),
    }
}

fn required_argument<'a>(
    arguments: &'a serde_json::Value,
    name: &str,
) -> Result<&'a str, Response> {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Response::error(format!("Missing required string argument `{name}`")))
}

fn finish(invocation: &Invocation) -> Response {
    let state: WebState = serde_json::from_value(invocation.state.clone()).unwrap_or_default();
    if state.phase != "waiting_http" {
        return Response::error("Web continuation has no pending HTTP request.");
    }
    let response = match incoming_http(invocation) {
        Ok(response) => response,
        Err(error) => return Response::error(error),
    };
    if (300..400).contains(&response.status) {
        return Response::error(match response.location {
            Some(location) => format!(
                "{} returned HTTP {} and redirects to {location}. Allow that host and fetch the redirect URL explicitly.",
                response.url, response.status
            ),
            None => format!("{} returned HTTP {} without a redirect location.", response.url, response.status),
        });
    }
    if !(200..300).contains(&response.status) {
        return Response::error(format!(
            "{} returned HTTP {}.",
            response.url, response.status
        ));
    }

    let readable = if response.content_type.contains("html") || looks_like_html(&response.body) {
        html_to_text(&response.body)
    } else {
        response.body
    };
    let readable = truncate_chars(readable.trim(), MAX_OUTPUT_CHARS);
    if state.tool == "web_search" {
        Response::ok(format!("Search results for DuckDuckGo\n\n{readable}"))
    } else {
        Response::ok(format!("Source: {}\n\n{readable}", response.url))
    }
}

fn incoming_http(invocation: &Invocation) -> Result<HttpResponse, String> {
    let event = invocation
        .events
        .iter()
        .find(|event| event.topic == "broker_response")
        .ok_or_else(|| "Missing network broker response.".to_string())?;
    let payload: BrokerResponsePayload = serde_json::from_value(event.payload.clone())
        .map_err(|_| "Malformed network broker response.".to_string())?;
    if payload.capability != "network" || payload.operation != "http" {
        return Err("Unexpected broker response while waiting for network/http.".into());
    }
    if !payload.ok {
        return Err(payload.error.map_or_else(
            || "network/http failed".into(),
            |error| format!("network/http error {}: {}", error.code, error.message),
        ));
    }
    let message = payload
        .value
        .ok_or_else(|| "Network broker response is missing a value.".to_string())?
        .message;
    let message = message
        .as_str()
        .ok_or_else(|| "Network broker response message is not a string.".to_string())?;
    serde_json::from_str(message).map_err(|_| "Network broker returned malformed HTTP data.".into())
}

fn looks_like_html(body: &str) -> bool {
    let prefix = body
        .trim_start()
        .chars()
        .take(128)
        .collect::<String>()
        .to_ascii_lowercase();
    prefix.starts_with("<!doctype html") || prefix.starts_with("<html")
}

fn html_to_text(html: &str) -> String {
    let mut output = String::with_capacity(html.len().min(MAX_OUTPUT_CHARS));
    let mut tag = String::new();
    let mut in_tag = false;
    let mut suppress_depth = 0_u32;
    let mut pending_space = false;
    let mut active_link: Option<String> = None;

    for ch in html.chars() {
        if in_tag {
            if ch == '>' {
                let normalized = tag.trim().to_ascii_lowercase();
                let closing = normalized.starts_with('/');
                let name = normalized
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches('/');
                if matches!(name, "script" | "style" | "svg") {
                    if closing {
                        suppress_depth = suppress_depth.saturating_sub(1);
                    } else if !normalized.ends_with('/') {
                        suppress_depth = suppress_depth.saturating_add(1);
                    }
                }
                if name == "a" {
                    if closing {
                        if let Some(link) = active_link.take() {
                            output.push_str(" (");
                            output.push_str(&decode_entities(&link));
                            output.push(')');
                        }
                    } else {
                        active_link = attribute_value(&tag, "href");
                    }
                }
                if suppress_depth == 0
                    && matches!(
                        name,
                        "p" | "div" | "br" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "pre"
                    )
                {
                    push_newline(&mut output);
                }
                tag.clear();
                in_tag = false;
            } else {
                tag.push(ch);
            }
            continue;
        }
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if suppress_depth > 0 {
            continue;
        }
        if ch.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !output.ends_with([' ', '\n']) {
                output.push(' ');
            }
            pending_space = false;
            output.push(ch);
        }
    }

    decode_entities(&output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn attribute_value(tag: &str, attribute: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let marker = format!("{attribute}=");
    let start = lower.find(&marker)? + marker.len();
    let rest = tag.get(start..)?.trim_start();
    let quote = rest.chars().next()?;
    if matches!(quote, '\'' | '"') {
        let value = &rest[quote.len_utf8()..];
        let end = value.find(quote)?;
        return Some(value[..end].to_owned());
    }
    let end = rest
        .find(|character: char| character.is_whitespace() || character == '>')
        .unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}

fn push_newline(output: &mut String) {
    while output.ends_with(' ') {
        output.pop();
    }
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
}

fn decode_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

fn percent_encode(input: &str) -> String {
    let mut output = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_owned();
    }
    let mut output = input.chars().take(max_chars).collect::<String>();
    output.push_str("\n\n[content truncated]");
    output
}

fn handle_invocation(invocation: &Invocation) -> Response {
    if is_continuation(invocation) {
        return finish(invocation);
    }
    match begin(invocation) {
        Ok((response, request)) => {
            send_broker_request(&request);
            response
        }
        Err(response) => response,
    }
}

fn send_broker_request(request: &BrokerRequest) {
    #[cfg(target_arch = "wasm32")]
    {
        let request = serde_json::to_vec(request).expect("broker request serializes");
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

static mut OUTPUT_PTR: *mut u8 = std::ptr::null_mut();
static mut OUTPUT_LEN: usize = 0;
static mut OUTPUT_CAPACITY: usize = 0;

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    let mut buffer = vec![0_u8; size as usize];
    let pointer = buffer.as_mut_ptr() as i32;
    std::mem::forget(buffer);
    pointer
}

#[no_mangle]
pub extern "C" fn extension_info() -> u64 {
    write_output(&extension_manifest())
}

#[no_mangle]
pub extern "C" fn execute_tool(pointer: i32, length: i32) -> u64 {
    let input = unsafe { std::slice::from_raw_parts(pointer as *const u8, length as usize) };
    let response = serde_json::from_slice::<Invocation>(input)
        .map(|invocation| handle_invocation(&invocation))
        .unwrap_or_else(|error| Response::error(format!("Invalid invocation: {error}")));
    write_output(&response)
}

#[no_mangle]
pub extern "C" fn execute_command(pointer: i32, length: i32) -> u64 {
    execute_tool(pointer, length)
}

fn write_output<T: Serialize>(value: &T) -> u64 {
    let mut bytes = serde_json::to_vec(value).expect("extension output serializes");
    let length = bytes.len();
    let capacity = bytes.capacity();
    let pointer = bytes.as_mut_ptr();
    unsafe {
        if !OUTPUT_PTR.is_null() {
            drop(Vec::from_raw_parts(OUTPUT_PTR, OUTPUT_LEN, OUTPUT_CAPACITY));
        }
        OUTPUT_PTR = pointer;
        OUTPUT_LEN = length;
        OUTPUT_CAPACITY = capacity;
    }
    std::mem::forget(bytes);
    ((pointer as u64) << 32) | (length as u64 & 0xffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_exposes_only_network_capability() {
        let manifest = extension_manifest();
        assert_eq!(manifest.capabilities, vec!["network"]);
        assert_eq!(
            manifest
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["fetch", "web_search"]
        );
    }

    #[test]
    fn search_encodes_query_and_requests_duckduckgo() {
        let invocation = Invocation {
            name: "web_search".into(),
            arguments: serde_json::json!({"query": "rust async/await"}),
            state: serde_json::Value::Null,
            events: Vec::new(),
        };
        let (_, request) = begin(&invocation).expect("search starts");
        assert_eq!(request.capability, "network");
        assert_eq!(
            request.arguments["url"],
            "https://html.duckduckgo.com/html/?q=rust%20async%2Fawait"
        );
    }

    #[test]
    fn html_conversion_removes_scripts_and_preserves_sections() {
        let html = "<h1>Title &amp; More</h1><script>bad()</script><p>Hello <a href=\"https://example.com?a=1&amp;b=2\">world</a>.</p>";
        assert_eq!(
            html_to_text(html),
            "Title & More\nHello world (https://example.com?a=1&b=2)."
        );
    }

    #[test]
    fn continuation_reports_denied_host() {
        let invocation = Invocation {
            name: "fetch".into(),
            arguments: serde_json::json!({"url": "https://example.com"}),
            state: serde_json::to_value(WebState {
                phase: "waiting_http".into(),
                tool: "fetch".into(),
            })
            .unwrap(),
            events: vec![ExtensionEvent {
                topic: "broker_response".into(),
                payload: serde_json::json!({
                    "capability": "network",
                    "operation": "http",
                    "ok": false,
                    "error": {"code": "host_denied", "message": "Network host `example.com` is not allowed"}
                }),
            }],
        };
        let response = handle_invocation(&invocation);
        assert!(response.error.unwrap().contains("host_denied"));
        assert_eq!(
            response.state,
            serde_json::to_value(WebState::default()).unwrap()
        );
    }
}
