//! Agent tools for the embedded browser panel: the session↔UI contract.
//!
//! The browser view lives on the GPUI UI thread, which agent tool execution
//! (tokio workers) cannot touch directly. This module owns the contract:
//! [`BrowserBridge`] is a cloneable, thread-safe handle the host frontend
//! constructs and hands to the session; tool calls become [`BrowserCommand`]
//! round-trips over an mpsc channel, and a pump spawned by the owning panel
//! applies them to the live view and replies through a oneshot.
//!
//! Moved verbatim from `threadlane-session::browser` (bridge, commands, and
//! tool-name constants). The tool *executor* (`BrowserToolExecutor`, schema
//! definitions, argument parsing) stays in `threadlane-session`, which owns
//! the runtime `ToolExecutor` trait. `threadlane-session` re-exports this
//! module as `browser` for compatibility; new code should import
//! `threadlane_protocol::browser` directly.
//!
//! With no bridge attached (headless runs, tests, non-macOS), every tool
//! reports unavailability instead of failing opaquely.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub const BROWSER_NAVIGATE_TOOL: &str = "browser_navigate";
pub const BROWSER_BACK_TOOL: &str = "browser_back";
pub const BROWSER_RELOAD_TOOL: &str = "browser_reload";
pub const BROWSER_CURRENT_URL_TOOL: &str = "browser_current_url";
pub const BROWSER_SNAPSHOT_TOOL: &str = "browser_snapshot";
pub const BROWSER_ACT_TOOL: &str = "browser_act";
pub const BROWSER_EVALUATE_TOOL: &str = "browser_evaluate_script";
pub const BROWSER_SCREENSHOT_TOOL: &str = "browser_screenshot";
pub const BROWSER_CONSOLE_LOGS_TOOL: &str = "browser_console_logs";
pub const BROWSER_WAIT_TOOL: &str = "browser_wait";

const BROWSER_ROUND_TRIP_TIMEOUT: Duration = Duration::from_secs(15);

pub const BROWSER_UNAVAILABLE: &str = "The embedded browser is unavailable (macOS desktop app with the Browser panel only). Tell the user what you would have opened instead.";

/// A single intent for the panel's browser view.
#[derive(Debug)]
pub enum BrowserCommand {
    Navigate {
        url: String,
    },
    Back,
    Reload,
    CurrentUrl,
    Snapshot,
    Act {
        action: String,
        target: ActTarget,
        text: Option<String>,
        key: Option<String>,
    },
    Evaluate {
        script: String,
    },
    Screenshot,
    ConsoleLogs {
        clear: bool,
        level: String,
    },
    Wait {
        selector: Option<String>,
        text: Option<String>,
        timeout_ms: u64,
    },
}

/// Addressable element for [`BrowserCommand::Act`]: a `browser_snapshot` ref
/// or a CSS selector. Exactly one must be set.
#[derive(Debug)]
pub enum ActTarget {
    Ref(u32),
    Selector(String),
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
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| BROWSER_UNAVAILABLE.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_bridge_reports_helpfully() {
        let bridge = BrowserBridge::unavailable();
        let error = bridge
            .round_trip(BrowserCommand::CurrentUrl)
            .await
            .expect_err("no channel");
        assert!(error.contains("unavailable"));
    }

    #[tokio::test]
    async fn round_trip_delivers_command_and_reply() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bridge = BrowserBridge::new(tx);
        let task = tokio::spawn(async move { bridge.round_trip(BrowserCommand::Back).await });
        let request = rx.recv().await.expect("command sent");
        assert!(matches!(request.command, BrowserCommand::Back));
        request
            .reply
            .send(Ok("went back".into()))
            .expect("reply");
        assert_eq!(task.await.expect("task").expect("ok"), "went back");
    }

    #[test]
    fn first_receiver_takes_the_channel() {
        let bridge = BrowserBridge::channel();
        assert!(bridge.take_receiver().is_some());
        assert!(bridge.take_receiver().is_none());
    }
}
