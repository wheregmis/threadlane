//! Git client service for GPUI frontend — all operations delegate to the daemon.

use std::path::Path;
use threadlane_protocol::capabilities::*;
use threadlane_protocol::git::{GitBranchesRequest, GitFile, GitHubPrInfo, GitStatus};

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    crate::services::chat::executor()
}

fn block<F, T>(f: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    executor()?.block_on(f)
}

async fn client() -> Result<std::sync::Arc<threadlane_protocol::DaemonClient>, String> {
    crate::services::daemon_client::get_daemon_client().await
}

pub fn sync_remote(_project: &Path) -> Result<(), String> {
    Ok(())
}

pub fn inspect(project: &Path) -> Result<GitStatus, String> {
    let project_str = project.to_string_lossy().to_string();
    block(async move {
        let c = client().await?;
        let res = c.git_status(&project_str).await?;
        let branch_details = c
            .git_branches(GitBranchesRequest {
                project_path: project_str.clone(),
            })
            .await?
            .branches;
        let files: Vec<GitFile> = res
            .files
            .into_iter()
            .map(|f| {
                let status_char = match &f.status {
                    threadlane_protocol::git::GitFileStatusCode::Added => 'A',
                    threadlane_protocol::git::GitFileStatusCode::Deleted => 'D',
                    threadlane_protocol::git::GitFileStatusCode::Renamed => 'R',
                    threadlane_protocol::git::GitFileStatusCode::Typechange => 'T',
                    threadlane_protocol::git::GitFileStatusCode::Conflicted => 'U',
                    threadlane_protocol::git::GitFileStatusCode::Untracked => '?',
                    threadlane_protocol::git::GitFileStatusCode::Modified => 'M',
                };
                GitFile {
                    path: f.path,
                    status: status_char.to_string(),
                    index_status: if f.staged { status_char } else { ' ' },
                    worktree_status: if f.staged { ' ' } else { status_char },
                    staged: f.staged,
                    unstaged: !f.staged,
                    ..Default::default()
                }
            })
            .collect();
        let branches = branch_details
            .iter()
            .filter(|branch| !branch.is_remote)
            .map(|branch| branch.name.clone())
            .collect::<Vec<_>>();
        let current_branch = res.branch.clone();
        let has_upstream = current_branch.as_deref().is_some_and(|current| {
            branch_details
                .iter()
                .any(|branch| branch.name == current && branch.upstream.is_some())
        });
        let staged_changes = files.iter().any(|file| file.staged);
        let unstaged_changes = files.iter().any(|file| file.unstaged);
        Ok(GitStatus {
            branch: res.branch,
            files,
            ahead: res.ahead,
            behind: res.behind,
            default_branch: branch_details
                .iter()
                .find(|branch| branch.is_default && !branch.is_remote)
                .map(|branch| branch.name.clone()),
            detached: current_branch.is_none(),
            has_upstream,
            has_changes: staged_changes || unstaged_changes,
            staged_changes,
            unstaged_changes,
            branches,
            branch_details,
            ..Default::default()
        })
    })
}

pub fn commit_message_diff(project: &Path) -> Result<String, String> {
    let project_str = project.to_string_lossy().to_string();
    block(async move {
        let c = client().await?;
        c.git_commit_diff_message(&project_str)
            .await
            .map(|r| r.message)
    })
}

pub fn diff_file(project: &Path, path: &str) -> Result<String, String> {
    let project_str = project.to_string_lossy().to_string();
    let path = path.to_string();
    block(async move {
        let c = client().await?;
        let res = c
            .git_diff(threadlane_protocol::GitDiffRequest {
                project_path: project_str,
                file_path: Some(path),
                staged: false,
            })
            .await?;
        Ok(res.diff)
    })
}

pub fn stage_file(project: &Path, path: &str) -> Result<(), String> {
    let req = GitStageFileRequest {
        project_path: project.to_string_lossy().to_string(),
        file_path: path.to_string(),
        stage: true,
    };
    block(async move { client().await?.git_stage_file(req).await })
}

pub fn unstage_file(project: &Path, path: &str) -> Result<(), String> {
    let req = GitStageFileRequest {
        project_path: project.to_string_lossy().to_string(),
        file_path: path.to_string(),
        stage: false,
    };
    block(async move { client().await?.git_stage_file(req).await })
}

pub fn commit_staged(project: &Path, message: &str) -> Result<String, String> {
    let req = GitCommitRequest {
        project_path: project.to_string_lossy().to_string(),
        message: message.to_string(),
    };
    block(async move { client().await?.git_commit(req).await.map(|r| r.sha) })
}

pub fn push(project: &Path) -> Result<(), String> {
    let req = GitPushPullRequest {
        project_path: project.to_string_lossy().to_string(),
    };
    block(async move { client().await?.git_push(req).await })
}

pub fn pull(project: &Path) -> Result<(), String> {
    let req = GitPushPullRequest {
        project_path: project.to_string_lossy().to_string(),
    };
    block(async move { client().await?.git_pull(req).await })
}

pub fn fetch(_project: &Path) -> Result<(), String> {
    Ok(())
}

pub fn create_pull_request(_project: &Path) -> Result<String, String> {
    Ok(String::new())
}

pub fn checkout(project: &Path, branch: &str) -> Result<(), String> {
    let req = threadlane_protocol::GitCheckoutRequest {
        project_path: project.to_string_lossy().to_string(),
        branch: branch.to_string(),
        create_if_missing: false,
    };
    block(async move { client().await?.git_checkout(req).await })
}

pub fn checkout_with_stash(project: &Path, branch: &str) -> Result<(), String> {
    checkout(project, branch)
}

pub fn checkout_carrying_changes(project: &Path, branch: &str) -> Result<(), String> {
    checkout(project, branch)
}

pub fn create_branch(project: &Path, branch: &str) -> Result<(), String> {
    let req = threadlane_protocol::GitCheckoutRequest {
        project_path: project.to_string_lossy().to_string(),
        branch: branch.to_string(),
        create_if_missing: true,
    };
    block(async move { client().await?.git_checkout(req).await })
}

pub fn merge(project: &Path, branch: &str) -> Result<(), String> {
    let req = GitMergeRequest {
        project_path: project.to_string_lossy().to_string(),
        branch: branch.to_string(),
    };
    block(async move { client().await?.git_merge(req).await })
}

pub fn pop_stash(project: &Path, idx: Option<usize>) -> Result<(), String> {
    let req = GitStashActionRequest {
        project_path: project.to_string_lossy().to_string(),
        index: idx,
    };
    block(async move { client().await?.git_pop_stash(req).await })
}

pub fn drop_stash(project: &Path, idx: Option<usize>) -> Result<(), String> {
    let req = GitStashActionRequest {
        project_path: project.to_string_lossy().to_string(),
        index: idx,
    };
    block(async move { client().await?.git_drop_stash(req).await })
}

pub fn discard_file_changes(project: &Path, path: &str) -> Result<(), String> {
    let req = GitDiscardFileRequest {
        project_path: project.to_string_lossy().to_string(),
        file_path: path.to_string(),
    };
    block(async move { client().await?.git_discard_file(req).await })
}

pub fn ignore_file(project: &Path, path: &str) -> Result<(), String> {
    let req = GitIgnoreRequest {
        project_path: project.to_string_lossy().to_string(),
        pattern: path.to_string(),
    };
    block(async move { client().await?.git_ignore(req).await })
}

pub fn ignore_extension(project: &Path, ext: &str) -> Result<(), String> {
    let pattern = format!("*.{ext}");
    let req = GitIgnoreRequest {
        project_path: project.to_string_lossy().to_string(),
        pattern,
    };
    block(async move { client().await?.git_ignore(req).await })
}

pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    Ok(())
}

pub fn inspect_stash_files(_project: &Path, _idx: usize) -> Result<Vec<GitFile>, String> {
    Ok(Vec::new())
}

pub fn diff_stash_file(_project: &Path, _idx: usize, _path: &str) -> Result<String, String> {
    Ok(String::new())
}

pub fn inspect_commit_files(_project: &Path, _sha: &str) -> Result<Vec<GitFile>, String> {
    Ok(Vec::new())
}

pub fn diff_commit_file(_project: &Path, _sha: &str, _path: &str) -> Result<String, String> {
    Ok(String::new())
}

pub fn inspect_pr_for_branch(
    _project: &Path,
    _branch: &str,
) -> Result<Option<GitHubPrInfo>, String> {
    Ok(None)
}
