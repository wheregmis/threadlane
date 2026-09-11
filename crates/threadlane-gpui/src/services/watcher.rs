use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceChangeEvent {
    pub(crate) git_dirty: bool,
    pub(crate) files_dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeClassification {
    Ignored,
    GitOnly,
    FilesAndGit,
    GitContent,
}

/// Classifies a path change within a workspace directory.
fn classify_path_change(root: &Path, path: &Path, kind: &EventKind) -> ChangeClassification {
    let Ok(relative) = path.strip_prefix(root) else {
        return ChangeClassification::Ignored;
    };

    let mut is_in_git = false;
    let mut is_git_metadata = false;

    for component in relative.components() {
        let name = component.as_os_str().to_string_lossy();
        if name == "target"
            || name == "node_modules"
            || name == ".threadlane"
            || name == ".DS_Store"
        {
            return ChangeClassification::Ignored;
        }
        if name.ends_with(".tmp") || name.ends_with(".swp") || name.starts_with(".#") {
            return ChangeClassification::Ignored;
        }
        if name == ".git" {
            is_in_git = true;
        }
    }

    if is_in_git {
        let path_str = relative.to_string_lossy();
        if path_str.contains(".git/objects")
            || path_str.contains(".git/logs")
            || path_str.contains(".git/hooks")
            || path_str.contains(".git/info")
            || path_str.ends_with(".lock")
        {
            return ChangeClassification::Ignored;
        }

        if path_str.ends_with(".git/index")
            || path_str.ends_with(".git/HEAD")
            || path_str.contains(".git/refs/")
            || path_str.ends_with(".git/config")
            || path_str.ends_with(".git/MERGE_HEAD")
        {
            is_git_metadata = true;
        }

        if is_git_metadata {
            return ChangeClassification::GitOnly;
        }
        return ChangeClassification::Ignored;
    }

    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => ChangeClassification::FilesAndGit,
        EventKind::Modify(modify_kind) => match modify_kind {
            notify::event::ModifyKind::Name(_) => ChangeClassification::FilesAndGit,
            _ => ChangeClassification::GitContent,
        },
        _ => ChangeClassification::GitContent,
    }
}

pub struct WorkspaceWatcher {
    _watcher: RecommendedWatcher,
    stop_tx: Option<mpsc::Sender<()>>,
}

impl WorkspaceWatcher {
    pub(crate) fn start<F>(
        root: PathBuf,
        debounce_duration: Duration,
        on_change: F,
    ) -> Result<Self, notify::Error>
    where
        F: Fn(WorkspaceChangeEvent) + Send + 'static,
    {
        let (raw_tx, raw_rx) = mpsc::channel::<Result<Event, notify::Error>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = raw_tx.send(res);
        })?;

        watcher.watch(&root, RecursiveMode::Recursive)?;

        let worker_root = root.clone();
        std::thread::Builder::new()
            .name("threadlane-workspace-watcher".to_string())
            .spawn(move || {
                let mut git_dirty = false;
                let mut files_dirty = false;
                let mut has_pending = false;
                let poll_interval = Duration::from_millis(50);
                let debounce_steps =
                    (debounce_duration.as_millis() / poll_interval.as_millis()).max(1) as usize;
                let mut settle_counter = 0;

                loop {
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }

                    let mut received_any = false;
                    while let Ok(event_res) = raw_rx.try_recv() {
                        received_any = true;
                        if let Ok(event) = event_res {
                            for path in &event.paths {
                                match classify_path_change(&worker_root, path, &event.kind) {
                                    ChangeClassification::Ignored => {}
                                    ChangeClassification::GitOnly => {
                                        git_dirty = true;
                                        has_pending = true;
                                    }
                                    ChangeClassification::GitContent => {
                                        git_dirty = true;
                                        has_pending = true;
                                    }
                                    ChangeClassification::FilesAndGit => {
                                        git_dirty = true;
                                        files_dirty = true;
                                        has_pending = true;
                                    }
                                }
                            }
                        }
                    }

                    if received_any {
                        settle_counter = 0;
                    } else if has_pending {
                        settle_counter += 1;
                        if settle_counter >= debounce_steps {
                            on_change(WorkspaceChangeEvent {
                                git_dirty,
                                files_dirty,
                            });
                            git_dirty = false;
                            files_dirty = false;
                            has_pending = false;
                            settle_counter = 0;
                        }
                    }

                    std::thread::sleep(poll_interval);
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
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::CreateKind;

    #[test]
    fn test_classify_git_metadata() {
        let root = Path::new("/workspace");
        let index_path = Path::new("/workspace/.git/index");
        let kind = EventKind::Modify(notify::event::ModifyKind::Any);
        assert_eq!(
            classify_path_change(root, index_path, &kind),
            ChangeClassification::GitOnly
        );

        let head_path = Path::new("/workspace/.git/HEAD");
        assert_eq!(
            classify_path_change(root, head_path, &kind),
            ChangeClassification::GitOnly
        );

        let ref_path = Path::new("/workspace/.git/refs/heads/feature");
        assert_eq!(
            classify_path_change(root, ref_path, &kind),
            ChangeClassification::GitOnly
        );
    }

    #[test]
    fn test_classify_fetch_head_ignored() {
        let root = Path::new("/workspace");
        let fetch_head = Path::new("/workspace/.git/FETCH_HEAD");
        let kind = EventKind::Modify(notify::event::ModifyKind::Any);

        assert_eq!(
            classify_path_change(root, fetch_head, &kind),
            ChangeClassification::Ignored
        );
    }

    #[test]
    fn test_classify_git_objects_ignored() {
        let root = Path::new("/workspace");
        let obj_path = Path::new("/workspace/.git/objects/3f/123456");
        let kind = EventKind::Create(CreateKind::File);
        assert_eq!(
            classify_path_change(root, obj_path, &kind),
            ChangeClassification::Ignored
        );
    }

    #[test]
    fn test_classify_ignored_directories() {
        let root = Path::new("/workspace");
        let target_path = Path::new("/workspace/target/debug/build/foo.o");
        let node_modules = Path::new("/workspace/node_modules/pkg/index.js");
        let threadlane = Path::new("/workspace/.threadlane/skills.json");
        let ds_store = Path::new("/workspace/.DS_Store");
        let kind = EventKind::Modify(notify::event::ModifyKind::Any);

        assert_eq!(
            classify_path_change(root, target_path, &kind),
            ChangeClassification::Ignored
        );
        assert_eq!(
            classify_path_change(root, node_modules, &kind),
            ChangeClassification::Ignored
        );
        assert_eq!(
            classify_path_change(root, threadlane, &kind),
            ChangeClassification::Ignored
        );
        assert_eq!(
            classify_path_change(root, ds_store, &kind),
            ChangeClassification::Ignored
        );
    }

    #[test]
    fn test_classify_source_files() {
        let root = Path::new("/workspace");
        let src_file = Path::new("/workspace/src/lib.rs");

        let mod_kind = EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        ));
        assert_eq!(
            classify_path_change(root, src_file, &mod_kind),
            ChangeClassification::GitContent
        );

        let create_kind = EventKind::Create(CreateKind::File);
        assert_eq!(
            classify_path_change(root, src_file, &create_kind),
            ChangeClassification::FilesAndGit
        );

        let remove_kind = EventKind::Remove(notify::event::RemoveKind::File);
        assert_eq!(
            classify_path_change(root, src_file, &remove_kind),
            ChangeClassification::FilesAndGit
        );
    }
}
