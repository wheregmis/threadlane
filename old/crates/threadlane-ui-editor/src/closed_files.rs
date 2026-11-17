use std::collections::VecDeque;
use std::path::PathBuf;

pub(crate) const CLOSED_FILE_HISTORY_LIMIT: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileTarget {
    pub(crate) project: PathBuf,
    pub(crate) path: String,
}

impl FileTarget {
    pub(crate) fn new(project: PathBuf, path: impl Into<String>) -> Self {
        Self {
            project,
            path: path.into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClosedFileHistory {
    entries: VecDeque<FileTarget>,
}

impl ClosedFileHistory {
    pub(crate) fn record(&mut self, target: FileTarget) {
        self.entries.retain(|entry| entry != &target);
        self.entries.push_front(target);
        self.entries.truncate(CLOSED_FILE_HISTORY_LIMIT);
    }

    pub(crate) fn pop(&mut self) -> Option<FileTarget> {
        self.entries.pop_front()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn targets(&self) -> impl Iterator<Item = &FileTarget> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ClosedFileHistory, FileTarget, CLOSED_FILE_HISTORY_LIMIT};

    fn target(path: &str) -> FileTarget {
        FileTarget::new(PathBuf::from("/checkout"), path)
    }

    #[test]
    fn records_newest_first_and_deduplicates_exact_targets() {
        let mut history = ClosedFileHistory::default();
        history.record(target("one.rs"));
        history.record(target("two.rs"));
        history.record(target("one.rs"));

        assert_eq!(
            history.targets().cloned().collect::<Vec<_>>(),
            vec![target("one.rs"), target("two.rs")]
        );
    }

    #[test]
    fn caps_history_at_twenty_entries() {
        let mut history = ClosedFileHistory::default();
        for index in 0..(CLOSED_FILE_HISTORY_LIMIT + 3) {
            history.record(target(&format!("{index}.rs")));
        }

        let paths: Vec<_> = history
            .targets()
            .map(|target| target.path.as_str())
            .collect();
        assert_eq!(paths.len(), CLOSED_FILE_HISTORY_LIMIT);
        assert_eq!(paths.first(), Some(&"22.rs"));
        assert_eq!(paths.last(), Some(&"3.rs"));
    }

    #[test]
    fn project_is_part_of_target_identity() {
        let mut history = ClosedFileHistory::default();
        history.record(FileTarget::new(PathBuf::from("/one"), "same.rs"));
        history.record(FileTarget::new(PathBuf::from("/two"), "same.rs"));

        assert_eq!(history.targets().count(), 2);
    }

    #[test]
    fn pop_is_newest_first_and_clear_discards_entries() {
        let mut history = ClosedFileHistory::default();
        history.record(target("one.rs"));
        history.record(target("two.rs"));

        assert_eq!(history.pop(), Some(target("two.rs")));
        history.clear();
        assert!(history.is_empty());
    }
}
