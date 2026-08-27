use std::path::Path;
use threadlane_git::{inspect, list_branches_detailed};
use threadlane_protocol::git::*;

#[derive(Clone, Default)]
pub struct GitService;

impl GitService {
    pub fn new() -> Self {
        Self
    }

    pub fn status(&self, req: GitStatusRequest) -> Result<GitStatusResponse, String> {
        let work_dir = Path::new(&req.project_path);
        let status = inspect(work_dir).map_err(|e| e.to_string())?;

        let files = status
            .files
            .into_iter()
            .map(|f| {
                let code = match f.status.chars().next().unwrap_or('M') {
                    '?' => GitFileStatusCode::Untracked,
                    'A' => GitFileStatusCode::Added,
                    'D' => GitFileStatusCode::Deleted,
                    'R' => GitFileStatusCode::Renamed,
                    'T' => GitFileStatusCode::Typechange,
                    'U' => GitFileStatusCode::Conflicted,
                    _ => GitFileStatusCode::Modified,
                };
                GitFileStatus {
                    path: f.path,
                    status: code,
                    staged: f.staged,
                }
            })
            .collect();

        Ok(GitStatusResponse {
            branch: status.branch,
            files,
            ahead: status.ahead,
            behind: status.behind,
        })
    }

    pub fn diff(&self, req: GitDiffRequest) -> Result<GitDiffResponse, String> {
        let work_dir = Path::new(&req.project_path);
        let mut args = vec!["diff"];
        if req.staged {
            args.push("--staged");
        }
        if let Some(ref file) = req.file_path {
            args.push("--");
            args.push(file);
        }

        let output = std::process::Command::new("git")
            .args(&args)
            .current_dir(work_dir)
            .output()
            .map_err(|e| format!("Failed to execute git diff: {e}"))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git diff failed: {err}"));
        }

        let diff = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(GitDiffResponse { diff })
    }

    pub fn list_branches(&self, req: GitBranchesRequest) -> Result<GitBranchesResponse, String> {
        let work_dir = Path::new(&req.project_path);
        let branches = list_branches_detailed(work_dir, None)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|b| GitBranchInfo {
                name: b.name,
                is_current: b.is_current,
                is_remote: b.is_remote,
            })
            .collect();

        Ok(GitBranchesResponse { branches })
    }

    pub fn checkout(&self, req: GitCheckoutRequest) -> Result<(), String> {
        let work_dir = Path::new(&req.project_path);
        let mut args = vec!["checkout"];
        if req.create_if_missing {
            args.push("-B");
        }
        args.push(&req.branch);

        let output = std::process::Command::new("git")
            .args(&args)
            .current_dir(work_dir)
            .output()
            .map_err(|e| format!("Failed to execute git checkout: {e}"))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git checkout failed: {err}"));
        }

        Ok(())
    }
}
