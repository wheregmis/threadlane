use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceChangeEvent {
    pub git_dirty: bool,
    pub files_dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeClassification {
    Ignored,
    GitOnly,
    FilesAndGit,
    GitContent,
}

fn classify(root: &Path, path: &Path, kind: &EventKind) -> ChangeClassification {
    let Ok(relative) = path.strip_prefix(root) else {
        return ChangeClassification::Ignored;
    };
    let mut in_git = false;
    for component in relative.components() {
        let name = component.as_os_str().to_string_lossy();
        if ["target", "node_modules", ".threadlane", ".DS_Store"].contains(&name.as_ref())
            || name.ends_with(".tmp")
            || name.ends_with(".swp")
            || name.starts_with(".#")
        {
            return ChangeClassification::Ignored;
        }
        if name == ".git" {
            in_git = true;
        }
    }
    if in_git {
        let path = relative.to_string_lossy();
        if [".git/objects", ".git/logs", ".git/hooks", ".git/info"]
            .iter()
            .any(|prefix| path.contains(prefix))
            || path.ends_with(".lock")
        {
            return ChangeClassification::Ignored;
        }
        if path.ends_with(".git/index")
            || path.ends_with(".git/HEAD")
            || path.contains(".git/refs/")
            || path.ends_with(".git/config")
            || path.ends_with(".git/MERGE_HEAD")
        {
            return ChangeClassification::GitOnly;
        }
        return ChangeClassification::Ignored;
    }
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => ChangeClassification::FilesAndGit,
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => ChangeClassification::FilesAndGit,
        _ => ChangeClassification::GitContent,
    }
}

pub struct WorkspaceWatcher {
    _watcher: RecommendedWatcher,
    stop_tx: Option<mpsc::Sender<()>>,
}

impl WorkspaceWatcher {
    pub fn start<F>(
        root: PathBuf,
        debounce_duration: Duration,
        on_change: F,
    ) -> Result<Self, notify::Error>
    where
        F: Fn(WorkspaceChangeEvent) + Send + 'static,
    {
        let (raw_tx, raw_rx) = mpsc::channel::<Result<Event, notify::Error>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let mut watcher = notify::recommended_watcher(move |result| {
            let _ = raw_tx.send(result);
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        let worker_root = root.clone();
        std::thread::Builder::new()
            .name("threadlane-workspace-watcher".into())
            .spawn(move || {
                let mut git_dirty = false;
                let mut files_dirty = false;
                let mut pending = false;
                let interval = Duration::from_millis(50);
                let steps = (debounce_duration.as_millis() / interval.as_millis()).max(1) as usize;
                let mut settled = 0;
                loop {
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }
                    let mut received = false;
                    while let Ok(result) = raw_rx.try_recv() {
                        received = true;
                        if let Ok(event) = result {
                            for path in &event.paths {
                                match classify(&worker_root, path, &event.kind) {
                                    ChangeClassification::Ignored => {}
                                    ChangeClassification::GitOnly
                                    | ChangeClassification::GitContent => {
                                        git_dirty = true;
                                        pending = true;
                                    }
                                    ChangeClassification::FilesAndGit => {
                                        git_dirty = true;
                                        files_dirty = true;
                                        pending = true;
                                    }
                                }
                            }
                        }
                    }
                    if received {
                        settled = 0;
                    } else if pending {
                        settled += 1;
                        if settled >= steps {
                            on_change(WorkspaceChangeEvent {
                                git_dirty,
                                files_dirty,
                            });
                            git_dirty = false;
                            files_dirty = false;
                            pending = false;
                            settled = 0;
                        }
                    }
                    std::thread::sleep(interval);
                }
            })
            .ok();
        Ok(Self {
            _watcher: watcher,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_generated_and_git_object_paths() {
        let root = Path::new("/workspace");
        let kind = EventKind::Modify(notify::event::ModifyKind::Any);
        assert_eq!(
            classify(root, Path::new("/workspace/target/out"), &kind),
            ChangeClassification::Ignored
        );
        assert_eq!(
            classify(root, Path::new("/workspace/.git/objects/abc"), &kind),
            ChangeClassification::Ignored
        );
    }
}
