use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
    if matches!(kind, EventKind::Access(_)) {
        return ChangeClassification::Ignored;
    }
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

fn classify_event(root: &Path, event: &Event) -> Option<WorkspaceChangeEvent> {
    let mut change = WorkspaceChangeEvent::default();
    for path in &event.paths {
        match classify(root, path, &event.kind) {
            ChangeClassification::Ignored => {}
            ChangeClassification::GitOnly | ChangeClassification::GitContent => {
                change.git_dirty = true;
            }
            ChangeClassification::FilesAndGit => {
                change.git_dirty = true;
                change.files_dirty = true;
            }
        }
    }
    (change.git_dirty || change.files_dirty).then_some(change)
}

#[derive(Default)]
struct PendingChange {
    change: WorkspaceChangeEvent,
    deadline: Option<Instant>,
}

impl PendingChange {
    fn record(&mut self, change: WorkspaceChangeEvent, now: Instant, debounce_duration: Duration) {
        self.change.git_dirty |= change.git_dirty;
        self.change.files_dirty |= change.files_dirty;
        self.deadline = Some(now + debounce_duration);
    }

    fn remaining(&self, now: Instant) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    fn take_if_settled(&mut self, now: Instant) -> Option<WorkspaceChangeEvent> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            Some(std::mem::take(&mut self.change))
        } else {
            None
        }
    }
}

enum WorkerMessage {
    Change(WorkspaceChangeEvent),
    Stop,
}

fn run_worker<F>(receiver: mpsc::Receiver<WorkerMessage>, debounce_duration: Duration, on_change: F)
where
    F: Fn(WorkspaceChangeEvent),
{
    let mut pending = PendingChange::default();
    loop {
        let message = match pending.remaining(Instant::now()) {
            Some(timeout) => match receiver.recv_timeout(timeout) {
                Ok(message) => message,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(change) = pending.take_if_settled(Instant::now()) {
                        on_change(change);
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match receiver.recv() {
                Ok(message) => message,
                Err(_) => break,
            },
        };
        match message {
            WorkerMessage::Change(change) => {
                pending.record(change, Instant::now(), debounce_duration)
            }
            WorkerMessage::Stop => break,
        }
    }
}

pub struct WorkspaceWatcher {
    _watcher: RecommendedWatcher,
    stop_tx: Option<mpsc::Sender<WorkerMessage>>,
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
        let (worker_tx, worker_rx) = mpsc::channel::<WorkerMessage>();
        let event_root = root.clone();
        let event_tx = worker_tx.clone();
        let mut watcher = notify::recommended_watcher(move |result| {
            let Ok(event) = result else {
                return;
            };
            if let Some(change) = classify_event(&event_root, &event) {
                let _ = event_tx.send(WorkerMessage::Change(change));
            }
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        std::thread::Builder::new()
            .name("threadlane-workspace-watcher".into())
            .spawn(move || run_worker(worker_rx, debounce_duration, on_change))
            .map_err(notify::Error::from)?;
        Ok(Self {
            _watcher: watcher,
            stop_tx: Some(worker_tx),
        })
    }
}

impl Drop for WorkspaceWatcher {
    fn drop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(WorkerMessage::Stop);
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

    #[test]
    fn classifies_access_events_as_ignored() {
        let event = Event::new(EventKind::Access(notify::event::AccessKind::Read))
            .add_path("/workspace/file".into());
        assert_eq!(classify_event(Path::new("/workspace"), &event), None);
    }

    #[test]
    fn relevant_bursts_coalesce_and_reset_flags() {
        let debounce = Duration::from_millis(50);
        let start = Instant::now();
        let mut pending = PendingChange::default();
        pending.record(
            WorkspaceChangeEvent {
                git_dirty: true,
                files_dirty: false,
            },
            start,
            debounce,
        );
        pending.record(
            WorkspaceChangeEvent {
                git_dirty: true,
                files_dirty: true,
            },
            start + Duration::from_millis(20),
            debounce,
        );

        assert_eq!(
            pending.take_if_settled(start + Duration::from_millis(69)),
            None
        );
        assert_eq!(
            pending.take_if_settled(start + Duration::from_millis(70)),
            Some(WorkspaceChangeEvent {
                git_dirty: true,
                files_dirty: true,
            })
        );
        assert_eq!(
            pending.take_if_settled(start + Duration::from_millis(100)),
            None
        );

        pending.record(
            WorkspaceChangeEvent {
                git_dirty: true,
                files_dirty: false,
            },
            start + Duration::from_millis(100),
            debounce,
        );
        assert_eq!(
            pending.take_if_settled(start + Duration::from_millis(150)),
            Some(WorkspaceChangeEvent {
                git_dirty: true,
                files_dirty: false,
            })
        );
    }

    #[test]
    fn ignored_events_do_not_extend_a_pending_deadline() {
        let root = Path::new("/workspace");
        let debounce = Duration::from_millis(50);
        let start = Instant::now();
        let mut pending = PendingChange::default();
        pending.record(
            WorkspaceChangeEvent {
                git_dirty: true,
                files_dirty: false,
            },
            start,
            debounce,
        );

        let generated = Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path("/workspace/target/output".into());
        let access = Event::new(EventKind::Access(notify::event::AccessKind::Read))
            .add_path("/workspace/source.rs".into());
        assert_eq!(classify_event(root, &generated), None);
        assert_eq!(classify_event(root, &access), None);

        assert_eq!(
            pending.take_if_settled(start + Duration::from_millis(50)),
            Some(WorkspaceChangeEvent {
                git_dirty: true,
                files_dirty: false,
            })
        );
    }

    fn assert_worker_stops(receiver: mpsc::Receiver<()>, worker: std::thread::JoinHandle<()>) {
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker should stop promptly");
        worker.join().expect("worker should exit cleanly");
    }

    #[test]
    fn worker_stops_while_idle() {
        let (worker_tx, worker_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_worker(worker_rx, Duration::from_secs(60), |_| {});
            let _ = done_tx.send(());
        });

        worker_tx.send(WorkerMessage::Stop).unwrap();
        assert_worker_stops(done_rx, worker);
    }

    #[test]
    fn worker_stops_while_a_change_is_pending() {
        let (worker_tx, worker_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        worker_tx
            .send(WorkerMessage::Change(WorkspaceChangeEvent {
                git_dirty: true,
                files_dirty: true,
            }))
            .unwrap();
        worker_tx.send(WorkerMessage::Stop).unwrap();
        let worker = std::thread::spawn(move || {
            run_worker(worker_rx, Duration::from_secs(60), |_| {});
            let _ = done_tx.send(());
        });

        assert_worker_stops(done_rx, worker);
    }
}
