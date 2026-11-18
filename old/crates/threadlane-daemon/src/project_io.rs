//! Project filesystem and Git operations served to clients (protocol v3).
//!
//! Every `SessionCommand` in the project-io surface resolves here:
//! `GitRequest` operations map onto `threadlane_git` calls, file commands
//! onto `threadlane_project::files` (with daemon-side path confinement),
//! and `WatchProject`/`UnwatchProject` manage a refcounted
//! [`WorkspaceWatcher`] per project root that feeds the ephemeral
//! `SessionEvent::WorkspaceChanged` stream.
//!
//! Everything is blocking (`git` subprocesses, `notify`, `fs`) — callers
//! run it inside `spawn_blocking` on the shared reactor.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use threadlane_protocol::daemon::SessionEvent;
use threadlane_protocol::repo::{
    CheckoutMode, GitActionOutcome, GitHubOperation, GitHubResponse, GitOperation, GitResponse,
};
use threadlane_project::watcher::{WorkspaceChangeEvent, WorkspaceWatcher};
use tokio::sync::mpsc;

/// Debounce matching the previous client-side watcher cadence.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(200);

/// One active watcher; refcounted so several clients can watch one root.
struct WatchEntry {
    count: usize,
    #[allow(dead_code)]
    watcher: WorkspaceWatcher,
}

/// `WatchProject`/`UnwatchProject` bookkeeping. Entries are removed when
/// the last subscriber unwatches, which also stops the notify worker.
#[derive(Default)]
pub struct ProjectWatchers {
    entries: Mutex<HashMap<PathBuf, WatchEntry>>,
}

impl ProjectWatchers {
    /// Start (or bump) the workspace watcher for `work_dir`; each change
    /// batch is ingested as a `WorkspaceChanged` event addressed to the
    /// daemon, which fans it out to all subscribers — watchers serve the
    /// host's filesystem regardless of which client asked first.
    pub fn watch(
        &self,
        work_dir: PathBuf,
        ingest_tx: mpsc::UnboundedSender<SessionEvent>,
    ) -> Result<(), String> {
        let mut entries = self.entries.lock().expect("project watchers poisoned");
        if let Some(entry) = entries.get_mut(&work_dir) {
            entry.count += 1;
            return Ok(());
        }
        let root = work_dir.clone();
        let watcher = WorkspaceWatcher::start(
            root.clone(),
            WATCH_DEBOUNCE,
            move |change: WorkspaceChangeEvent| {
                let _ = ingest_tx.send(SessionEvent::WorkspaceChanged {
                    work_dir: root.clone(),
                    git_dirty: change.git_dirty,
                    files_dirty: change.files_dirty,
                });
            },
        )
        .map_err(|error| error.to_string())?;
        entries.insert(work_dir, WatchEntry { count: 1, watcher });
        Ok(())
    }

    /// Release one watch on `work_dir`; dropping the last entry stops the
    /// underlying `notify` watcher and its debounce worker.
    pub fn unwatch(&self, work_dir: &Path) -> Result<(), String> {
        let mut entries = self.entries.lock().expect("project watchers poisoned");
        if let Some(entry) = entries.get_mut(work_dir) {
            entry.count = entry.count.saturating_sub(1);
            if entry.count == 0 {
                entries.remove(work_dir);
            }
        }
        Ok(())
    }
}

/// Run `action`, then re-inspect the repository and bundle both into a
/// [`GitResponse::Action`]. Mirrors the panel's action-then-inspect flow: a
/// failed action still reports the freshest status it could get.
fn action_then_inspect(
    work_dir: &Path,
    action: impl FnOnce() -> Result<Option<String>, String>,
) -> GitResponse {
    let action_result = action();
    let status = threadlane_git::inspect(work_dir).map_err(|error| error.to_string());
    GitResponse::Action {
        outcome: GitActionOutcome {
            action_error: action_result.as_ref().err().cloned(),
            message: action_result.ok().flatten(),
            status,
        },
    }
}

fn inspect_status(work_dir: &Path) -> Result<GitResponse, String> {
    threadlane_git::inspect(work_dir)
        .map(|status| GitResponse::Status {
            status: Box::new(status),
        })
        .map_err(|error| error.to_string())
}

/// Execute one [`GitOperation`] against `work_dir`. Read-only operations
/// answer their own payload variant; every mutation returns
/// [`GitResponse::Action`] via [`action_then_inspect`].
pub fn run_git_operation(work_dir: &Path, operation: &GitOperation) -> Result<GitResponse, String> {
    let git_error = |error: threadlane_git::GitError| error.to_string();
    let response = match operation {
        GitOperation::Inspect { sync_remote } => {
            if *sync_remote {
                let _ = threadlane_git::sync_remote(work_dir);
            }
            inspect_status(work_dir)?
        }
        GitOperation::InspectPrForBranch { branch } => GitResponse::Pr {
            pr: threadlane_git::inspect_pr_for_branch(work_dir, branch).map_err(git_error)?,
        },
        GitOperation::DiffFile { path, options } => GitResponse::Text {
            text: threadlane_git::diff_file_with_options(work_dir, path, *options)
                .map_err(git_error)?,
        },
        GitOperation::DiffFiles { paths, options } => {
            let mut joined = String::new();
            for path in paths {
                let Ok(diff) = threadlane_git::diff_file_with_options(work_dir, path, *options)
                else {
                    continue;
                };
                if diff.is_empty() {
                    continue;
                }
                if !joined.is_empty() {
                    joined.push('\n');
                }
                joined.push_str(&diff);
            }
            GitResponse::Text { text: joined }
        }
        GitOperation::DiffWorktree { options } => GitResponse::Text {
            text: threadlane_git::worktree_diff_with_options(work_dir, *options)
                .map_err(git_error)?,
        },
        GitOperation::CommitMessageDiff => GitResponse::Text {
            text: threadlane_git::commit_message_diff(work_dir).map_err(git_error)?,
        },
        GitOperation::DraftPrDiff { base } => GitResponse::Text {
            text: threadlane_git::draft_pr_diff(work_dir, base).map_err(git_error)?,
        },
        GitOperation::DiffBranch { branch } => GitResponse::Text {
            text: threadlane_git::diff_branch(work_dir, branch).map_err(git_error)?,
        },
        GitOperation::StashFiles { index } => GitResponse::Files {
            files: threadlane_git::inspect_stash_files(work_dir, *index),
        },
        GitOperation::DiffStashFile { index, path } => GitResponse::Text {
            text: threadlane_git::diff_stash_file(work_dir, *index, path).map_err(git_error)?,
        },
        GitOperation::CommitFiles { sha } => GitResponse::Files {
            files: threadlane_git::inspect_commit_files(work_dir, sha),
        },
        GitOperation::DiffCommitFile { sha, path } => GitResponse::Text {
            text: threadlane_git::diff_commit_file(work_dir, sha, path).map_err(git_error)?,
        },
        GitOperation::FileInventory => GitResponse::Inventory {
            inventory: threadlane_git::list_project_files(work_dir).map_err(|error| {
                match error {
                    threadlane_git::FileInventoryError::NotARepository => {
                        threadlane_protocol::repo::FILE_INVENTORY_NOT_A_REPOSITORY.to_string()
                    }
                    threadlane_git::FileInventoryError::Failed(git_error) => {
                        git_error.to_string()
                    }
                }
            })?,
        },
        GitOperation::IsRepo => GitResponse::Bool {
            value: threadlane_git::is_git_repo(work_dir),
        },

        GitOperation::Commit {
            message,
            selected_paths,
            push,
        } => action_then_inspect(work_dir, || {
            let status = threadlane_git::inspect(work_dir).map_err(git_error)?;
            let selected: HashSet<&str> =
                selected_paths.iter().map(String::as_str).collect();
            for file in &status.files {
                if selected.contains(file.path.as_str()) {
                    threadlane_git::stage_file(work_dir, &file.path).map_err(git_error)?;
                } else {
                    let _ = threadlane_git::unstage_file(work_dir, &file.path);
                }
            }
            threadlane_git::commit_staged(work_dir, message).map_err(git_error)?;
            if *push {
                threadlane_git::push(work_dir).map_err(git_error)?;
            }
            Ok(None)
        }),
        GitOperation::Push => {
            action_then_inspect(work_dir, || threadlane_git::push(work_dir).map(|_| None).map_err(git_error))
        }
        GitOperation::Pull => {
            action_then_inspect(work_dir, || threadlane_git::pull(work_dir).map(Some).map_err(git_error))
        }
        GitOperation::Fetch => {
            action_then_inspect(work_dir, || threadlane_git::fetch(work_dir).map(|_| None).map_err(git_error))
        }
        GitOperation::Stage { paths } => action_then_inspect(work_dir, || {
            if paths.len() == 1 {
                threadlane_git::stage_file(work_dir, &paths[0]).map_err(git_error)?;
                Ok(Some(format!("Staged {}", paths[0])))
            } else {
                threadlane_git::stage_files(work_dir, paths).map_err(git_error)?;
                Ok(Some(format!("Staged {} files", paths.len())))
            }
        }),
        GitOperation::Unstage { paths } => action_then_inspect(work_dir, || {
            if paths.len() == 1 {
                threadlane_git::unstage_file(work_dir, &paths[0]).map_err(git_error)?;
                Ok(Some(format!("Unstaged {}", paths[0])))
            } else {
                threadlane_git::unstage_files(work_dir, paths).map_err(git_error)?;
                Ok(Some(format!("Unstaged {} files", paths.len())))
            }
        }),
        GitOperation::StageAll => {
            action_then_inspect(work_dir, || threadlane_git::stage_all(work_dir).map(|_| None).map_err(git_error))
        }
        GitOperation::UnstageAll => {
            action_then_inspect(work_dir, || threadlane_git::unstage_all(work_dir).map(|_| None).map_err(git_error))
        }
        GitOperation::CreatePullRequest => action_then_inspect(work_dir, || {
            let pr = threadlane_git::create_pull_request(work_dir).map_err(git_error)?;
            Ok(Some(if pr.is_empty() {
                "Pull request created successfully.".into()
            } else {
                format!("Pull request created: {pr}")
            }))
        }),
        GitOperation::CreateDraftPullRequest { base, title, body } => {
            action_then_inspect(work_dir, || {
                // The dialog reads the created URL back out of `message`;
                // keep it bare rather than a display sentence.
                let url = threadlane_git::create_draft_pull_request(work_dir, base, title, body)
                    .map_err(git_error)?;
                Ok(Some(url))
            })
        }
        GitOperation::Checkout { branch, mode } => action_then_inspect(work_dir, || {
            match mode {
                CheckoutMode::Clean => threadlane_git::checkout(work_dir, branch),
                CheckoutMode::Stash => threadlane_git::checkout_with_stash(work_dir, branch),
                CheckoutMode::Carry => {
                    threadlane_git::checkout_carrying_changes(work_dir, branch)
                }
            }
            .map(|_| None)
            .map_err(git_error)
        }),
        GitOperation::CreateBranch { name } => action_then_inspect(work_dir, || {
            threadlane_git::create_branch(work_dir, name)
                .map(|_| None)
                .map_err(git_error)
        }),
        GitOperation::DeleteBranch { branch, force } => action_then_inspect(work_dir, || {
            threadlane_git::delete_branch(work_dir, branch, *force).map_err(git_error)?;
            Ok(Some(format!("Deleted local branch {branch}")))
        }),
        GitOperation::Merge { branch } => action_then_inspect(work_dir, || {
            threadlane_git::merge(work_dir, branch).map(Some).map_err(git_error)
        }),
        GitOperation::StashPush {
            message,
            include_untracked,
        } => action_then_inspect(work_dir, || {
            threadlane_git::stash_push(work_dir, message.as_deref(), *include_untracked)
                .map_err(git_error)?;
            Ok(Some("Stashed changes successfully".to_string()))
        }),
        GitOperation::PopStash { index } => action_then_inspect(work_dir, || {
            threadlane_git::pop_stash(work_dir, *index)
                .map(|_| None)
                .map_err(git_error)
        }),
        GitOperation::DropStash { index } => action_then_inspect(work_dir, || {
            threadlane_git::drop_stash(work_dir, *index)
                .map(|_| None)
                .map_err(git_error)
        }),
        GitOperation::Discard { paths } => action_then_inspect(work_dir, || {
            if paths.is_empty() {
                return Ok(None);
            }
            threadlane_git::discard_files(work_dir, paths).map_err(git_error)?;
            Ok(Some(if paths.len() == 1 {
                format!("Discarded changes in {}", paths[0])
            } else {
                format!("Discarded changes in {} files", paths.len())
            }))
        }),
        GitOperation::DiscardAll => action_then_inspect(work_dir, || {
            threadlane_git::discard_all_changes(work_dir).map_err(git_error)?;
            Ok(Some("Discarded all changes".to_string()))
        }),
        GitOperation::IgnoreFile { path } => action_then_inspect(work_dir, || {
            threadlane_git::ignore_file(work_dir, path)
                .map(|_| None)
                .map_err(git_error)
        }),
        GitOperation::IgnoreExtension { extension } => action_then_inspect(work_dir, || {
            threadlane_git::ignore_extension(work_dir, extension)
                .map(|_| None)
                .map_err(git_error)
        }),
        GitOperation::CreateWorktree { worktree, branch } => {
            action_then_inspect(work_dir, || {
                threadlane_git::create_worktree(work_dir, worktree, branch)
                    .map(|_| None)
                    .map_err(git_error)
            })
        }
        GitOperation::RemoveWorktree { worktree, force } => {
            action_then_inspect(work_dir, || {
                threadlane_git::remove_worktree(work_dir, worktree, *force)
                    .map_err(git_error)?;
                threadlane_tools::remove_worktree_cargo_target_dir(worktree);
                Ok(None)
            })
        }
        GitOperation::PruneWorktrees => action_then_inspect(work_dir, || {
            threadlane_git::prune_worktrees(work_dir)
                .map(|_| None)
                .map_err(git_error)
        }),
    };
    Ok(response)
}

/// Execute one [`GitHubOperation`] against `work_dir`. Issue/PR reads answer
/// their own payload variant; mutations answer [`GitHubResponse::Action`]
/// with the forge command's user-facing output when it produced any.
/// Everything runs through `gh`, so callers honor the host's configured
/// forge credentials — not the client's.
pub fn run_github_operation(
    work_dir: &Path,
    operation: &GitHubOperation,
) -> Result<GitHubResponse, String> {
    let git_error = |error: threadlane_git::GitError| error.to_string();
    let response = match operation {
        GitHubOperation::ListIssues {
            state,
            query,
            limit,
        } => GitHubResponse::Issues {
            issues: threadlane_git::list_github_issues(
                work_dir,
                state.as_str(),
                query.as_deref(),
                *limit,
            )
            .map_err(git_error)?,
        },
        GitHubOperation::InspectIssue { number } => GitHubResponse::Issue {
            detail: threadlane_git::inspect_github_issue(work_dir, *number).map_err(git_error)?,
        },
        GitHubOperation::CreateIssue { title, body } => GitHubResponse::Number {
            number: threadlane_git::create_github_issue(work_dir, title, body)
                .map_err(git_error)?,
        },
        GitHubOperation::CommentIssue { number, body } => GitHubResponse::Action {
            message: Some(
                threadlane_git::comment_on_github_issue(work_dir, *number, body)
                    .map_err(git_error)?,
            )
            .filter(|text| !text.is_empty()),
        },
        GitHubOperation::SetIssueState { number, close } => {
            threadlane_git::set_github_issue_state(work_dir, *number, *close)
                .map_err(git_error)?;
            GitHubResponse::Action {
                message: Some(if *close {
                    format!("Closed issue #{number}")
                } else {
                    format!("Reopened issue #{number}")
                }),
            }
        }
        GitHubOperation::DeleteIssue { number } => {
            threadlane_git::delete_github_issue(work_dir, *number).map_err(git_error)?;
            GitHubResponse::Action {
                message: Some(format!("Deleted issue #{number}")),
            }
        }
        GitHubOperation::ListPullRequests {
            state,
            query,
            limit,
        } => GitHubResponse::PullRequests {
            prs: threadlane_git::list_github_pull_requests(
                work_dir,
                state.as_str(),
                query.as_deref(),
                *limit,
            )
            .map_err(git_error)?,
        },
        GitHubOperation::InspectPullRequest { number } => GitHubResponse::PullRequest {
            pr: threadlane_git::inspect_pr_number(work_dir, *number).map_err(git_error)?,
        },
        GitHubOperation::PullRequestDiff { number } => GitHubResponse::Text {
            text: threadlane_git::pull_request_diff(work_dir, *number).map_err(git_error)?,
        },
        GitHubOperation::CommentPullRequest { number, body } => GitHubResponse::Action {
            message: Some(
                threadlane_git::comment_on_pull_request(work_dir, *number, body)
                    .map_err(git_error)?,
            )
            .filter(|text| !text.is_empty()),
        },
    };
    Ok(response)
}
