use std::path::{Path, PathBuf};

use gpui_component::IconName;
use threadlane_git::{GitFile, GitStatus};

pub(crate) fn can_publish_branch(worktree_available: bool, status: Option<&GitStatus>) -> bool {
    worktree_available
        && status.is_some_and(|status| {
            !status.has_upstream
                && !status.detached
                && status.branch.is_some()
                && status.remote.is_some()
        })
}

pub(crate) fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

pub(crate) fn can_create_pull_request(worktree_available: bool, status: Option<&GitStatus>) -> bool {
    worktree_available
        && status.is_some_and(|status| {
            status.pr_ready
                && !status.detached
                && status.branch.as_deref().and_then(nonempty).is_some()
                && status.remote.is_some()
                && status.has_upstream
                && status.pr_lookup_available
                && status.pr.is_none()
        })
}

pub(crate) fn message_generated_matches_active_project(origin: &Path, active: Option<&Path>) -> bool {
    active == Some(origin)
}

pub(crate) fn normalize_generated_commit_message(raw: &str) -> String {
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(trimmed)
        .trim();
    if unquoted.starts_with("```") {
        unquoted
            .lines()
            .filter(|line| !line.trim().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    } else {
        unquoted.to_string()
    }
}

pub(crate) fn detect_language(path_str: &str) -> &'static str {
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

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ReviewTab {
    #[default]
    Changes,
    History,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Surface {
    Review,
    Files,
    Browser,
}

impl Surface {
    pub(crate) fn all() -> Vec<Self> {
        let mut surfaces = vec![Self::Review, Self::Files];
        #[cfg(target_os = "macos")]
        surfaces.push(Self::Browser);
        surfaces
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Review => "Review",
            Self::Files => "Files",
            Self::Browser => "Browser",
        }
    }

    pub(crate) fn icon(self) -> IconName {
        match self {
            Self::Review => IconName::File,
            Self::Files => IconName::Folder,
            Self::Browser => IconName::Globe,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitAction {
    Commit,
    CommitAndPush,
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
    Merge(String),
    PopStash(Option<usize>),
    DropStash(Option<usize>),
    DiscardFile(String),
    IgnoreFile(String),
    IgnoreExtension(String),
}

#[derive(Clone, Debug)]
pub(crate) struct FileNode {
    pub(crate) relative_path: String,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    pub(crate) children: Vec<FileNode>,
}

pub(crate) enum PanelEvent {
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
    },
    ActionFinished {
        project: PathBuf,
        status: Result<GitStatus, String>,
        action_error: Option<String>,
        action_message: Option<String>,
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
