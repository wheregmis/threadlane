use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitError {
    pub(crate) work_dir: PathBuf,
    pub message: String,
}

impl GitError {
    pub fn new(work_dir: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            work_dir: work_dir.into(),
            message: message.into(),
        }
    }

    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.work_dir.display(), self.message)
    }
}

impl std::error::Error for GitError {}
