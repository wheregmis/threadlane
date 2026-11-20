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
use serde_json::Value as Json;
use threadlane_core::forge::{
    ApprovedBranch, CommentTarget, Forge, ForgeError, PullRequestUpdate, RepositoryAction,
};
use threadlane_core::github::{REVIEW_MARKER_PREFIX, normalize_remote};
use threadlane_core::model::Repository;
use threadlane_core::workspace::{valid_branch_name, valid_commit_id};
use tokio::process::Command;
use tokio::sync::OnceCell;

use crate::workspace::{check_fast_forward, git, run};

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
        let marker = format!("{REVIEW_MARKER_PREFIX}{delivery_key} -->");
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

fn pull_request_head<'a>(
    pull: &'a Json,
    repository: &Repository,
    name: &str,
) -> Result<&'a str, ForgeError> {
    if pull["state"] != "open" {
        return Err(ForgeError("the pull request must be open".into()));
    }
    for side in ["base", "head"] {
        if !pull[side]["repo"]["full_name"]
            .as_str()
            .is_some_and(|actual| actual.eq_ignore_ascii_case(name))
        {
            return Err(ForgeError(
                "the pull request must have both branches in the registered repository".into(),
            ));
        }
    }
    let head = pull["head"]["ref"]
        .as_str()
        .filter(|head| valid_branch_name(head))
        .ok_or_else(|| ForgeError("the pull request has no valid head branch".into()))?;
    if head == repository.default_branch
        || pull["head"]["repo"]["default_branch"] == head
        || pull["base"]["ref"] == head
        || repository
            .protected_branches
            .iter()
            .any(|branch| branch == head)
    {
        return Err(ForgeError(
            "the pull request head is a default, base, or protected branch".into(),
        ));
    }
    Ok(head)
}

async fn push_fast_forward(
    worktree: &Path,
    remote: &str,
    branch: &str,
    update: PullRequestUpdate<'_>,
) -> Result<(), ForgeError> {
    check_fast_forward(worktree, update.expected_head, update.revision)
        .await
        .map_err(|error| ForgeError(error.0))?;
    let target = format!("refs/heads/{branch}");
    let lease = format!("--force-with-lease={target}:{}", update.expected_head);
    let refspec = format!("{}:{target}", update.revision);
    // Ancestry is already checked. The lease guards against a concurrent ref change.
    git(
        Some(worktree),
        [
            "push",
            "--quiet",
            "--no-follow-tags",
            &lease,
            "--",
            remote,
            &refspec,
        ],
    )
    .await
    .map_err(|error| ForgeError(error.0))?;
    Ok(())
}

#[async_trait]
impl Forge for GitForge {
    async fn push(&self, worktree: &Path, branch: ApprovedBranch<'_>) -> Result<(), ForgeError> {
        check_branch(branch.name)?;
        if !valid_commit_id(branch.revision) {
            return Err(ForgeError("publish requires a full commit SHA".into()));
        }
        // A branch can move between the runner's check and Git resolving the refspec.
        let refspec = format!("{}^{{commit}}:refs/heads/{}", branch.revision, branch.name);
        git(
            Some(worktree),
            ["push", "--quiet", "origin", "--", refspec.as_str()],
        )
        .await
        .map_err(|error| ForgeError(error.0))?;
        Ok(())
    }

    async fn fast_forward_pull_request(
        &self,
        worktree: &Path,
        repository: &Repository,
        update: PullRequestUpdate<'_>,
    ) -> Result<String, ForgeError> {
        let remote = normalize_remote(&repository.remote_url)
            .ok_or_else(|| ForgeError("the registered remote is not a GitHub repository".into()))?;
        let (host, name) = remote
            .split_once('/')
            .ok_or_else(|| ForgeError("the registered remote has no repository name".into()))?;
        let hostname = format!("--hostname={host}");
        let endpoint = format!("repos/{name}/pulls/{}", update.number);
        let output = self
            .gh(
                worktree,
                args(&["api", &hostname, "--method=GET", &endpoint]),
            )
            .await?;
        let pull: Json = serde_json::from_str(&output)
            .map_err(|error| ForgeError(format!("invalid pull request response: {error}")))?;
        let head = pull_request_head(&pull, repository, name)?;
        let current = pull["head"]["sha"].as_str().unwrap_or_default();
        if current != update.revision {
            if current != update.expected_head {
                return Err(ForgeError(format!(
                    "PR #{} no longer has expected head {}. Inspect its current head before retrying.",
                    update.number, update.expected_head
                )));
            }
            push_fast_forward(worktree, &repository.remote_url, head, update).await?;
        }
        Ok(head.to_owned())
    }

    async fn open_draft_pull_request(
        &self,
        worktree: &Path,
        branch: ApprovedBranch<'_>,
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
                    "pr",
                    "create",
                    "--draft",
                    "--head",
                    branch.name,
                    "--base",
                    base,
                    &title,
                    &body,
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
        let repo = normalize_remote(remote)
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
            RepositoryAction::MarkReady { number } => args(&["pr", "ready", &number.to_string()]),
            RepositoryAction::GhCli { args } => args.iter().map(OsString::from).collect(),
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
    use threadlane_testkit::git::{bare_repository, git as local_git};

    #[test]
    fn a_pr_head_must_be_an_open_branch_in_the_registered_repository() {
        use serde_json::json;
        use threadlane_core::ids::{ProjectId, RepositoryId};

        let repository = Repository {
            id: RepositoryId(1),
            project_id: ProjectId(1),
            name: "app".into(),
            remote_url: "https://github.com/acme/app.git".into(),
            default_branch: "main".into(),
            protected_branches: vec!["reserved".into()],
        };
        let pull = json!({
            "state": "open",
            "base": {"ref": "main", "repo": {"full_name": "acme/app"}},
            "head": {"ref": "feature/pr", "repo": {"full_name": "Acme/App", "default_branch": "main"}}
        });
        assert_eq!(
            pull_request_head(&pull, &repository, "acme/app").unwrap(),
            "feature/pr"
        );
        for field in ["main", "reserved", "refs:other"] {
            let mut candidate = pull.clone();
            candidate["head"]["ref"] = json!(field);
            assert!(pull_request_head(&candidate, &repository, "acme/app").is_err());
        }
        let mut fork = pull.clone();
        fork["head"]["repo"]["full_name"] = json!("fork/app");
        assert!(pull_request_head(&fork, &repository, "acme/app").is_err());
        let mut closed = pull;
        closed["state"] = json!("closed");
        assert!(pull_request_head(&closed, &repository, "acme/app").is_err());
    }

    #[tokio::test]
    async fn a_fast_forward_requires_ancestry_and_the_exact_remote_head() {
        let dir = tempfile::tempdir().unwrap();
        let remote = bare_repository(dir.path(), "app");
        let seed = dir.path().join("app-seed");
        let remote_url = remote.to_str().unwrap();
        let initial = local_git(&seed, &["rev-parse", "HEAD"]);
        let initial = initial.trim();
        local_git(&remote, &["update-ref", "refs/heads/pr", initial]);
        local_git(
            &seed,
            &["commit", "--allow-empty", "-m", "Concurrent commit"],
        );
        let concurrent = local_git(&seed, &["rev-parse", "HEAD"]);
        let concurrent = concurrent.trim();
        local_git(
            &seed,
            &["commit", "--allow-empty", "-m", "Reviewed changes"],
        );
        let revision = local_git(&seed, &["rev-parse", "HEAD"]);
        let revision = revision.trim();
        let update = PullRequestUpdate {
            number: 1.try_into().unwrap(),
            expected_head: initial,
            revision,
        };

        push_fast_forward(&seed, remote_url, "pr", update)
            .await
            .unwrap();
        assert_eq!(local_git(&remote, &["rev-parse", "pr"]).trim(), revision);

        let backwards = PullRequestUpdate {
            expected_head: revision,
            revision: initial,
            ..update
        };
        assert!(
            push_fast_forward(&seed, remote_url, "pr", backwards)
                .await
                .is_err()
        );
        assert_eq!(local_git(&remote, &["rev-parse", "pr"]).trim(), revision);

        local_git(
            &seed,
            &[
                "push",
                remote_url,
                &format!("{concurrent}:refs/heads/advanced"),
            ],
        );
        assert!(
            push_fast_forward(&seed, remote_url, "advanced", update)
                .await
                .is_err()
        );
        assert_eq!(
            local_git(&remote, &["rev-parse", "advanced"]).trim(),
            concurrent
        );

        assert!(
            push_fast_forward(&seed, remote_url, "deleted", update)
                .await
                .is_err()
        );
        assert!(!local_git(&remote, &["branch", "--list"]).contains("deleted"));
    }

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
            let branch = ApprovedBranch {
                name: bad,
                revision: &"a".repeat(40),
            };
            assert!(forge.push(worktree, branch).await.is_err(), "{bad}");
            assert!(
                forge.find_pull_request(worktree, bad).await.is_err(),
                "{bad}"
            );
        }
        let opened = forge
            .open_draft_pull_request(
                worktree,
                ApprovedBranch {
                    name: "threadlane/T-0001",
                    revision: &"a".repeat(40),
                },
                "--force",
                "t",
                "b",
                "A, T-0001",
            )
            .await;
        assert!(opened.unwrap_err().0.contains("not a valid branch name"));
    }
}
