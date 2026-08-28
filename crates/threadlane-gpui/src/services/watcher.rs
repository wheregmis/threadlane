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
        on_change: F,
    ) -> Result<Self, String>
    where
        F: Fn(WorkspaceChangeEvent) + Send + 'static,
    {
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let root_str = root.to_string_lossy().to_string();
        let watch_path = root_str.clone();
        let callback_path = root_str.clone();

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

                let mut events = client.subscribe_workspace_events();
                if let Err(error) = client.watch_workspace(&watch_path).await {
                    tracing::warn!("WorkspaceWatcher: cannot watch {watch_path}: {error}");
                    return;
                }
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => {
                            let _ = client.unwatch_workspace(&watch_path).await;
                            break;
                        }
                        Ok(event) = events.recv() => {
                            if event.project_path == callback_path {
                                on_change(WorkspaceChangeEvent {
                                    git_dirty: event.git_dirty,
                                    files_dirty: event.files_dirty,
                                });
                            }
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
