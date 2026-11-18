//! In-process transport: [`LocalDaemon`] wraps [`DaemonCore`] so the desktop
//! app exercises exactly the dispatch/event path a remote client would —
//! one code path, one set of semantics, no parallel in-process shortcut.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};

use threadlane_daemon::core::DaemonCore;
use threadlane_protocol::daemon::{
    CommandRequest, CommandResponse, SessionCommand, SessionEvent,
};

use crate::DaemonClient;

/// In-process [`DaemonClient`] over an owned [`DaemonCore`].
pub struct LocalDaemon {
    core: Arc<DaemonCore>,
}

impl LocalDaemon {
    /// Construct the in-process client around a fresh [`DaemonCore`].
    pub fn new(core: Arc<DaemonCore>) -> Arc<Self> {
        Arc::new(Self { core })
    }

    /// The owned core, for hosts that manage session-local state the wire
    /// contract does not expose (runtime status sweeps, view caches).
    pub fn core(&self) -> &Arc<DaemonCore> {
        &self.core
    }
}

#[async_trait]
impl DaemonClient for LocalDaemon {
    async fn command(&self, command: SessionCommand) -> Result<(), String> {
        self.core.clone().dispatch(command).await.map(|_| ())
    }

    async fn command_request(
        &self,
        request: CommandRequest,
    ) -> Result<CommandResponse, String> {
        // In-process the reply is just the dispatch return — no frame
        // correlation needed.
        self.core.clone().dispatch(request.command).await
    }

    fn supports_command_requests(&self) -> bool {
        // The in-process core is always this build's version.
        true
    }

    fn supports_file_search(&self) -> bool { true }

    fn supports_guarded_saves(&self) -> bool { true }

    fn supports_project_io(&self) -> bool {
        true
    }

    fn supports_editor_lsp(&self) -> bool {
        true
    }

    fn supports_github_automation(&self) -> bool {
        true
    }

    fn subscribe(&self) -> mpsc::UnboundedReceiver<SessionEvent> {
        // An in-process attach starts an empty cursor — the whole journal
        // tail replays (this client never reconnects, so dedupe by seq
        // is not needed).
        let (tail, mut broadcast_rx) = self.core.subscribe_with_tail(0);
        let (tx, rx) = mpsc::unbounded_channel();
        match threadlane_daemon::chat::executor() {
            Ok(executor) => {
                executor.spawn(async move {
                    for (_, event) in tail {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                    loop {
                        match broadcast_rx.recv().await {
                            Ok((_, event)) => {
                                if tx.send(event).is_err() {
                                    return;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                // The journal is the recovery surface: report
                                // the gap so the client can re-snapshot rather
                                // than believe a silently stale state.
                                if tx
                                    .send(SessionEvent::DaemonError {
                                        session_id: None,
                                        message: format!(
                                            "dropped {skipped} daemon events; refresh the session"
                                        ),
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => return,
                        }
                    }
                });
            }
            Err(error) => {
                let _ = tx.send(SessionEvent::DaemonError {
                    session_id: None,
                    message: format!("daemon event executor unavailable: {error}"),
                });
            }
        }
        rx
    }
}
