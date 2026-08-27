//! Workspace watcher service: uses the `notify` crate to watch registered
//! project directories and push `workspace/changed` notifications to connected
//! GPUI clients via the connection's notification sender.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tracing::info;

/// Event pushed as a `workspace/changed` notification over the RPC connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceChangedEvent {
    pub project_path: String,
    pub git_dirty: bool,
    pub files_dirty: bool,
}

type NotifySender = tokio::sync::broadcast::Sender<WorkspaceChangedEvent>;

/// Classify a file-system event path relative to a project root.
fn is_relevant(root: &std::path::Path, path: &std::path::Path) -> (bool, bool) {
    let Ok(rel) = path.strip_prefix(root) else {
        return (false, false);
    };
    let mut git_dirty = false;
    let mut files_dirty = false;
    let mut in_git = false;

    for component in rel.components() {
        let name = component.as_os_str().to_string_lossy();
        if matches!(
            name.as_ref(),
            "target" | "node_modules" | ".threadlane" | ".DS_Store"
        ) || name.ends_with(".tmp")
            || name.ends_with(".swp")
            || name.starts_with(".#")
        {
            return (false, false);
        }
        if name == ".git" {
            in_git = true;
        }
    }

    if in_git {
        let rel_str = rel.to_string_lossy();
        if rel_str.contains(".git/objects")
            || rel_str.contains(".git/logs")
            || rel_str.contains(".git/hooks")
            || rel_str.contains(".git/info")
            || rel_str.ends_with(".lock")
        {
            return (false, false);
        }
        if rel_str.ends_with(".git/index")
            || rel_str.ends_with(".git/HEAD")
            || rel_str.contains(".git/refs/")
            || rel_str.ends_with(".git/config")
        {
            git_dirty = true;
        }
    } else {
        git_dirty = true;
        files_dirty = true;
    }
    (git_dirty, files_dirty)
}

struct WatchedProject {
    _watcher: RecommendedWatcher,
}

#[derive(Clone, Default)]
pub struct WatcherService {
    /// project_path → active watcher
    projects: Arc<Mutex<HashMap<String, WatchedProject>>>,
}

impl WatcherService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start watching `project_path` and broadcasting `WorkspaceChangedEvent`
    /// on `event_tx`. Replaces any existing watcher for this path.
    pub fn watch_project(
        &self,
        project_path: String,
        event_tx: NotifySender,
    ) -> Result<(), String> {
        let root = PathBuf::from(&project_path);
        let root_clone = root.clone();
        let path_clone = project_path.clone();

        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<Result<Event, notify::Error>>();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = raw_tx.send(res);
        })
        .map_err(|e| format!("Failed to create watcher: {e}"))?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| format!("Failed to watch {}: {e}", root.display()))?;

        // Debounce thread
        std::thread::Builder::new()
            .name(format!("watcher-{project_path}"))
            .spawn(move || {
                let debounce = Duration::from_millis(300);
                let poll = Duration::from_millis(50);
                let steps = (debounce.as_millis() / poll.as_millis()).max(1) as usize;

                let mut git_dirty = false;
                let mut files_dirty = false;
                let mut has_pending = false;
                let mut settle = 0usize;

                loop {
                    let mut received = false;
                    while let Ok(event_res) = raw_rx.try_recv() {
                        received = true;
                        if let Ok(event) = event_res {
                            for p in &event.paths {
                                let (g, f) = is_relevant(&root_clone, p);
                                if g {
                                    git_dirty = true;
                                    has_pending = true;
                                }
                                if f {
                                    files_dirty = true;
                                }
                            }
                        }
                    }
                    if received {
                        settle = 0;
                    } else if has_pending {
                        settle += 1;
                        if settle >= steps {
                            let _ = event_tx.send(WorkspaceChangedEvent {
                                project_path: path_clone.clone(),
                                git_dirty,
                                files_dirty,
                            });
                            git_dirty = false;
                            files_dirty = false;
                            has_pending = false;
                            settle = 0;
                        }
                    }
                    std::thread::sleep(poll);
                }
            })
            .map_err(|e| format!("Failed to spawn watcher thread: {e}"))?;

        let mut lock = self.projects.lock().unwrap();
        lock.insert(project_path, WatchedProject { _watcher: watcher });
        info!("Watching project: {}", root.display());
        Ok(())
    }

    pub fn unwatch_project(&self, project_path: &str) {
        let mut lock = self.projects.lock().unwrap();
        if lock.remove(project_path).is_some() {
            info!("Stopped watching project: {project_path}");
        }
    }
}
