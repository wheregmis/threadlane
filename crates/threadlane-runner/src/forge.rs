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
    ApprovedBranch, CommentTarget, Forge, ForgeError, PullRequestImage, PullRequestUpdate,
    RepositoryAction,
};
use threadlane_core::github::{REVIEW_MARKER_PREFIX, normalize_remote};
use threadlane_core::model::Repository;
use threadlane_core::workspace::{valid_branch_name, valid_commit_id};
use tokio::process::Command;
use tokio::sync::OnceCell;

use crate::workspace::{COMMITTER, check_fast_forward, git, git_command, run};

/// The longest one `gh` command may take.
const GH_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// [`Forge`] through `git` and `gh`.
#[derive(Debug, Clone)]
pub struct GitForge {
    gh: PathBuf,
    /// The `git` that deletes a closed pull request's branch.
    git: PathBuf,
    /// The `gh` user's login, looked up once; `None` if that failed.
    login: Arc<OnceCell<Option<String>>>,
    /// The wait between polls of an asynchronous merge.
    merge_poll: Duration,
}

/// What a merge call reported, each with its report.
enum Merge {
    /// The asynchronous merge API answered that the PR merged.
    Confirmed(String),
    /// `gh pr merge` succeeded, which a merge queue or auto-merge also
    /// reports for a PR it only queued.
    Requested(String),
}

impl Merge {
    fn into_report(self) -> String {
        match self {
            Self::Confirmed(report) | Self::Requested(report) => report,
        }
    }
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
            git: "git".into(),
            login: Arc::default(),
            merge_poll: Duration::from_secs(2),
        }
    }

    /// Polls an asynchronous merge every `interval`.
    #[must_use]
    pub fn with_merge_poll(mut self, interval: Duration) -> Self {
        self.merge_poll = interval;
        self
    }

    /// Deletes branches with `git` from `program`.
    #[must_use]
    pub fn with_git(mut self, program: impl Into<PathBuf>) -> Self {
        self.git = program.into();
        self
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

    /// PR `number` of `repo` (`host/owner/name`), or an error unless its head is
    /// still `expected_head`: an approval given for an older head performs nothing.
    async fn pull_at_head(
        &self,
        repo: &str,
        number: std::num::NonZeroU64,
        expected_head: &str,
    ) -> Result<Json, ForgeError> {
        let (host, name) = repo
            .split_once('/')
            .ok_or_else(|| ForgeError("the registered remote has no repository name".into()))?;
        let output = self
            .gh(
                Path::new("."),
                args(&[
                    "api",
                    &format!("--hostname={host}"),
                    "--method=GET",
                    &format!("repos/{name}/pulls/{number}"),
                ]),
            )
            .await?;
        let pull: Json = serde_json::from_str(&output)
            .map_err(|error| ForgeError(format!("invalid pull request response: {error}")))?;
        if pull["head"]["sha"].as_str() != Some(expected_head) {
            return Err(ForgeError(format!(
                "PR #{number} no longer has expected head {expected_head}. Inspect its current head before retrying."
            )));
        }
        Ok(pull)
    }

    /// An error unless `expected_head` contains `fixed_by`.
    async fn check_contains(
        &self,
        hostname: &str,
        name: &str,
        number: std::num::NonZeroU64,
        expected_head: &str,
        fixed_by: &str,
    ) -> Result<(), ForgeError> {
        let compare = self
            .gh(
                Path::new("."),
                args(&[
                    "api",
                    hostname,
                    "--method=GET",
                    &format!("repos/{name}/compare/{fixed_by}...{expected_head}"),
                ]),
            )
            .await?;
        let compare: Json = serde_json::from_str(&compare)
            .map_err(|error| ForgeError(format!("invalid compare response: {error}")))?;
        if !matches!(compare["status"].as_str(), Some("ahead" | "identical")) {
            return Err(ForgeError(format!(
                "PR #{number}'s head {expected_head} does not contain {fixed_by}."
            )));
        }
        Ok(())
    }

    /// Replies on and resolves review thread `thread_id` of PR `number`, only
    /// while the PR head is `expected_head` and contains `fixed_by` (or, without
    /// it, as declined), and only a thread of that PR. An already resolved thread is left alone, so a retry finishes.
    async fn resolve_review_thread(
        &self,
        repo: &str,
        number: std::num::NonZeroU64,
        expected_head: &str,
        thread_id: &str,
        fixed_by: Option<&str>,
        body: &str,
    ) -> Result<String, ForgeError> {
        self.pull_at_head(repo, number, expected_head).await?;
        let (host, name) = repo
            .split_once('/')
            .ok_or_else(|| ForgeError("the registered remote has no repository name".into()))?;
        let hostname = format!("--hostname={host}");
        let here = Path::new(".");
        if let Some(fixed_by) = fixed_by {
            self.check_contains(&hostname, name, number, expected_head, fixed_by)
                .await?;
        }
        let id = format!("id={thread_id}");
        let thread = self
            .gh(
                here,
                args(&[
                    "api",
                    &hostname,
                    "graphql",
                    "-f",
                    "query=query($id:ID!){node(id:$id){... on PullRequestReviewThread{isResolved pullRequest{number repository{nameWithOwner}}}}}",
                    "-f",
                    &id,
                ]),
            )
            .await?;
        let thread: Json = serde_json::from_str(&thread)
            .map_err(|error| ForgeError(format!("invalid review thread response: {error}")))?;
        let node = &thread["data"]["node"];
        let on_this_pull = node["pullRequest"]["number"].as_u64() == Some(number.get())
            && node["pullRequest"]["repository"]["nameWithOwner"]
                .as_str()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(name));
        if !on_this_pull {
            return Err(ForgeError(format!(
                "{thread_id} is not a review thread of PR #{number}."
            )));
        }
        if node["isResolved"] == true {
            return Ok(format!("Review thread {thread_id} was already resolved."));
        }
        // GraphQL runs the two mutations in order, so the reply lands before the resolve.
        self.gh(
            here,
            args(&[
                "api",
                &hostname,
                "graphql",
                "-f",
                "query=mutation($id:ID!,$body:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$id,body:$body}){clientMutationId} resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}",
                "-f",
                &id,
                "-f",
                &match fixed_by {
                    Some(fixed_by) => format!("body={}\n\nFixed in {fixed_by}.", body.trim_end()),
                    None => format!("body={}\n\nDeclined: not changed.", body.trim_end()),
                },
            ]),
        )
        .await?;
        Ok(match fixed_by {
            Some(fixed_by) => {
                format!("Resolved review thread {thread_id} on PR #{number}, fixed in {fixed_by}.")
            }
            None => format!("Resolved review thread {thread_id} on PR #{number} as declined."),
        })
    }

    /// Re-requests `reviewers` on PR `number` and posts `comment`, only while
    /// its head is `expected_head`.
    async fn request_review(
        &self,
        repo: &str,
        number: std::num::NonZeroU64,
        expected_head: &str,
        reviewers: &[String],
        comment: Option<&str>,
    ) -> Result<String, ForgeError> {
        self.pull_at_head(repo, number, expected_head).await?;
        let (host, name) = repo
            .split_once('/')
            .ok_or_else(|| ForgeError("the registered remote has no repository name".into()))?;
        let hostname = format!("--hostname={host}");
        let here = Path::new(".");
        if !reviewers.is_empty() {
            let mut argv = args(&[
                "api",
                &hostname,
                "--method=POST",
                &format!("repos/{name}/pulls/{number}/requested_reviewers"),
            ]);
            for reviewer in reviewers {
                argv.push("-f".into());
                argv.push(format!("reviewers[]={reviewer}").into());
            }
            self.gh(here, argv).await?;
        }
        if let Some(comment) = comment {
            self.gh(
                here,
                args(&[
                    "api",
                    &hostname,
                    "--method=POST",
                    &format!("repos/{name}/issues/{number}/comments"),
                    "-f",
                    &format!("body={comment}"),
                ]),
            )
            .await?;
        }
        Ok(format!(
            "Requested a fresh review of PR #{number} at {expected_head}."
        ))
    }

    /// Merges PR `number` of `repo` (`host/owner/name`) only at `head`. A PR in a
    /// GitHub stack refuses `gh pr merge`, so it goes through the asynchronous
    /// merge REST API, pinned to the same head, and waits for its result.
    /// That API also merges the open PRs below it in the stack, so in a
    /// `batch` it is refused while one is open: those would skip their checks.
    async fn merge_pull(
        &self,
        repo: &str,
        number: std::num::NonZeroU64,
        head: &str,
        batch: bool,
    ) -> Result<Merge, ForgeError> {
        let error = match self
            .gh(
                Path::new("."),
                args(&[
                    "pr",
                    "merge",
                    &number.to_string(),
                    "--merge",
                    &format!("--match-head-commit={head}"),
                    &format!("--repo={repo}"),
                ]),
            )
            .await
        {
            Err(error) if error.0.contains("part of a stack") => error,
            result => return result.map(Merge::Requested),
        };
        if batch {
            let pr = |argv: &[&str]| self.gh(Path::new("."), args(argv));
            let repo_flag = format!("--repo={repo}");
            let base = pr(&[
                "pr",
                "view",
                &number.to_string(),
                "--json=baseRefName",
                "--jq=.baseRefName",
                &repo_flag,
            ])
            .await?;
            let below = pr(&[
                "pr",
                "list",
                &format!("--head={}", base.trim()),
                "--state=open",
                "--json=number",
                "--jq=.[].number",
                &repo_flag,
            ])
            .await?;
            let below: Vec<_> = below.split_whitespace().map(|n| format!("#{n}")).collect();
            if !below.is_empty() {
                return Err(ForgeError(format!(
                    "{}; the asynchronous merge would also merge open downstack PR {} without \
                     the batch's head checks, so merge the stack from the bottom",
                    error.0,
                    below.join(", ")
                )));
            }
        }
        let (host, name) = repo
            .split_once('/')
            .ok_or_else(|| ForgeError("the registered remote has no repository name".into()))?;
        let endpoint = format!("repos/{name}/pulls/{number}/merge-async");
        let api = |method: &str, extra: &[String], endpoint: String| {
            let mut argv = args(&[
                "api",
                &format!("--hostname={host}"),
                &format!("--method={method}"),
            ]);
            argv.extend(extra.iter().map(OsString::from));
            argv.push(endpoint.into());
            self.gh(Path::new("."), argv)
        };
        let fields = [
            "-f".to_owned(),
            format!("sha={head}"),
            "-f".to_owned(),
            "merge_method=merge".to_owned(),
            "-f".to_owned(),
            "merge_action=direct_merge".to_owned(),
        ];
        let mut output = api("PUT", &fields, endpoint.clone())
            .await
            .map_err(|async_error| {
                ForgeError(format!(
                    "{}; the asynchronous merge also failed: {}",
                    error.0, async_error.0
                ))
            })?;
        // shortcut: polls for about a minute, then reports it still pending; raise if stacks merge slower.
        for poll in 0..=30 {
            let result: Json = serde_json::from_str(&output)
                .map_err(|error| ForgeError(format!("invalid merge response: {error}")))?;
            let message = result["details"]["message"].as_str().unwrap_or_default();
            match result["status"].as_str() {
                Some("merged") => {
                    let sha = result["details"]["sha"].as_str().unwrap_or("unknown");
                    return Ok(Merge::Confirmed(format!(
                        "Merged PR #{number} at {head} through the asynchronous merge API \
                         (part of a stack); merge commit {sha}."
                    )));
                }
                Some("pending") => {
                    let uuid = result["details"]["uuid"].as_str().ok_or_else(|| {
                        ForgeError("the pending merge response has no uuid".into())
                    })?;
                    if poll == 30 {
                        break;
                    }
                    tokio::time::sleep(self.merge_poll).await;
                    output = api("GET", &[], format!("{endpoint}/{uuid}")).await?;
                }
                status => {
                    return Err(ForgeError(format!(
                        "the asynchronous merge of PR #{number} ended {}: {message}",
                        status.unwrap_or("without a status")
                    )));
                }
            }
        }
        Err(ForgeError(format!(
            "the asynchronous merge of PR #{number} is still pending; check the PR before retrying"
        )))
    }

    /// Merges `pulls` in order, each only if GitHub still sees its pinned
    /// head, and stops at the first that fails or that GitHub does not then
    /// report merged, saying which merged.
    async fn merge_batch(
        &self,
        repo: &str,
        pulls: &[(std::num::NonZeroU64, String)],
    ) -> Result<String, ForgeError> {
        let mut merged = Vec::new();
        for (number, head) in pulls {
            let merge = self.merge_pull(repo, *number, head, true).await;
            // With a merge queue or auto-merge, `gh pr merge` succeeds having only queued the PR.
            let merge = match merge {
                Ok(Merge::Confirmed(_)) => Ok(()),
                Ok(Merge::Requested(_)) => self
                    .gh(
                        Path::new("."),
                        args(&[
                            "pr",
                            "view",
                            &number.to_string(),
                            "--json=state",
                            "--jq=.state",
                            &format!("--repo={repo}"),
                        ]),
                    )
                    .await
                    .and_then(|state| match state.trim() {
                        "MERGED" => Ok(()),
                        state => Err(ForgeError(format!(
                            "GitHub reports it {state} after the merge, so it was queued \
                             (merge queue or auto-merge), not merged"
                        ))),
                    }),
                Err(error) => Err(error),
            };
            if let Err(error) = merge {
                let done = if merged.is_empty() {
                    "none merged".to_owned()
                } else {
                    format!("merged {}", merged.join(", "))
                };
                return Err(ForgeError(format!(
                    "batch stopped at #{number} (expected head {head}), {done}, the rest not \
                     attempted: {}",
                    error.0
                )));
            }
            merged.push(format!("#{number}"));
        }
        Ok(format!("merged {} in order", merged.join(", ")))
    }

    /// Marks each draft PR ready in order, stopping at the first that fails.
    async fn mark_ready_batch(
        &self,
        repo: &str,
        numbers: &[std::num::NonZeroU64],
    ) -> Result<String, ForgeError> {
        let mut ready = Vec::new();
        for number in numbers {
            let call = args(&[
                "pr",
                "ready",
                &number.to_string(),
                &format!("--repo={repo}"),
            ]);
            if let Err(error) = self.gh(Path::new("."), call).await {
                let done = if ready.is_empty() {
                    "none marked ready".to_owned()
                } else {
                    format!("marked {} ready", ready.join(", "))
                };
                return Err(ForgeError(format!(
                    "batch stopped at #{number}, {done}, the rest not attempted: {}",
                    error.0
                )));
            }
            ready.push(format!("#{number}"));
        }
        Ok(format!("marked {} ready in order", ready.join(", ")))
    }

    /// Closes PR `number` of `repo` (`host/owner/name`) only while its head
    /// is `expected_head`, then deletes its head branch if asked. A PR that is
    /// already closed at that head is not closed again, so a retry finishes.
    async fn close_pull_request(
        &self,
        remote: &str,
        repo: &str,
        number: std::num::NonZeroU64,
        expected_head: &str,
        delete_branch: bool,
    ) -> Result<String, ForgeError> {
        let (host, name) = repo
            .split_once('/')
            .ok_or_else(|| ForgeError("the registered remote has no repository name".into()))?;
        let hostname = format!("--hostname={host}");
        let endpoint = format!("repos/{name}/pulls/{number}");
        let here = Path::new(".");
        let pull = self.pull_at_head(repo, number, expected_head).await?;
        let branch = match delete_branch {
            // GitHub nulls the head repository once the head branch is gone.
            true if pull["head"]["repo"].is_null() => None,
            true => Some(closable_head(&pull, name)?),
            false => None,
        };
        let closed = if pull["merged"] == true {
            format!("Pull request #{number} was already merged")
        } else {
            format!("Closed pull request #{number}")
        };
        // shortcut: GitHub cannot pin a close to a head SHA, so a push in the moment between the
        // check above and the PATCH below is not caught; the branch deletion is pinned by a lease.
        if pull["state"] == "open" {
            self.gh(
                here,
                args(&[
                    "api",
                    &hostname,
                    "--method=PATCH",
                    &endpoint,
                    "-f",
                    "state=closed",
                ]),
            )
            .await?;
        }
        let Some(branch) = branch else {
            return Ok(format!("{closed}."));
        };
        // A closed PR's head SHA is frozen, so check the live branch: it may have been reused.
        let live_ref = format!("repos/{name}/git/ref/heads/{branch}");
        match self
            .gh(here, args(&["api", &hostname, "--method=GET", &live_ref]))
            .await
        {
            Err(error) if error.0.contains("(HTTP 404)") => {
                return Ok(format!(
                    "{closed}; its branch {branch} was already deleted."
                ));
            }
            Err(error) => return Err(error),
            Ok(output) => {
                let live: Json = serde_json::from_str(&output)
                    .map_err(|error| ForgeError(format!("invalid branch response: {error}")))?;
                if live["object"]["sha"].as_str() != Some(expected_head) {
                    return Ok(kept(&closed, branch, expected_head));
                }
            }
        }
        // The lease makes the deletion atomic: Git refuses it if the branch moved since the check.
        match self.delete_branch(remote, branch, expected_head).await {
            Ok(()) => {}
            Err(error) if error.0.contains("stale info") => {
                return Ok(kept(&closed, branch, expected_head));
            }
            Err(error) => return Err(error),
        }
        Ok(format!("{closed} and deleted its branch {branch}."))
    }

    /// Deletes `branch` on `remote` with `git push --force-with-lease`, so only
    /// while it still points at `expected_head`. `git push` needs a repository
    /// to run in, so it runs in an empty one made for the call.
    async fn delete_branch(
        &self,
        remote: &str,
        branch: &str,
        expected_head: &str,
    ) -> Result<(), ForgeError> {
        static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let scratch =
            std::env::temp_dir().join(format!("threadlane-delete-{}-{call}", std::process::id()));
        let target = format!("refs/heads/{branch}");
        let lease = format!("--force-with-lease={target}:{expected_head}");
        let git = |argv: Vec<OsString>| {
            let mut command = Command::new(&self.git);
            command.args(&argv).env("GIT_TERMINAL_PROMPT", "0");
            async move {
                run(command, "git", &argv, GH_TIMEOUT)
                    .await
                    .map_err(ForgeError)
            }
        };
        let mut init = args(&["init", "--bare", "--quiet", "--"]);
        init.push(scratch.clone().into());
        let result = match git(init).await {
            Ok(_) => {
                let mut push = vec![OsString::from("-C"), scratch.clone().into()];
                push.extend(args(&[
                    "push", "--quiet", &lease, "--delete", "--", remote, &target,
                ]));
                git(push).await.map(drop)
            }
            Err(error) => Err(error),
        };
        let _ = tokio::fs::remove_dir_all(&scratch).await;
        result
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

    /// Labels an issue through `gh api`. Refuses a pull request, adds only
    /// labels the repository already has (the API would create the others),
    /// and treats removing a label the issue lacks as done.
    async fn label_issue(
        &self,
        repo: &str,
        number: std::num::NonZeroU64,
        add: &[String],
        remove: &[String],
    ) -> Result<String, ForgeError> {
        let (host, name) = repo
            .split_once('/')
            .ok_or_else(|| ForgeError("the registered remote has no repository name".into()))?;
        let api = |method: &str, endpoint: String, extra: Vec<String>| {
            let mut argv = args(&[
                "api",
                &format!("--hostname={host}"),
                &format!("--method={method}"),
            ]);
            argv.extend(extra.into_iter().map(OsString::from));
            argv.push(endpoint.into());
            self.gh(Path::new("."), argv)
        };
        let issue = api("GET", format!("repos/{name}/issues/{number}"), Vec::new()).await?;
        let issue: Json = serde_json::from_str(&issue)
            .map_err(|error| ForgeError(format!("invalid issue response: {error}")))?;
        if !issue["pull_request"].is_null() {
            return Err(ForgeError(format!(
                "#{number} is a pull request; label_issue labels issues only"
            )));
        }
        let mut report = Vec::new();
        if !add.is_empty() {
            let known = api(
                "GET",
                format!("repos/{name}/labels"),
                vec!["--paginate".into(), "--jq=.[].name".into()],
            )
            .await?;
            let known: Vec<&str> = known.lines().map(str::trim).collect();
            let (found, missing): (Vec<&String>, Vec<&String>) = add
                .iter()
                .partition(|label| known.iter().any(|k| k.eq_ignore_ascii_case(label)));
            if !found.is_empty() {
                let fields = found
                    .iter()
                    .map(|label| format!("--raw-field=labels[]={label}"));
                api(
                    "POST",
                    format!("repos/{name}/issues/{number}/labels"),
                    fields.collect(),
                )
                .await?;
                report.push(format!("Added {}.", join(&found)));
            }
            if !missing.is_empty() {
                report.push(format!(
                    "Not added, because {repo} has no such label: {}.",
                    join(&missing)
                ));
            }
        }
        for label in remove {
            let endpoint = format!(
                "repos/{name}/issues/{number}/labels/{}",
                path_segment(label)
            );
            match api("DELETE", endpoint, Vec::new()).await {
                Ok(_) => {}
                // The issue does not have the label (or it does not exist): nothing to remove.
                Err(ForgeError(error)) if error.contains("HTTP 404") => {}
                Err(error) => return Err(error),
            }
        }
        if !remove.is_empty() {
            report.push(format!("Removed {}.", remove.join(", ")));
        }
        Ok(format!("Issue #{number} in {repo}: {}", report.join(" ")))
    }
}

/// The report of a close whose branch was kept because it moved.
fn kept(closed: &str, branch: &str, expected_head: &str) -> String {
    format!(
        "{closed}. Warning: kept branch {branch}, because it no longer points at {expected_head}."
    )
}

fn join(labels: &[&String]) -> String {
    labels
        .iter()
        .map(|l| l.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Percent-encodes `text` for one URL path segment.
fn path_segment(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
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

/// The head branch of `pull` if it may be deleted: in the registered
/// repository and not its base or default branch.
fn closable_head<'a>(pull: &'a Json, name: &str) -> Result<&'a str, ForgeError> {
    let head = &pull["head"];
    if !head["repo"]["full_name"]
        .as_str()
        .is_some_and(|actual| actual.eq_ignore_ascii_case(name))
    {
        return Err(ForgeError(
            "the pull request's head branch is not in the registered repository".into(),
        ));
    }
    let branch = head["ref"]
        .as_str()
        .filter(|branch| valid_branch_name(branch))
        .ok_or_else(|| ForgeError("the pull request has no valid head branch".into()))?;
    if pull["base"]["ref"] == branch || head["repo"]["default_branch"] == branch {
        return Err(ForgeError(
            "the pull request head is the base or default branch".into(),
        ));
    }
    Ok(branch)
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
        let base_ref = format!("refs/heads/{base}");
        git(
            Some(worktree),
            [
                "ls-remote",
                "--exit-code",
                "--heads",
                "origin",
                base_ref.as_str(),
            ],
        )
        .await
        .map_err(|_| ForgeError(format!("base branch {base:?} does not exist on the remote")))?;
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
        if let RepositoryAction::ClosePullRequest {
            number,
            expected_head,
            delete_branch,
        } = action
        {
            return self
                .close_pull_request(remote, &repo, *number, expected_head, *delete_branch)
                .await;
        }
        match action {
            RepositoryAction::ResolveReviewThread {
                number,
                expected_head,
                thread_id,
                fixed_by,
                body,
            } => {
                return self
                    .resolve_review_thread(
                        &repo,
                        *number,
                        expected_head,
                        thread_id,
                        fixed_by.as_deref(),
                        body,
                    )
                    .await;
            }
            RepositoryAction::RequestReview {
                number,
                expected_head,
                reviewers,
                comment,
            } => {
                return self
                    .request_review(&repo, *number, expected_head, reviewers, comment.as_deref())
                    .await;
            }
            RepositoryAction::MergeBatch { pulls } => return self.merge_batch(&repo, pulls).await,
            RepositoryAction::MarkReadyBatch { numbers } => {
                return self.mark_ready_batch(&repo, numbers).await;
            }
            _ => {}
        }
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
            RepositoryAction::Merge {
                number,
                expected_head,
            } => {
                return self
                    .merge_pull(&repo, *number, expected_head, false)
                    .await
                    .map(Merge::into_report);
            }
            RepositoryAction::MarkReady { number } => args(&["pr", "ready", &number.to_string()]),
            RepositoryAction::EditPullRequest {
                number,
                title,
                body,
            } => {
                let mut argv = args(&["pr", "edit", &number.to_string()]);
                argv.extend(title.iter().map(|title| format!("--title={title}").into()));
                argv.extend(body.iter().map(|body| format!("--body={body}").into()));
                argv
            }
            RepositoryAction::LabelIssue {
                number,
                add,
                remove,
            } => return self.label_issue(&repo, *number, add, remove).await,
            RepositoryAction::ClosePullRequest { .. }
            | RepositoryAction::ResolveReviewThread { .. }
            | RepositoryAction::RequestReview { .. }
            | RepositoryAction::MergeBatch { .. }
            | RepositoryAction::MarkReadyBatch { .. } => unreachable!("handled above"),
            RepositoryAction::GhCli { args } => args.iter().map(OsString::from).collect(),
            RepositoryAction::AssignIssue { number } => {
                args(&["issue", "edit", &number.to_string(), "--add-assignee=@me"])
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

    async fn attach_image(
        &self,
        worktree: &Path,
        attachment: PullRequestImage<'_>,
    ) -> Result<String, ForgeError> {
        let repo = normalize_remote(attachment.remote)
            .ok_or_else(|| ForgeError("the registered remote is not a forge repository".into()))?;
        let git_in = |argv: Vec<OsString>, env: &[(&str, &OsString)]| {
            let mut command = git_command(Some(worktree), &argv);
            command.envs(env.iter().copied());
            async move {
                run(command, "git", &argv, GH_TIMEOUT)
                    .await
                    .map(|out| out.trim().to_owned())
                    .map_err(ForgeError)
            }
        };
        let mut hash = args(&["hash-object", "-w", "--"]);
        hash.push(attachment.image.into());
        let blob = git_in(hash, &[]).await?;
        if blob != attachment.blob {
            return Err(ForgeError(
                "the image changed since it was approved; request approval again".into(),
            ));
        }
        let name = format!("{}.{}", &blob[..12], attachment.extension);
        // A one-file tree, built in a scratch index so the worktree's own is untouched.
        let index: OsString = std::env::temp_dir()
            .join(format!("threadlane-image-{}-{blob}", std::process::id()))
            .into();
        let cacheinfo = format!("100644,{blob},{name}");
        let tree = async {
            git_in(
                args(&["update-index", "--add", "--cacheinfo", &cacheinfo]),
                &[("GIT_INDEX_FILE", &index)],
            )
            .await?;
            git_in(args(&["write-tree"]), &[("GIT_INDEX_FILE", &index)]).await
        }
        .await;
        let _ = tokio::fs::remove_file(&index).await;
        let tree = tree?;
        // A fixed date makes the commit, and so its branch, the same on a retry.
        let epoch = OsString::from("@0 +0000");
        let message = format!("Threadlane image {name}");
        let mut commit_tree = args(&COMMITTER);
        commit_tree.extend(args(&["commit-tree", &tree, "-m", &message]));
        let commit = git_in(
            commit_tree,
            &[("GIT_AUTHOR_DATE", &epoch), ("GIT_COMMITTER_DATE", &epoch)],
        )
        .await?;
        if !valid_commit_id(&commit) {
            return Err(ForgeError("git commit-tree printed no commit ID".into()));
        }
        // shortcut: one branch per image keeps the push non-forced and needs no fetch; move to a shared assets branch if they pile up.
        let refspec = format!("{commit}:refs/heads/threadlane-assets/{}", &commit[..16]);
        git_in(args(&["push", "--quiet", "origin", "--", &refspec]), &[]).await?;
        let url = format!("https://{repo}/raw/{commit}/{name}");
        let comment = self
            .gh(
                worktree,
                args(&[
                    "pr",
                    "comment",
                    &attachment.number.to_string(),
                    &format!("--body=![{}]({url})", attachment.caption),
                    &format!("--repo={repo}"),
                ]),
            )
            .await?;
        Ok(format!("Image: {url}\nComment: {}", comment.trim()))
    }

    async fn pull_request_base(
        &self,
        remote: &str,
        number: std::num::NonZeroU64,
    ) -> Result<String, ForgeError> {
        let repo = normalize_remote(remote)
            .ok_or_else(|| ForgeError("the registered remote is not a forge repository".into()))?;
        let (host, name) = repo
            .split_once('/')
            .ok_or_else(|| ForgeError("the registered remote has no repository name".into()))?;
        let output = self
            .gh(
                Path::new("."),
                args(&[
                    "api",
                    &format!("--hostname={host}"),
                    "--method=GET",
                    &format!("repos/{name}/pulls/{number}"),
                    "--jq=.base.ref",
                ]),
            )
            .await?;
        let base = output.trim();
        check_branch(base)?;
        Ok(base.to_owned())
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
