//! [`GitForge`]: pushes with `git` and opens draft pull requests with `gh`,
//! in a task's worktree (Plan.md §9.2, docs/milestone-3.md §1 item 4).
//!
//! Both run as subprocesses with explicit arguments, never through a shell,
//! each bounded by a timeout. The branch must be a plain branch name and is
//! passed after `--` where the command takes one; a title and a body are
//! passed bound to their option (`--title=...`), so they can never be read as
//! an option themselves. The system sets no credential and reads no token:
//! `git` and `gh` use the logins this user already has (invariant 8). They
//! are only kept from prompting.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use threadlane_core::forge::{CommentTarget, Forge, ForgeError, RepositoryAction};
use threadlane_core::workspace::valid_branch_name;
use tokio::process::Command;
use tokio::sync::OnceCell;

use crate::workspace::{git, run};

/// The longest one `gh` command may take.
const GH_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// [`Forge`] through `git` and `gh`.
#[derive(Debug, Clone)]
pub struct GitForge {
    gh: PathBuf,
    /// The `gh` user's login, looked up once; `None` if that failed.
    login: Arc<OnceCell<Option<String>>>,
}

impl Default for GitForge {
    /// Runs the `gh` found on `PATH`.
    fn default() -> Self {
        Self::with_gh("gh")
    }
}

impl GitForge {
    /// Runs `gh` from `program`.
    #[must_use]
    pub fn with_gh(program: impl Into<PathBuf>) -> Self {
        Self {
            gh: program.into(),
            login: Arc::default(),
        }
    }

    /// Posts a comment-only GitHub review on the submitted commit. The marker
    /// lets delivery resume after GitHub accepted a review but before local acknowledgement.
    pub async fn publish_review(
        &self,
        pull_url: &str,
        commit: &str,
        body: &str,
        delivery_key: &str,
    ) -> Result<(), ForgeError> {
        let (repo, number) = pull_url
            .strip_prefix("https://")
            .and_then(|path| path.rsplit_once("/pull/"))
            .ok_or_else(|| ForgeError("invalid pull request URL".into()))?;
        let (host, repository) = repo
            .split_once('/')
            .ok_or_else(|| ForgeError("missing repository".into()))?;
        let number: std::num::NonZeroU64 = number
            .parse()
            .map_err(|_| ForgeError("invalid pull request number".into()))?;
        if repository.split('/').count() != 2
            || !repo.split('/').all(|part| {
                !part.is_empty()
                    && !part.starts_with('.')
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
            })
        {
            return Err(ForgeError("invalid review repository".into()));
        }
        let endpoint = format!("repos/{repository}/pulls/{number}/reviews");
        let host = format!("--hostname={host}");
        let marker = format!("<!-- threadlane-review-{delivery_key} -->");
        let existing = self
            .gh(
                Path::new("."),
                args(&["api", &host, "--paginate", &endpoint, "--jq", ".[].body"]),
            )
            .await?;
        if existing.lines().any(|line| line == marker) {
            return Ok(());
        }
        self.gh(
            Path::new("."),
            args(&[
                "api",
                &host,
                "--method",
                "POST",
                &endpoint,
                "-f",
                "event=COMMENT",
                "-f",
                &format!("commit_id={commit}"),
                "-f",
                &format!("body={body}\n\n{marker}"),
            ]),
        )
        .await?;
        Ok(())
    }

    /// The `gh` user's login, from `gh api user` on first use. A failed or
    /// odd-looking lookup gives `None` and is not retried.
    async fn login(&self, worktree: &Path) -> Option<String> {
        self.login
            .get_or_init(|| async {
                let login = self
                    .gh(worktree, args(&["api", "user", "--jq", ".login"]))
                    .await
                    .ok()?;
                let login = login.trim();
                (!login.is_empty() && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
                    .then(|| login.to_owned())
            })
            .await
            .clone()
    }

    /// `gh <args>` in `worktree`, kept from prompting.
    fn gh_command(&self, worktree: &Path, args: &[OsString]) -> Command {
        let mut command = Command::new(&self.gh);
        command
            .args(args)
            .current_dir(worktree)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        command
    }

    async fn gh(&self, worktree: &Path, args: Vec<OsString>) -> Result<String, ForgeError> {
        run(self.gh_command(worktree, &args), "gh", &args, GH_TIMEOUT)
            .await
            .map_err(ForgeError)
    }
}

fn check_branch(branch: &str) -> Result<(), ForgeError> {
    if valid_branch_name(branch) {
        Ok(())
    } else {
        Err(ForgeError(format!("{branch:?} is not a valid branch name")))
    }
}

fn args(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

#[async_trait]
impl Forge for GitForge {
    async fn push(&self, worktree: &Path, branch: &str) -> Result<(), ForgeError> {
        check_branch(branch)?;
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        git(
            Some(worktree),
            ["push", "--quiet", "origin", "--", refspec.as_str()],
        )
        .await
        .map_err(|error| ForgeError(error.0))?;
        Ok(())
    }

    async fn open_draft_pull_request(
        &self,
        worktree: &Path,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
        opened_by: &str,
    ) -> Result<String, ForgeError> {
        check_branch(base)?;
        self.push(worktree, branch).await?;
        let title = format!("--title={title}");
        let body = match self.login(worktree).await {
            Some(login) => format!(
                "--body={}\n\n🤖 Opened by Threadlane ({opened_by}) acting for @{login}",
                body.trim_end()
            ),
            None => format!(
                "--body={}\n\n🤖 Opened by Threadlane ({opened_by})",
                body.trim_end()
            ),
        };
        let output = self
            .gh(
                worktree,
                args(&[
                    "pr", "create", "--draft", "--head", branch, "--base", base, &title, &body,
                ]),
            )
            .await?;
        output
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ForgeError("gh pr create printed no pull request URL".to_owned()))
    }

    async fn apply(&self, remote: &str, action: &RepositoryAction) -> Result<String, ForgeError> {
        let repo = threadlane_core::github::normalize_remote(remote)
            .ok_or_else(|| ForgeError("the registered remote is not a forge repository".into()))?;
        let mut argv = match action {
            RepositoryAction::Comment {
                target,
                number,
                body,
            } => args(&[
                match target {
                    CommentTarget::PullRequest => "pr",
                    CommentTarget::Issue => "issue",
                },
                "comment",
                &number.to_string(),
                &format!("--body={body}"),
            ]),
            RepositoryAction::CreateIssue { title, body } => args(&[
                "issue",
                "create",
                &format!("--title={title}"),
                &format!("--body={body}"),
            ]),
            RepositoryAction::Merge { number } => {
                args(&["pr", "merge", &number.to_string(), "--merge"])
            }
        };
        argv.push(format!("--repo={repo}").into());
        self.gh(Path::new("."), argv).await
    }

    /// `gh auth status` succeeds. Only its exit status is read: its output
    /// can show part of the token.
    async fn logged_in(&self) -> bool {
        let mut command = Command::new(&self.gh);
        command
            .args(["auth", "status"])
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        matches!(
            tokio::time::timeout(GH_TIMEOUT, command.status()).await,
            Ok(Ok(status)) if status.success()
        )
    }

    async fn find_pull_request(
        &self,
        worktree: &Path,
        branch: &str,
    ) -> Result<Option<String>, ForgeError> {
        check_branch(branch)?;
        let output = self
            .gh(
                worktree,
                args(&[
                    "pr", "list", "--head", branch, "--state", "all", "--limit", "1", "--json",
                    "url", "--jq", ".[].url",
                ]),
            )
            .await?;
        Ok(output
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use crate::workspace::git_command;

    /// The environment a command sets on top of what it inherits.
    fn set_env(command: &Command) -> Vec<(String, String)> {
        command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value
                        .map(OsStr::to_string_lossy)
                        .unwrap_or_default()
                        .into_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn the_forge_only_invokes_git_and_gh_and_sets_no_credentials() {
        let forge = GitForge::default();
        let gh = forge.gh_command(Path::new("/work"), &args(&["pr", "list"]));
        let git = git_command(Some(Path::new("/work")), &args(&["push"]));
        assert_eq!(gh.as_std().get_program(), "gh");
        assert_eq!(git.as_std().get_program(), "git");
        for command in [&gh, &git] {
            for (key, value) in set_env(command) {
                assert!(
                    matches!(key.as_str(), "GH_PROMPT_DISABLED" | "GIT_TERMINAL_PROMPT"),
                    "sets only what keeps it from prompting, not {key}={value}"
                );
                let upper = key.to_ascii_uppercase();
                for credential in ["TOKEN", "PASSWORD", "ASKPASS", "CREDENTIAL", "AUTH"] {
                    assert!(!upper.contains(credential), "{key}");
                }
            }
        }
    }

    #[tokio::test]
    async fn a_branch_git_could_misread_is_refused_before_anything_runs() {
        let forge = GitForge::with_gh("/nonexistent/gh");
        let worktree = Path::new("/nonexistent");
        for bad in ["--force", "-f", "a..b", "main:other"] {
            assert!(forge.push(worktree, bad).await.is_err(), "{bad}");
            assert!(
                forge.find_pull_request(worktree, bad).await.is_err(),
                "{bad}"
            );
        }
        let opened = forge
            .open_draft_pull_request(
                worktree,
                "threadlane/T-0001",
                "--force",
                "t",
                "b",
                "A, T-0001",
            )
            .await;
        assert!(opened.unwrap_err().0.contains("not a valid branch name"));
    }
}
