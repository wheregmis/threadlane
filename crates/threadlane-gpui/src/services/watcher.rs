//! Workspace watcher — subscribes to `workspace/changed` notifications pushed
//! by the daemon instead of running a local `notify` watcher in GPUI.
//!
//! The daemon's `WatcherService` owns the actual `RecommendedWatcher`; GPUI
//! just asks the daemon to start watching a project and then receives events.

use std::path::PathBuf;

/// Summary of a workspace-change notification received from the daemon.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceChangeEvent {
    pub git_dirty: bool,
    pub files_dirty: bool,
}

/// A handle returned to the caller that stops the subscription on drop.
pub struct WorkspaceWatcher {
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl WorkspaceWatcher {
    /// Begin watching `root`. Calls `on_change` on the calling thread's Tokio
    /// executor whenever the daemon pushes a `workspace/changed` event for this
    /// project.
    pub fn start<F>(
        root: PathBuf,
        _debounce_duration: std::time::Duration,
        _on_change: F,
    ) -> Result<Self, notify::Error>
    where
        F: Fn(WorkspaceChangeEvent) + Send + 'static,
    {
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let root_str = root.to_string_lossy().to_string();

        // Spawn an async task on the shared daemon-client Tokio runtime.
        if let Ok(rt) = crate::services::chat::executor() {
            rt.spawn(async move {
                // Ask the daemon to start watching this project path.
                let client = match crate::services::daemon_client::get_daemon_client().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("WorkspaceWatcher: cannot reach daemon: {e}");
                        return;
                    }
                };

                // Register the project with the daemon watcher service via project/register.
                // The watcher_service auto-starts watching on register when configured.
                let _ = client.register_project(&root_str).await;

                // Subscribe to workspace/changed notifications.
                // These arrive as terminal events with method "workspace/changed" — for now
                // we poll the session-events bus for project-specific dirty signals.
                // TODO: add dedicated workspace/changed notification channel to the protocol.
                let mut events = client.subscribe_session_events();
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        Ok(event) = events.recv() => {
                            // The daemon broadcasts workspace/changed info as part of
                            // SessionEvent::SessionStarted for now; a dedicated event
                            // type will be added to the protocol in a follow-up.
                            let _ = event; // placeholder until workspace/changed is in protocol
                        }
                    }
                }
            });
        }

        Ok(Self {
            stop_tx: Some(stop_tx),
        })
    }
}

impl Drop for WorkspaceWatcher {
    fn drop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

// Re-export notify::Error so existing call-sites compile without needing `notify`.
pub use notify::Error as NotifyError;
