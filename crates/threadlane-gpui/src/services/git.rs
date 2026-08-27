//! Git client service for GPUI frontend talking to threadlane-daemon.

use std::path::Path;
use threadlane_protocol::git::{GitFile, GitStatus, GitHubPrInfo};

pub fn sync_remote(_project: &Path) -> Result<(), String> {
    Ok(())
}

pub fn inspect(project: &Path) -> Result<GitStatus, String> {
    let project_str = project.to_string_lossy().to_string();
    if let Ok(executor) = crate::services::chat::executor() {
        executor.block_on(async move {
            let client = crate::services::daemon_client::get_daemon_client().await?;
            let res = client.git_status(&project_str).await?;
            let files = res
                .files
                .into_iter()
                .map(|f| GitFile {
                    path: f.path,
                    staged: f.staged,
                    unstaged: !f.staged,
                    ..Default::default()
                })
                .collect();
            Ok(GitStatus {
                branch: res.branch,
                files,
                ahead: res.ahead,
                behind: res.behind,
                ..Default::default()
            })
        })
    } else {
        Err("Failed to acquire runtime".into())
    }
}

pub fn commit_message_diff(_project: &Path) -> Result<String, String> {
    Ok(String::new())
}

pub fn diff_file(_project: &Path, _path: &str) -> Result<String, String> {
    Ok(String::new())
}

pub fn stage_file(_project: &Path, _path: &str) -> Result<(), String> {
    Ok(())
}

pub fn unstage_file(_project: &Path, _path: &str) -> Result<(), String> {
    Ok(())
}

pub fn commit_staged(_project: &Path, _message: &str) -> Result<String, String> {
    Ok("HEAD".into())
}

pub fn push(_project: &Path) -> Result<(), String> {
    Ok(())
}

pub fn pull(_project: &Path) -> Result<(), String> {
    Ok(())
}

pub fn fetch(_project: &Path) -> Result<(), String> {
    Ok(())
}

pub fn create_pull_request(_project: &Path) -> Result<String, String> {
    Ok(String::new())
}

pub fn checkout(_project: &Path, _branch: &str) -> Result<(), String> {
    Ok(())
}

pub fn checkout_with_stash(_project: &Path, _branch: &str) -> Result<(), String> {
    Ok(())
}

pub fn checkout_carrying_changes(_project: &Path, _branch: &str) -> Result<(), String> {
    Ok(())
}

pub fn create_branch(_project: &Path, _branch: &str) -> Result<(), String> {
    Ok(())
}

pub fn merge(_project: &Path, _branch: &str) -> Result<(), String> {
    Ok(())
}

pub fn pop_stash(_project: &Path, _idx: Option<usize>) -> Result<(), String> {
    Ok(())
}

pub fn drop_stash(_project: &Path, _idx: Option<usize>) -> Result<(), String> {
    Ok(())
}

pub fn discard_file_changes(_project: &Path, _path: &str) -> Result<(), String> {
    Ok(())
}

pub fn ignore_file(_project: &Path, _path: &str) -> Result<(), String> {
    Ok(())
}

pub fn ignore_extension(_project: &Path, _ext: &str) -> Result<(), String> {
    Ok(())
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

pub fn inspect_pr_for_branch(_project: &Path, _branch: &str) -> Result<Option<GitHubPrInfo>, String> {
    Ok(None)
}
