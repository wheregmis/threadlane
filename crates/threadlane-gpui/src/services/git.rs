//! Git client service for GPUI frontend — all operations delegate to the daemon.

use std::path::Path;
use threadlane_protocol::capabilities::*;
use threadlane_protocol::git::{GitFile, GitHubPrInfo, GitStatus};

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

pub fn sync_remote(project: &Path) -> Result<(), String> {
    fetch(project)
}

pub fn inspect(project: &Path) -> Result<GitStatus, String> {
    let project_str = project.to_string_lossy().to_string();
    block(async move {
        let c = client().await?;
        Ok(c.git_inspect(&project_str).await?.status)
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

pub fn fetch(project: &Path) -> Result<(), String> {
    let req = threadlane_protocol::GitFetchRequest {
        project_path: project.to_string_lossy().to_string(),
    };
    block(async move { client().await?.git_fetch(req).await })
}

pub fn create_pull_request(project: &Path) -> Result<String, String> {
    let req = GitPushPullRequest {
        project_path: project.to_string_lossy().to_string(),
    };
    block(async move {
        client()
            .await?
            .git_create_pull_request(req)
            .await
            .map(|r| r.url)
    })
}

pub fn checkout(project: &Path, branch: &str) -> Result<(), String> {
    let req = threadlane_protocol::GitCheckoutRequest {
        project_path: project.to_string_lossy().to_string(),
        branch: branch.to_string(),
        create_if_missing: false,
        mode: threadlane_protocol::GitCheckoutMode::Direct,
    };
    block(async move { client().await?.git_checkout(req).await })
}

pub fn checkout_with_stash(project: &Path, branch: &str) -> Result<(), String> {
    let req = threadlane_protocol::GitCheckoutRequest {
        project_path: project.to_string_lossy().to_string(),
        branch: branch.to_string(),
        create_if_missing: false,
        mode: threadlane_protocol::GitCheckoutMode::Stash,
    };
    block(async move { client().await?.git_checkout(req).await })
}

pub fn checkout_carrying_changes(project: &Path, branch: &str) -> Result<(), String> {
    let req = threadlane_protocol::GitCheckoutRequest {
        project_path: project.to_string_lossy().to_string(),
        branch: branch.to_string(),
        create_if_missing: false,
        mode: threadlane_protocol::GitCheckoutMode::Carry,
    };
    block(async move { client().await?.git_checkout(req).await })
}

pub fn create_branch(project: &Path, branch: &str) -> Result<(), String> {
    let req = threadlane_protocol::GitCheckoutRequest {
        project_path: project.to_string_lossy().to_string(),
        branch: branch.to_string(),
        create_if_missing: true,
        mode: threadlane_protocol::GitCheckoutMode::Direct,
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

pub fn inspect_stash_files(project: &Path, idx: usize) -> Result<Vec<GitFile>, String> {
    let req = threadlane_protocol::GitStashFilesRequest {
        project_path: project.to_string_lossy().to_string(),
        index: idx,
    };
    block(async move {
        client()
            .await?
            .git_inspect_stash_files(req)
            .await
            .map(|r| r.files)
    })
}

pub fn diff_stash_file(project: &Path, idx: usize, path: &str) -> Result<String, String> {
    let req = threadlane_protocol::GitStashDiffRequest {
        project_path: project.to_string_lossy().to_string(),
        index: idx,
        file_path: path.to_string(),
    };
    block(async move {
        client()
            .await?
            .git_diff_stash_file(req)
            .await
            .map(|r| r.diff)
    })
}

pub fn inspect_commit_files(project: &Path, sha: &str) -> Result<Vec<GitFile>, String> {
    let req = threadlane_protocol::GitCommitFilesRequest {
        project_path: project.to_string_lossy().to_string(),
        sha: sha.to_string(),
    };
    block(async move {
        client()
            .await?
            .git_inspect_commit_files(req)
            .await
            .map(|r| r.files)
    })
}

pub fn diff_commit_file(project: &Path, sha: &str, path: &str) -> Result<String, String> {
    let req = threadlane_protocol::GitCommitDiffRequest {
        project_path: project.to_string_lossy().to_string(),
        sha: sha.to_string(),
        file_path: path.to_string(),
    };
    block(async move {
        client()
            .await?
            .git_diff_commit_file(req)
            .await
            .map(|r| r.diff)
    })
}

pub fn inspect_pr_for_branch(project: &Path, branch: &str) -> Result<Option<GitHubPrInfo>, String> {
    let req = threadlane_protocol::GitInspectPrRequest {
        project_path: project.to_string_lossy().to_string(),
        branch: Some(branch.to_string()),
    };
    block(async move { client().await?.git_inspect_pr(req).await.map(|r| r.pr) })
}
