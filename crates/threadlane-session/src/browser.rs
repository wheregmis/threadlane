//! Agent tools for the embedded browser panel.
//!
//! The browser view lives on the GPUI UI thread, which agent tool execution
//! (tokio workers) cannot touch directly. This module owns the contract:
//! [`BrowserBridge`] is a cloneable, thread-safe handle the host frontend
//! constructs and passes via [`CodingAgentOptions`](crate::CodingAgentOptions).
//! Tool calls become [`BrowserCommand`] round-trips over an mpsc channel; a
//! pump spawned by the owning panel applies them to the live view and
//! replies through a oneshot.
//!
//! With no bridge attached (headless runs, tests, non-macOS), every tool
//! reports unavailability instead of failing opaquely.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use threadlane_runtime::{AgentToolDefinition, ToolExecutor};
use tokio::sync::{mpsc, oneshot};

pub const BROWSER_NAVIGATE_TOOL: &str = "browser_navigate";
pub const BROWSER_BACK_TOOL: &str = "browser_back";
pub const BROWSER_RELOAD_TOOL: &str = "browser_reload";
pub const BROWSER_CURRENT_URL_TOOL: &str = "browser_current_url";

const BROWSER_ROUND_TRIP_TIMEOUT: Duration = Duration::from_secs(15);

pub const BROWSER_UNAVAILABLE: &str = "The embedded browser is unavailable (macOS desktop app with the Browser panel only). Tell the user what you would have opened instead.";

/// A single intent for the panel's browser view.
#[derive(Debug)]
pub enum BrowserCommand {
    Navigate { url: String },
    Back,
    Reload,
    CurrentUrl,
}

/// A command plus its reply channel, sent from a tool worker to the UI pump.
pub struct BrowserRequest {
    pub command: BrowserCommand,
    pub reply: oneshot::Sender<Result<String, String>>,
}

/// Thread-safe handle to the panel browser. Clone into every session that
/// should expose browser tools; leave `None` for headless contexts.
///
/// The receiver slot is shared across clones: the first panel pump to call
/// [`BrowserBridge::take_receiver`] owns the command stream, so in
/// multi-window setups the first workspace's Browser tab serves all sessions.
#[derive(Clone, Default)]
pub struct BrowserBridge {
    tx: Option<mpsc::UnboundedSender<BrowserRequest>>,
    rx: Arc<std::sync::Mutex<Option<mpsc::UnboundedReceiver<BrowserRequest>>>>,
}

impl BrowserBridge {
    pub fn unavailable() -> Self {
        Self {
            tx: None,
            rx: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn channel() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx: Some(tx),
            rx: Arc::new(std::sync::Mutex::new(Some(rx))),
        }
    }

    pub fn new(tx: mpsc::UnboundedSender<BrowserRequest>) -> Self {
        Self {
            tx: Some(tx),
            rx: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn is_available(&self) -> bool {
        self.tx.is_some()
    }

    /// Take the command receiver. Only the first caller wins; the panel pump
    /// owns it for the life of the app.
    pub fn take_receiver(&self) -> Option<mpsc::UnboundedReceiver<BrowserRequest>> {
        self.rx.lock().ok()?.take()
    }

    pub async fn round_trip(&self, command: BrowserCommand) -> Result<String, String> {
        let tx = self.tx.as_ref().ok_or_else(|| BROWSER_UNAVAILABLE.to_string())?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(BrowserRequest {
            command,
            reply: reply_tx,
        })
        .map_err(|_| "The browser panel closed its command channel.".to_string())?;
        tokio::time::timeout(BROWSER_ROUND_TRIP_TIMEOUT, reply_rx)
            .await
            .map_err(|_| "Timed out waiting for the browser panel.".to_string())?
            .map_err(|_| "The browser panel dropped the request.".to_string())?
    }
}

pub struct BrowserToolExecutor {
    bridge: BrowserBridge,
}

impl BrowserToolExecutor {
    pub fn new(bridge: BrowserBridge) -> Self {
        Self { bridge }
    }
}

fn browser_tool_definitions() -> Arc<[AgentToolDefinition]> {
    vec![
        AgentToolDefinition::new(
            BROWSER_NAVIGATE_TOOL,
            "Navigate the embedded browser panel to a URL or search query (host-like text gets https://, plain phrases become a web search, same as the address bar). The page stays visible to the user in the Browser tab. Use this to look at docs, search the web, or open the user's local dev server.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL, host (example.com, localhost:3000), or search phrase to open."
                    }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            BROWSER_BACK_TOOL,
            "Go back one entry in the embedded browser panel history.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            BROWSER_RELOAD_TOOL,
            "Reload the current page in the embedded browser panel.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        AgentToolDefinition::new(
            BROWSER_CURRENT_URL_TOOL,
            "Report the embedded browser panel's current URL (empty when nothing has been opened yet).",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
    ]
    .into()
}

#[async_trait]
impl ToolExecutor for BrowserToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.host.browser"
    }

    fn tool_definitions(&self) -> Arc<[AgentToolDefinition]> {
        browser_tool_definitions()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        let command = match name {
            BROWSER_NAVIGATE_TOOL => {
                let parsed: serde_json::Value = serde_json::from_str(args).map_err(|error| {
                    format!("Invalid {BROWSER_NAVIGATE_TOOL} arguments: {error}")
                }).ok()?;
                match parsed.get("url").and_then(|value| value.as_str()) {
                    Some(url) if !url.trim().is_empty() => BrowserCommand::Navigate {
                        url: url.trim().to_string(),
                    },
                    _ => return Some(Err("`browser_navigate` requires a non-empty `url`.".into())),
                }
            }
            BROWSER_BACK_TOOL => BrowserCommand::Back,
            BROWSER_RELOAD_TOOL => BrowserCommand::Reload,
            BROWSER_CURRENT_URL_TOOL => BrowserCommand::CurrentUrl,
            _ => return None,
        };
        Some(self.bridge.round_trip(command).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_bridge_reports_helpfully() {
        let executor = BrowserToolExecutor::new(BrowserBridge::unavailable());
        let result = executor
            .execute_tool(BROWSER_CURRENT_URL_TOOL, "{}")
            .await
            .expect("handled");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unavailable"));
    }

    #[tokio::test]
    async fn navigate_rejects_empty_url() {
        let executor = BrowserToolExecutor::new(BrowserBridge::unavailable());
        let result = executor
            .execute_tool(BROWSER_NAVIGATE_TOOL, r#"{"url": "  "}"#)
            .await
            .expect("handled");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unknown_tool_is_not_claimed() {
        let executor = BrowserToolExecutor::new(BrowserBridge::unavailable());
        assert!(
            executor
                .execute_tool("read_file", "{}")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn round_trip_delivers_command_and_reply() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bridge = BrowserBridge::new(tx);
        let executor = BrowserToolExecutor::new(bridge);
        let task = tokio::spawn(async move {
            executor
                .execute_tool(BROWSER_NAVIGATE_TOOL, r#"{"url": "example.com"}"#)
                .await
        });
        let request = rx.recv().await.expect("command sent");
        match request.command {
            BrowserCommand::Navigate { url } => assert_eq!(url, "example.com"),
            other => panic!("unexpected command: {other:?}"),
        }
        request.reply.send(Ok("Opened https://example.com".into())).expect("reply");
        let result = task.await.expect("task").expect("handled");
        assert_eq!(result.unwrap(), "Opened https://example.com");
    }

    #[test]
    fn definitions_cover_all_four_tools() {
        let definitions = browser_tool_definitions();
        let names: Vec<_> = definitions
            .iter()
            .map(|def| def.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                BROWSER_NAVIGATE_TOOL,
                BROWSER_BACK_TOOL,
                BROWSER_RELOAD_TOOL,
                BROWSER_CURRENT_URL_TOOL
            ]
        );
    }
}
