use std::time::{SystemTime, UNIX_EPOCH};

/// A `JoinHandle` wrapper that aborts the spawned task on drop.
///
/// Used by the tool dispatcher and turn driver to ensure spawned
/// provider/tool tasks are cancelled when the parent is dropped.
pub struct AbortOnDrop<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    pub(crate) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    pub(crate) async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        let result = self.handle.as_mut().expect("task handle missing").await;
        self.handle = None;
        result
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// Home directory resolution, canonical in `threadlane-project`.
/// Re-exported here so existing `threadlane_runtime::utils::dirs_home` and
/// `threadlane_runtime::dirs_home` paths keep working; new code should
/// import `threadlane_project::dirs_home` directly.
pub use threadlane_project::dirs_home;

/// Returns the current Unix timestamp in milliseconds.
pub fn now_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Returns the current Unix timestamp in seconds.
pub fn now_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
