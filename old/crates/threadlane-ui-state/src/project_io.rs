//! Async helpers over the daemon's project-io surface (protocol v3).
//!
//! Every UI callsite that browses project files or runs Git work routes
//! through these so a remote-attached app operates on the daemon host's
//! filesystem, not its own. An attached pre-3 daemon answers with
//! [`UNSUPPORTED_PROJECT_IO`] rather than silently touching the client's
//! disk — the wrong-host result is worse than the honest error.
//!
//! All calls go through [`DaemonClient::request`]; run them on
//! `crate::chat::executor()` (or any Tokio context) since `command_request`
//! is async.

use std::path::Path;
use std::sync::Arc;

use threadlane_client::DaemonClient;
use threadlane_protocol::daemon::{CommandResponse, SessionCommand};
pub use threadlane_protocol::daemon::{ProjectFileError, VersionedFile};
use threadlane_protocol::repo::{
    DiffOptions, GitActionOutcome, GitFile, GitFileInventory, GitHubPrInfo, GitOperation,
    GitResponse, GitStatus, ProjectFileNode,
};

/// Error produced for every project-io call an attached pre-3 daemon
/// cannot answer.
pub const UNSUPPORTED_PROJECT_IO: &str =
    "the attached daemon does not support project file/git requests (protocol v3)";

fn guarded_request_error(client: &Arc<dyn DaemonClient>, message: String) -> ProjectFileError {
    if !client.is_connected() || message.contains("not connected") {
        ProjectFileError::Disconnected
    } else if message.contains("Guarded file saves require protocol")
        || message.contains("does not support command requests")
    {
        ProjectFileError::Unsupported
    } else {
        ProjectFileError::Io { message }
    }
}

async fn git_request(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    operation: GitOperation,
) -> Result<GitResponse, String> {
    if !client.supports_project_io() {
        return Err(UNSUPPORTED_PROJECT_IO.to_string());
    }
    match client
        .request(SessionCommand::GitRequest {
            work_dir: work_dir.to_path_buf(),
            operation,
        })
        .await?
    {
        CommandResponse::Git { response } => Ok(response),
        _ => Err("daemon answered GitRequest with a mismatched response".to_string()),
    }
}

async fn git_text(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    operation: GitOperation,
) -> Result<String, String> {
    match git_request(client, work_dir, operation).await? {
        GitResponse::Text { text } => Ok(text),
        _ => Err("daemon answered a diff request with a mismatched response".to_string()),
    }
}

/// `git status` (+ `fetch --prune` when `sync_remote`).
pub async fn inspect(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    sync_remote: bool,
) -> Result<GitStatus, String> {
    match git_request(client, work_dir, GitOperation::Inspect { sync_remote }).await? {
        GitResponse::Status { status } => Ok(*status),
        _ => Err("daemon answered Inspect with a mismatched response".to_string()),
    }
}

/// The pull request associated with `branch`, if one exists.
pub async fn inspect_pr_for_branch(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    branch: String,
) -> Result<Option<GitHubPrInfo>, String> {
    match git_request(client, work_dir, GitOperation::InspectPrForBranch { branch }).await? {
        GitResponse::Pr { pr } => Ok(pr),
        _ => Err("daemon answered InspectPrForBranch with a mismatched response".to_string()),
    }
}

/// Unified diff of one path (untracked included).
pub async fn diff_file(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    path: String,
    options: DiffOptions,
) -> Result<String, String> {
    git_text(client, work_dir, GitOperation::DiffFile { path, options }).await
}

/// Concatenated diffs of `paths` (empty hunks skipped).
pub async fn diff_files(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    paths: Vec<String>,
    options: DiffOptions,
) -> Result<String, String> {
    git_text(client, work_dir, GitOperation::DiffFiles { paths, options }).await
}

/// Whole-worktree diff (staged + unstaged + untracked).
pub async fn diff_worktree(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    options: DiffOptions,
) -> Result<String, String> {
    git_text(client, work_dir, GitOperation::DiffWorktree { options }).await
}

/// The diff commit-message generation consumes.
pub async fn commit_message_diff(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
) -> Result<String, String> {
    git_text(client, work_dir, GitOperation::CommitMessageDiff).await
}

/// `base...HEAD` diff for draft-PR generation.
pub async fn draft_pr_diff(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    base: String,
) -> Result<String, String> {
    git_text(client, work_dir, GitOperation::DraftPrDiff { base }).await
}

/// Diff of `branch` against its base (lane/worktree review).
pub async fn diff_branch(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    branch: String,
) -> Result<String, String> {
    git_text(client, work_dir, GitOperation::DiffBranch { branch }).await
}

/// Files recorded in `stash@{index}`.
pub async fn stash_files(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    index: usize,
) -> Result<Vec<GitFile>, String> {
    match git_request(client, work_dir, GitOperation::StashFiles { index }).await? {
        GitResponse::Files { files } => Ok(files),
        _ => Err("daemon answered StashFiles with a mismatched response".to_string()),
    }
}

/// Diff of one file inside `stash@{index}`.
pub async fn diff_stash_file(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    index: usize,
    path: String,
) -> Result<String, String> {
    git_text(client, work_dir, GitOperation::DiffStashFile { index, path }).await
}

/// Files recorded in commit `sha`.
pub async fn commit_files(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    sha: String,
) -> Result<Vec<GitFile>, String> {
    match git_request(client, work_dir, GitOperation::CommitFiles { sha }).await? {
        GitResponse::Files { files } => Ok(files),
        _ => Err("daemon answered CommitFiles with a mismatched response".to_string()),
    }
}

/// Diff of one file inside commit `sha`.
pub async fn diff_commit_file(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    sha: String,
    path: String,
) -> Result<String, String> {
    git_text(client, work_dir, GitOperation::DiffCommitFile { sha, path }).await
}

/// `git ls-files` inventory for `@` completion.
pub async fn file_inventory(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
) -> Result<GitFileInventory, String> {
    match git_request(client, work_dir, GitOperation::FileInventory).await? {
        GitResponse::Inventory { inventory } => Ok(inventory),
        _ => Err("daemon answered FileInventory with a mismatched response".to_string()),
    }
}

/// Whether `work_dir` is inside a Git work tree.
pub async fn is_repo(client: &Arc<dyn DaemonClient>, work_dir: &Path) -> Result<bool, String> {
    match git_request(client, work_dir, GitOperation::IsRepo).await? {
        GitResponse::Bool { value } => Ok(value),
        _ => Err("daemon answered IsRepo with a mismatched response".to_string()),
    }
}

/// Run a mutating `GitOperation`; the daemon re-inspects afterwards and
/// reports both the action's result and the fresh status.
pub async fn run_action(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    operation: GitOperation,
) -> Result<GitActionOutcome, String> {
    match git_request(client, work_dir, operation).await? {
        GitResponse::Action { outcome } => Ok(outcome),
        _ => Err("daemon answered a mutation with a mismatched response".to_string()),
    }
}

/// The project's file tree, up to `limit` entries.
pub async fn project_files(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    limit: usize,
) -> Result<Vec<ProjectFileNode>, String> {
    if !client.supports_project_io() {
        return Err(UNSUPPORTED_PROJECT_IO.to_string());
    }
    match client
        .request(SessionCommand::ListProjectFiles {
            work_dir: work_dir.to_path_buf(),
            limit,
        })
        .await?
    {
        CommandResponse::ProjectFiles { nodes } => Ok(nodes),
        _ => Err("daemon answered ListProjectFiles with a mismatched response".to_string()),
    }
}

/// Read `path` (project-relative) as UTF-8 text.
pub async fn read_file(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    path: String,
) -> Result<String, String> {
    if !client.supports_project_io() {
        return Err(UNSUPPORTED_PROJECT_IO.to_string());
    }
    match client
        .request(SessionCommand::ReadProjectFile {
            work_dir: work_dir.to_path_buf(),
            path,
        })
        .await?
    {
        CommandResponse::FileContent { content } => Ok(content),
        _ => Err("daemon answered ReadProjectFile with a mismatched response".to_string()),
    }
}

/// Read `path` as UTF-8 text with a daemon-owned SHA-256 version.
pub async fn read_file_versioned(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    path: String,
) -> Result<VersionedFile, ProjectFileError> {
    if !client.is_connected() {
        return Err(ProjectFileError::Disconnected);
    }
    if !client.supports_guarded_saves() {
        return Err(ProjectFileError::Unsupported);
    }
    let response = client
        .request(SessionCommand::ReadProjectFileVersioned {
            work_dir: work_dir.to_path_buf(),
            path,
        })
        .await
        .map_err(|message| guarded_request_error(client, message))?;
    match response {
        CommandResponse::VersionedFile { result } => result,
        _ => Err(ProjectFileError::Io {
            message: "daemon answered ReadProjectFileVersioned with a mismatched response"
                .to_string(),
        }),
    }
}

/// Save `content` only if the daemon's current file version matches the
/// version previously returned by `read_file_versioned`.
pub async fn write_file_guarded(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    path: String,
    content: String,
    expected_version: String,
) -> Result<String, ProjectFileError> {
    if !client.is_connected() {
        return Err(ProjectFileError::Disconnected);
    }
    if !client.supports_guarded_saves() {
        return Err(ProjectFileError::Unsupported);
    }
    let response = client
        .request(SessionCommand::WriteProjectFileGuarded {
            work_dir: work_dir.to_path_buf(),
            path,
            content,
            expected_version,
        })
        .await
        .map_err(|message| guarded_request_error(client, message))?;
    match response {
        CommandResponse::GuardedFileWrite { result } => result,
        _ => Err(ProjectFileError::Io {
            message: "daemon answered WriteProjectFileGuarded with a mismatched response"
                .to_string(),
        }),
    }
}

/// Overwrite `path` (project-relative) with UTF-8 `content`.
pub async fn write_file(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    path: String,
    content: String,
) -> Result<(), String> {
    if !client.supports_project_io() {
        return Err(UNSUPPORTED_PROJECT_IO.to_string());
    }
    client
        .request(SessionCommand::WriteProjectFile {
            work_dir: work_dir.to_path_buf(),
            path,
            content,
        })
        .await
        .map(|_| ())
}

/// Whether `path` names an existing file entry under `work_dir`.
pub async fn file_exists(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
    path: String,
) -> Result<bool, String> {
    if !client.supports_project_io() {
        return Err(UNSUPPORTED_PROJECT_IO.to_string());
    }
    match client
        .request(SessionCommand::ProjectFileExists {
            work_dir: work_dir.to_path_buf(),
            path,
        })
        .await?
    {
        CommandResponse::FileExists { exists } => Ok(exists),
        _ => Err("daemon answered ProjectFileExists with a mismatched response".to_string()),
    }
}

/// Subscribe to `SessionEvent::WorkspaceChanged` for `work_dir`.
pub async fn watch_project(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
) -> Result<(), String> {
    if !client.supports_project_io() {
        return Err(UNSUPPORTED_PROJECT_IO.to_string());
    }
    client
        .request(SessionCommand::WatchProject {
            work_dir: work_dir.to_path_buf(),
        })
        .await
        .map(|_| ())
}

/// Release one `watch_project` subscription.
pub async fn unwatch_project(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
) -> Result<(), String> {
    if !client.supports_project_io() {
        return Err(UNSUPPORTED_PROJECT_IO.to_string());
    }
    client
        .request(SessionCommand::UnwatchProject {
            work_dir: work_dir.to_path_buf(),
        })
        .await
        .map(|_| ())
}

/// Ask the daemon to emit `SessionEvent::WorktreeBases` for `work_dir`
/// (fire-and-forget; the journaled event is the answer).
pub async fn get_worktree_bases(
    client: &Arc<dyn DaemonClient>,
    work_dir: &Path,
) -> Result<(), String> {
    if !client.supports_project_io() {
        return Err(UNSUPPORTED_PROJECT_IO.to_string());
    }
    client
        .command(SessionCommand::GetWorktreeBases {
            work_dir: work_dir.to_path_buf(),
        })
        .await
}

/// Search only the owning daemon's saved files. Never fall back to client disk.
pub async fn search_files(client: &Arc<dyn DaemonClient>, root: &Path, query: String) -> Result<threadlane_protocol::repo::FileSearchResult, String> {
    if !client.is_connected() { return Err("Daemon disconnected. Reconnect and retry.".into()); }
    if !client.supports_file_search() { return Err("Unsupported daemon version. Update to protocol v6 or newer.".into()); }
    match client.request(SessionCommand::SearchProjectFiles { work_dir: root.into(), query }).await? {
        CommandResponse::FileSearch { result } => Ok(result),
        _ => Err("Unexpected search response".into()),
    }
}

pub async fn validate_search_target(client: &Arc<dyn DaemonClient>, root: &Path, path: String) -> Result<(), String> {
    if !client.supports_file_search() { return Err("Find in files requires a connected protocol v6 daemon".into()); }
    match client.request(SessionCommand::ValidateSearchTarget { work_dir: root.into(), path }).await? {
        CommandResponse::Ack => Ok(()),
        _ => Err("Unexpected file validation response".into()),
    }
}
