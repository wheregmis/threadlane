use std::path::Path;
use threadlane_git::{
    checkout as git_checkout, create_branch as git_create_branch, diff_file as git_diff_file,
    inspect, list_branches_detailed, normalize_branch_for_checkout, stage_file as git_stage_file,
    unstage_file as git_unstage_file,
};
use threadlane_protocol::capabilities::*;
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
        if !req.staged {
            if let Some(file) = req.file_path.as_deref() {
                return git_diff_file(work_dir, file)
                    .map(|diff| GitDiffResponse { diff })
                    .map_err(|e| e.to_string());
            }
        }

        let mut args = vec!["diff"];
        if req.staged {
            args.push("--cached");
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
                is_default: b.is_default,
                is_remote: b.is_remote,
                relative_time: b.relative_time,
                committer_date_unix: b.committer_date_unix,
                upstream: b.upstream,
            })
            .collect();

        Ok(GitBranchesResponse { branches })
    }

    pub fn checkout(&self, req: GitCheckoutRequest) -> Result<(), String> {
        let work_dir = Path::new(&req.project_path);
        if req.create_if_missing {
            let branch = normalize_branch_for_checkout(&req.branch);
            let exists = list_branches_detailed(work_dir, None)
                .map_err(|e| e.to_string())?
                .into_iter()
                .any(|candidate| !candidate.is_remote && candidate.name == branch);
            if !exists {
                return git_create_branch(work_dir, branch).map_err(|e| e.to_string());
            }
        }
        git_checkout(work_dir, &req.branch).map_err(|e| e.to_string())
    }

    // ── Extended git operations ────────────────────────────────────────────

    fn run_git(args: &[&str], work_dir: &Path) -> Result<String, String> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .output()
            .map_err(|e| format!("Failed to execute git {}: {e}", args[0]))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git {} failed: {err}", args[0]));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub fn stage_file(&self, req: GitStageFileRequest) -> Result<(), String> {
        let work_dir = Path::new(&req.project_path);
        if req.stage {
            git_stage_file(work_dir, &req.file_path).map_err(|e| e.to_string())?;
        } else {
            git_unstage_file(work_dir, &req.file_path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn commit(&self, req: GitCommitRequest) -> Result<GitCommitResponse, String> {
        let work_dir = Path::new(&req.project_path);
        threadlane_git::commit_staged(work_dir, &req.message).map_err(|e| e.to_string())?;
        let sha = Self::run_git(&["rev-parse", "HEAD"], work_dir)?;
        Ok(GitCommitResponse { sha })
    }

    pub fn push(&self, req: GitPushPullRequest) -> Result<(), String> {
        let work_dir = Path::new(&req.project_path);
        Self::run_git(&["push"], work_dir)?;
        Ok(())
    }

    pub fn pull(&self, req: GitPushPullRequest) -> Result<(), String> {
        let work_dir = Path::new(&req.project_path);
        Self::run_git(&["pull", "--rebase"], work_dir)?;
        Ok(())
    }

    pub fn discard_file(&self, req: GitDiscardFileRequest) -> Result<(), String> {
        let work_dir = Path::new(&req.project_path);
        Self::run_git(&["restore", "--", &req.file_path], work_dir)?;
        Ok(())
    }

    pub fn ignore(&self, req: GitIgnoreRequest) -> Result<(), String> {
        let gitignore = Path::new(&req.project_path).join(".gitignore");
        let mut content = std::fs::read_to_string(&gitignore).unwrap_or_default();
        if !content.lines().any(|l| l.trim() == req.pattern.trim()) {
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&req.pattern);
            content.push('\n');
            std::fs::write(&gitignore, content).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn merge(&self, req: GitMergeRequest) -> Result<(), String> {
        let work_dir = Path::new(&req.project_path);
        Self::run_git(&["merge", "--no-ff", &req.branch], work_dir)?;
        Ok(())
    }

    pub fn stash_pop(&self, req: GitStashActionRequest) -> Result<(), String> {
        let work_dir = Path::new(&req.project_path);
        if let Some(idx) = req.index {
            Self::run_git(&["stash", "pop", &format!("stash@{{{idx}}}")], work_dir)?;
        } else {
            Self::run_git(&["stash", "pop"], work_dir)?;
        }
        Ok(())
    }

    pub fn stash_drop(&self, req: GitStashActionRequest) -> Result<(), String> {
        let work_dir = Path::new(&req.project_path);
        if let Some(idx) = req.index {
            Self::run_git(&["stash", "drop", &format!("stash@{{{idx}}}")], work_dir)?;
        } else {
            Self::run_git(&["stash", "drop"], work_dir)?;
        }
        Ok(())
    }

    pub fn commit_diff_message(
        &self,
        project_path: &str,
    ) -> Result<GitCommitDiffMessageResponse, String> {
        let work_dir = Path::new(project_path);
        let diff = Self::run_git(&["diff", "--staged", "--stat"], work_dir)?;
        // Return the staged diff stat as a commit message seed.
        let message = if diff.is_empty() {
            String::new()
        } else {
            diff.lines().last().unwrap_or("").trim().to_string()
        };
        Ok(GitCommitDiffMessageResponse { message })
    }
}
