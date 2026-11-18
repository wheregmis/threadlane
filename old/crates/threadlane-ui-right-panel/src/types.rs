use std::path::{Path, PathBuf};

use threadlane_git::{GitFile, GitStatus};
pub use threadlane_ui_kit::RightPanelSurface as Surface;

pub fn can_publish_branch(worktree_available: bool, status: Option<&GitStatus>) -> bool {
    threadlane_ui_kit::review_can_publish_branch(worktree_available, status)
}

pub fn message_generated_matches_active_project(origin: &Path, active: Option<&Path>) -> bool {
    active == Some(origin)
}

pub fn normalize_generated_commit_message(raw: &str) -> String {
    threadlane_provider::normalize_commit_message(raw)
}

pub fn detect_language(path_str: &str) -> &'static str {
    let path = Path::new(path_str);
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts" | "mts" | "cts" | "jsx" | "tsx") => "typescript",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("html" | "htm") => "html",
        Some("css") => "css",
        Some("md" | "markdown") => "markdown",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("go") => "go",
        Some("c" | "h") => "c",
        Some("cpp" | "hpp" | "cc" | "cxx" | "hh") => "cpp",
        Some("diff" | "patch") => "diff",
        Some("zig") => "zig",
        _ => match path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|s| s.to_lowercase())
            .as_deref()
        {
            Some("dockerfile") => "bash",
            Some("cargo.lock") => "toml",
            _ => "text",
        },
    }
}

pub use threadlane_ui_kit::ReviewTab;
pub use threadlane_ui_kit::ReviewViewMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReviewDiffTarget {
    File(String),
    AllChanges,
}

impl ReviewDiffTarget {
    pub(crate) fn title(&self) -> String {
        match self {
            Self::File(path) => format!("Review · {path}"),
            Self::AllChanges => "Review · All changes".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReviewDiffRequest {
    pub project: PathBuf,
    pub target: ReviewDiffTarget,
    pub options: threadlane_git::DiffOptions,
    pub revision: u64,
}

pub(crate) enum ReviewDiffState {
    Loading,
    Ready { empty: bool },
    Failed(String),
}

pub(crate) fn available_surfaces() -> Vec<Surface> {
    let mut surfaces = vec![
        Surface::Trajectory,
        Surface::Agents,
        Surface::Review,
        Surface::Files,
    ];
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    surfaces.push(Surface::Browser);
    surfaces
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitAction {
    Commit,
    CommitAndPush,
    StageFile(String),
    UnstageFile(String),
    StageFiles(Vec<String>),
    UnstageFiles(Vec<String>),
    StashPush {
        message: Option<String>,
        include_untracked: bool,
    },
    Push,
    Pull,
    Fetch,
    StageAll,
    UnstageAll,
    #[allow(dead_code)]
    CreatePullRequest,
    Checkout(String),
    CheckoutStash(String),
    CheckoutCarry(String),
    CreateBranch(String),
    DeleteBranch(String),
    Merge(String),
    PopStash(Option<usize>),
    DropStash(Option<usize>),
    DiscardFile(String),
    DiscardFiles(Vec<String>),
    DiscardAll,
    IgnoreFile(String),
    IgnoreExtension(String),
}

pub use threadlane_ui_kit::ReviewDiscardTarget as DiscardOption;

pub(crate) fn discard_git_action(target: &DiscardOption) -> GitAction {
    match target {
        DiscardOption::Single(path) => GitAction::DiscardFile(path.clone()),
        DiscardOption::Selected(paths) => GitAction::DiscardFiles(paths.clone()),
        DiscardOption::All(_) => GitAction::DiscardAll,
    }
}

/// Files-surface tree node, delivered daemon-side by
/// `SessionCommand::ListProjectFiles` (protocol v3).
pub type FileNode = threadlane_protocol::repo::ProjectFileNode;

pub enum PanelEvent {
    FilesLoaded {
        project: PathBuf,
        nodes: Vec<FileNode>,
    },
    ReviewLoaded {
        project: PathBuf,
        status: Option<GitStatus>,
        files: Vec<GitFile>,
        error: Option<String>,
    },
    WorkspaceChanged {
        project: PathBuf,
        git_dirty: bool,
        files_dirty: bool,
    },
    MessageGenerated {
        project: PathBuf,
        result: Result<String, String>,
        /// True when the diff sent to the model was truncated at the size
        /// limit (Synara DiffTruncationWarning pattern).
        diff_truncated: bool,
    },
    ActionFinished {
        project: PathBuf,
        status: Result<GitStatus, String>,
        action_error: Option<String>,
        action_message: Option<String>,
        checkout_succeeded: bool,
    },
    CommitFilesLoaded {
        sha: String,
        files: Vec<GitFile>,
    },
    StashFilesLoaded {
        project: PathBuf,
        index: usize,
        files: Vec<GitFile>,
    },
}
