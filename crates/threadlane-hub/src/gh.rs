//! [`GhSource`]: the GitHub source through `gh api` (Plan.md §6.6,
//! docs/milestone-8.md §1 decision 6).
//!
//! Every call is `gh api --method GET --include <path>`: the method is stated, never
//! left to `gh`, which switches to POST when given fields, and no field,
//! input or other method is ever passed. So the source never marks a
//! notification read (`PATCH`/`PUT notifications...`) and never writes. The
//! query is part of the path. `gh` runs as a subprocess with explicit
//! arguments, never through a shell, bounded by a timeout, with the login
//! this user already has; the system sets no credential and reads no token
//! (invariant 8).
//!
//! The calls:
//!
//! - the feed: `gh api --method GET --include [-H "If-Modified-Since: <cursor>"]
//!   -H "Accept: application/vnd.github+json" notifications?per_page=50`.
//!   `304` is "not modified", `Last-Modified` the next cursor,
//!   `X-Poll-Interval` the interval asked for;
//! - a pull request's head, when only its number is known:
//!   `repos/<owner>/<name>/pulls/<n>`, reading `head.ref` and `head.sha`;
//! - check runs: `repos/<owner>/<name>/commits/<ref>/check-runs?per_page=100`,
//!   on the head commit when it was looked up (a fork's branch, or one
//!   deleted after the merge, is not in the base repository), else the branch;
//! - a pull request's reviews: `repos/<owner>/<name>/pulls/<n>/reviews?per_page=100`;
//! - its comments: `repos/<owner>/<name>/issues/<n>/comments?per_page=100[&since=<time>]`;
//! - who holds an issue, before `assign_issue` asks: `repos/<owner>/<name>/issues/<n>`,
//!   its `timeline?per_page=100` for open pull requests that cross-reference it,
//!   and `user` for the login `gh` reads as;
//! - a pull request's state, before a task's own step on it: `repos/<owner>/<name>/pulls/<n>`
//!   for its head, draft flag and `mergeable_state`, and the check runs on its head commit;
//! - a pull request's changed files and their patches: `repos/<owner>/<name>/pulls/<n>/files?per_page=100`;
//! - its unresolved review threads, for the lead's readiness read: the one
//!   exception to GET, `gh api graphql` with the fixed query
//!   [`REVIEW_THREADS_QUERY`], which only reads (REST has no thread state).
//!
//! With `--include` each answer starts with the status line and headers. A
//! `Link` header's `rel="next"` is followed, as another such GET of the path
//! it names. Feed and work polls read at most [`MAX_PAGES`] pages per call.
//! Latest-activity lookups compare timestamps across all pages.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::github::{
    CheckRun, Feed, GithubError, GithubItem, GithubKind, GithubSource, IssueClaim, PullRequestFile,
    PullRequestState, PullTarget, REVIEW_MARKER_PREFIX, pull_request_number,
};
use tokio::process::Command;

/// The longest one `gh api` call may take.
const GH_TIMEOUT: Duration = Duration::from_secs(60);
/// What every call accepts.
const ACCEPT: &str = "Accept: application/vnd.github+json";
/// The most pages one call reads.
const MAX_PAGES: usize = 10;
/// The one GraphQL call, a fixed read-only query: REST cannot say whether a
/// review thread is resolved.
// shortcut: reads the first 100 threads, enough for any PR seen so far; page with `after` if one has more.
const REVIEW_THREADS_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100){nodes{id isResolved isOutdated path line comments(first:1){nodes{author{login} body}}}}}}}";

/// [`GithubSource`] through `gh api`.
#[derive(Debug, Clone)]
pub struct GhSource {
    gh: PathBuf,
}

impl Default for GhSource {
    /// Runs the `gh` found on `PATH`.
    fn default() -> Self {
        Self::with_gh("gh")
    }
}

impl GhSource {
    /// Runs `gh` from `program`.
    #[must_use]
    pub fn with_gh(program: impl Into<PathBuf>) -> Self {
        Self { gh: program.into() }
    }

    /// Runs `gh api --method GET --include <extra> <path>`, kept from
    /// prompting. Returns whether it succeeded, and its standard output.
    async fn get(&self, extra: &[String], path: &str) -> Result<(bool, String), GithubError> {
        let mut args: Vec<String> = ["api", "--method", "GET", "--include"]
            .map(str::to_owned)
            .into();
        args.extend_from_slice(extra);
        args.extend(["-H".to_owned(), ACCEPT.to_owned(), path.to_owned()]);
        self.gh(&args, path).await
    }

    /// Runs `gh <args>`, kept from prompting. Returns whether it succeeded,
    /// and its standard output; `path` names the call in errors.
    async fn gh(&self, args: &[String], path: &str) -> Result<(bool, String), GithubError> {
        let mut command = Command::new(&self.gh);
        command
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(GH_TIMEOUT, command.output())
            .await
            .map_err(|_| GithubError(format!("gh api {path} timed out after {GH_TIMEOUT:?}")))?
            .map_err(|error| GithubError(format!("could not run gh api {path}: {error}")))?;
        if !output.status.success() {
            tracing::debug!(
                path,
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "gh api failed"
            );
        }
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    /// The pages of `path`, following `rel="next"` up to [`MAX_PAGES`];
    /// `extra` goes with the first call only. A page that failed and is not
    /// a 304 fails the call.
    async fn get_pages(&self, extra: &[String], path: &str) -> Result<Vec<Answer>, GithubError> {
        let mut pages = Vec::new();
        let mut next = Some(path.to_owned());
        while let Some(path) = next.take() {
            let extra = if pages.is_empty() { extra } else { &[] };
            let page = self.get_page(extra, &path).await?;
            next = page.next.clone().filter(|_| pages.len() + 1 < MAX_PAGES);
            pages.push(page);
        }
        Ok(pages)
    }

    /// Every page of `path`, or an error when [`MAX_PAGES`] leaves some
    /// unread, for answers that must be complete, such as check state.
    async fn get_every_page(&self, path: &str) -> Result<Vec<Answer>, GithubError> {
        let pages = self.get_pages(&[], path).await?;
        if pages.len() == MAX_PAGES && pages.last().is_some_and(|page| page.next.is_some()) {
            return Err(GithubError(format!(
                "gh api {path} has more than {MAX_PAGES} pages; not all were read"
            )));
        }
        Ok(pages)
    }

    async fn get_page(&self, extra: &[String], path: &str) -> Result<Answer, GithubError> {
        let (ok, output) = self.get(extra, path).await?;
        match parse_answer(&output) {
            Ok(page) if ok || page.status == 304 => Ok(page),
            Err(error) if ok => Err(error),
            _ => Err(GithubError(format!("gh api {path} failed"))),
        }
    }

    /// The JSON of a one-page GET that must succeed.
    async fn get_json(&self, path: &str) -> Result<Json, GithubError> {
        self.get_page(&[], path).await?.json(path)
    }

    /// Issue comments are ordered by id, so a later page or an edit to an
    /// older comment can supply the most recently updated item.
    async fn latest_item(
        &self,
        path: String,
        target: &PullTarget,
        parse: fn(&Json, &PullTarget) -> Vec<GithubItem>,
    ) -> Result<Option<GithubItem>, GithubError> {
        let mut next = Some(path);
        let mut visited = std::collections::HashSet::new();
        let mut latest: Option<GithubItem> = None;
        while let Some(path) = next {
            if visited.len() >= MAX_PAGES * 5 {
                return Err(GithubError(format!(
                    "GitHub pagination exceeded {} pages: {path}",
                    MAX_PAGES * 5
                )));
            }
            if !visited.insert(path.clone()) {
                return Err(GithubError(format!("repeated GitHub page: {path}")));
            }
            let page = self.get_page(&[], &path).await?;
            latest = latest
                .into_iter()
                .chain(parse(&page.json(&path)?, target))
                .max_by(|a, b| a.at.cmp(&b.at));
            next = page.next;
        }
        Ok(latest)
    }

    /// The head branch and commit of pull request `number`, read from
    /// `pulls/<n>`.
    async fn head(&self, repository: &str, number: u64) -> Result<(String, String), GithubError> {
        let path = format!("repos/{repository}/pulls/{number}");
        let body = self.get_json(&path).await?;
        head_of(&body)
            .ok_or_else(|| GithubError(format!("gh api {path} names no head branch and commit")))
    }

    /// The items of every JSON page of a call that must succeed.
    async fn get_items(
        &self,
        path: &str,
        target: &PullTarget,
        parse: fn(&Json, &PullTarget) -> Vec<GithubItem>,
    ) -> Result<Vec<GithubItem>, GithubError> {
        let mut items = Vec::new();
        for page in self.get_pages(&[], path).await? {
            items.extend(parse(&page.json(path)?, target));
        }
        Ok(items)
    }
}

#[async_trait]
impl GithubSource for GhSource {
    async fn notifications(&self, cursor: Option<&str>) -> Result<Feed, GithubError> {
        let mut extra = Vec::new();
        if let Some(cursor) = cursor.filter(|cursor| !cursor.chars().any(char::is_control)) {
            extra.extend(["-H".to_owned(), format!("If-Modified-Since: {cursor}")]);
        }
        // gh exits non-zero on a 304, which is the answer "not modified";
        // the first page gives the cursor and the interval.
        let mut pages = self
            .get_pages(&extra, "notifications?per_page=50")
            .await?
            .into_iter();
        let mut feed = pages.next().map(feed_of).transpose()?.unwrap_or_default();
        for page in pages {
            feed.items.extend(feed_of(page)?.items);
        }
        Ok(feed)
    }

    async fn pull_request(
        &self,
        target: &PullTarget,
        since: Option<&str>,
    ) -> Result<Vec<GithubItem>, GithubError> {
        let repository = &target.repository;
        if !valid_full_name(repository) {
            return Err(GithubError(format!(
                "{repository:?} is not a repository name"
            )));
        }
        let resolved;
        let (target, reference) = match target.number {
            Some(number) if target.branch.is_empty() => {
                let (branch, sha) = self.head(repository, number).await?;
                resolved = PullTarget {
                    branch,
                    ..target.clone()
                };
                (&resolved, sha)
            }
            _ => (target, target.branch.clone()),
        };
        let reference = encode(&reference);
        let mut items = self
            .get_items(
                &format!("repos/{repository}/commits/{reference}/check-runs?per_page=100"),
                target,
                settled_check_runs,
            )
            .await?;
        if let Some(number) = target.number {
            items.extend(
                self.get_items(
                    &format!("repos/{repository}/pulls/{number}/reviews?per_page=100"),
                    target,
                    parse_reviews,
                )
                .await?,
            );
            let since_query =
                since.map_or_else(String::new, |since| format!("&since={}", encode(since)));
            items.extend(
                self.get_items(
                    &format!(
                        "repos/{repository}/issues/{number}/comments?per_page=100{since_query}"
                    ),
                    target,
                    parse_comments,
                )
                .await?,
            );
        }
        items.retain(|item| since.is_none_or(|since| item.at.as_str() >= since));
        items.sort_by(|a, b| a.at.cmp(&b.at));
        Ok(items)
    }

    async fn activity(
        &self,
        notification: &GithubItem,
        items: &mut Vec<GithubItem>,
    ) -> Result<(), GithubError> {
        let (Some(number), repository) = (notification.pull_request, &notification.repository)
        else {
            items.push(notification.clone());
            return Ok(());
        };
        if !valid_full_name(repository) {
            return Err(GithubError(format!(
                "{repository:?} is not a repository name"
            )));
        }
        let path = format!("repos/{repository}/pulls/{number}");
        let pull = self.get_json(&path).await?;
        let (branch, sha) = head_of(&pull)
            .ok_or_else(|| GithubError(format!("gh api {path} names no head branch and commit")))?;
        let state = if pull["merged"] == true {
            "merged".to_owned()
        } else {
            text(&pull, "/state").unwrap_or_default()
        };
        let target = PullTarget {
            repository: repository.clone(),
            branch,
            number: Some(number),
        };
        // The notification's own id: each update of the thread is one item,
        // and the same update delivered again is the same item.
        items.push(GithubItem {
            kind: GithubKind::PullRequestUpdate,
            branch: Some(target.branch.clone()),
            title: format!("head {sha}"),
            body: format!(
                "{} ({state})\nNotified as {}.",
                notification.title,
                notification.kind.describe()
            ),
            url: text(&pull, "/html_url"),
            ..notification.clone()
        });
        if notification.kind == GithubKind::CheckResult {
            let checks = self
                .get_items(
                    &format!(
                        "repos/{repository}/commits/{}/check-runs?per_page=100",
                        encode(&sha)
                    ),
                    &target,
                    parse_check_runs,
                )
                .await?;
            items.extend(checks);
            return Ok(());
        }
        let review = self
            .latest_item(
                format!("repos/{repository}/pulls/{number}/reviews?per_page=100"),
                &target,
                submitted_reviews,
            )
            .await?;
        items.extend(review.filter(external_review));
        let comment = match notification
            .url
            .as_deref()
            .and_then(|url| comment_path(url, repository))
        {
            Some(path) => {
                let comment = self.get_json(&format!("repos/{repository}/{path}")).await?;
                parse_comments(&Json::Array(vec![comment]), &target)
                    .pop()
                    .map(|mut item| {
                        if path.starts_with("pulls/") {
                            item.id = format!("inline:{}", item.id);
                            item.title = "inline review comment".into();
                        }
                        item
                    })
            }
            None => {
                self.latest_item(
                    format!("repos/{repository}/issues/{number}/comments?per_page=100"),
                    &target,
                    parse_comments,
                )
                .await?
            }
        };
        items.extend(comment);
        if notification.kind == GithubKind::ReviewRequest {
            let request = self
                .latest_item(
                    format!("repos/{repository}/issues/{number}/events?per_page=100"),
                    &target,
                    parse_review_requests,
                )
                .await?;
            items.extend(request);
        }
        Ok(())
    }

    async fn issue_claim(&self, repository: &str, number: u64) -> Result<IssueClaim, GithubError> {
        if !valid_full_name(repository) {
            return Err(GithubError(format!(
                "{repository:?} is not a repository name"
            )));
        }
        let issue = self
            .get_json(&format!("repos/{repository}/issues/{number}"))
            .await?;
        let path = format!("repos/{repository}/issues/{number}/timeline?per_page=100");
        let mut open_pull_requests = Vec::new();
        for page in self.get_pages(&[], &path).await? {
            open_pull_requests.extend(open_references(&page.json(&path)?));
        }
        let viewer = text(&self.get_json("user").await?, "/login")
            .ok_or_else(|| GithubError("gh api user names no login".to_owned()))?;
        Ok(IssueClaim {
            viewer,
            is_pull_request: !issue["pull_request"].is_null(),
            open: issue["state"] == "open",
            assignees: issue["assignees"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|user| text(user, "/login"))
                .collect(),
            open_pull_requests,
        })
    }

    async fn issue(&self, repository: &str, number: u64) -> Result<Json, GithubError> {
        if !valid_full_name(repository) {
            return Err(GithubError(format!(
                "{repository:?} is not a repository name"
            )));
        }
        let issue = self
            .get_json(&format!("repos/{repository}/issues/{number}"))
            .await?;
        let path = format!("repos/{repository}/issues/{number}/comments?per_page=100");
        let mut comments = Vec::new();
        for page in self.get_pages(&[], &path).await? {
            comments.extend(
                page.json(&path)?
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|comment| {
                        serde_json::json!({
                            "author": text(comment, "/user/login"),
                            "at": text(comment, "/created_at"),
                            "body": text(comment, "/body"),
                        })
                    }),
            );
        }
        Ok(serde_json::json!({
            "number": number,
            "url": text(&issue, "/html_url"),
            "is_pull_request": !issue["pull_request"].is_null(),
            "title": text(&issue, "/title"),
            "state": text(&issue, "/state"),
            "author": text(&issue, "/user/login"),
            "labels": issue["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|label| text(label, "/name"))
                .collect::<Vec<_>>(),
            "body": text(&issue, "/body"),
            "comments": comments,
        }))
    }

    async fn pull_request_state(
        &self,
        repository: &str,
        number: u64,
    ) -> Result<PullRequestState, GithubError> {
        if !valid_full_name(repository) {
            return Err(GithubError(format!(
                "{repository:?} is not a repository name"
            )));
        }
        let path = format!("repos/{repository}/pulls/{number}");
        let pull = self.get_json(&path).await?;
        let (head_ref, head_sha) = head_of(&pull)
            .ok_or_else(|| GithubError(format!("gh api {path} names no head branch and commit")))?;
        let path = format!(
            "repos/{repository}/commits/{}/check-runs?per_page=100",
            encode(&head_sha)
        );
        let mut checks = Vec::new();
        for page in self.get_every_page(&path).await? {
            let page = page.json(&path)?;
            let runs = page.get("check_runs").and_then(Json::as_array);
            checks.extend(runs.into_iter().flatten().map(|run| {
                CheckRun {
                    name: text(run, "/name").unwrap_or_default(),
                    conclusion: (run["status"] == "completed")
                        .then(|| text(run, "/conclusion"))
                        .flatten(),
                }
            }));
        }
        Ok(PullRequestState {
            head_repository: text(&pull, "/head/repo/full_name").unwrap_or_default(),
            head_ref,
            head_sha,
            draft: pull["draft"] == true,
            mergeable_state: text(&pull, "/mergeable_state")
                .unwrap_or_default()
                .to_ascii_lowercase(),
            checks,
            url: text(&pull, "/html_url").unwrap_or_default(),
            state: text(&pull, "/state").unwrap_or_default(),
            merged: pull["merged"] == true,
        })
    }

    async fn unresolved_review_threads(
        &self,
        repository: &str,
        number: u64,
    ) -> Result<Vec<Json>, GithubError> {
        let Some((owner, name)) = repository
            .split_once('/')
            .filter(|_| valid_full_name(repository))
        else {
            return Err(GithubError(format!(
                "{repository:?} is not a repository name"
            )));
        };
        let args: Vec<String> = [
            "api".to_owned(),
            "-f".to_owned(),
            format!("query={REVIEW_THREADS_QUERY}"),
            "-f".to_owned(),
            format!("owner={owner}"),
            "-f".to_owned(),
            format!("name={name}"),
            "-F".to_owned(),
            format!("number={number}"),
            "graphql".to_owned(),
        ]
        .into();
        let (ok, output) = self.gh(&args, "graphql reviewThreads").await?;
        let answer: Json = serde_json::from_str(&output)
            .map_err(|error| GithubError(format!("gh api graphql printed no JSON: {error}")))?;
        let threads = answer
            .pointer("/data/repository/pullRequest/reviewThreads/nodes")
            .and_then(Json::as_array)
            .filter(|_| ok && answer.get("errors").is_none())
            .ok_or_else(|| {
                GithubError(format!(
                    "gh api graphql could not read the review threads of {repository}#{number}"
                ))
            })?;
        Ok(threads
            .iter()
            .filter(|thread| thread["isResolved"] == false)
            .map(|thread| {
                let comment = &thread["comments"]["nodes"][0];
                serde_json::json!({
                    "id": thread["id"],
                    "path": thread["path"],
                    "line": thread["line"],
                    "outdated": thread["isOutdated"],
                    "author": comment["author"]["login"],
                    "body": comment["body"],
                })
            })
            .collect())
    }

    async fn pull_request_files(
        &self,
        repository: &str,
        number: u64,
    ) -> Result<Vec<PullRequestFile>, GithubError> {
        if !valid_full_name(repository) {
            return Err(GithubError(format!(
                "{repository:?} is not a repository name"
            )));
        }
        let path = format!("repos/{repository}/pulls/{number}/files?per_page=100");
        let mut files = Vec::new();
        for page in self.get_every_page(&path).await? {
            let page = page.json(&path)?;
            files.extend(
                page.as_array()
                    .into_iter()
                    .flatten()
                    .map(|file| PullRequestFile {
                        path: text(file, "/filename").unwrap_or_default(),
                        previous_path: text(file, "/previous_filename"),
                        status: text(file, "/status").unwrap_or_default(),
                        additions: file["additions"].as_u64().unwrap_or_default(),
                        deletions: file["deletions"].as_u64().unwrap_or_default(),
                        patch: text(file, "/patch"),
                    }),
            );
        }
        Ok(files)
    }
}

/// The open pull requests a timeline page says reference the issue, as
/// `(number, author login)`.
fn open_references(timeline: &Json) -> Vec<(u64, String)> {
    timeline
        .as_array()
        .into_iter()
        .flatten()
        .filter(|event| event["event"] == "cross-referenced")
        .filter_map(|event| {
            let source = &event["source"]["issue"];
            (!source["pull_request"].is_null() && source["state"] == "open")
                .then(|| Some((source["number"].as_u64()?, text(source, "/user/login")?)))?
        })
        .collect()
}

/// `owner/name` with the characters GitHub allows in them.
fn valid_full_name(name: &str) -> bool {
    let mut parts = name.split('/');
    let part = |part: Option<&str>| {
        part.is_some_and(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    };
    part(parts.next()) && part(parts.next()) && parts.next().is_none()
}

/// Percent-encodes everything but the unreserved characters, so a branch
/// such as `threadlane/T-0001` is one path segment.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn parse_review_requests(answer: &Json, target: &PullTarget) -> Vec<GithubItem> {
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter(|event| event["event"] == "review_requested")
        .filter_map(|event| {
            Some(GithubItem {
                id: format!("review_request:{}", id(event)?),
                kind: GithubKind::ReviewRequest,
                repository: target.repository.clone(),
                pull_request: target.number,
                branch: Some(target.branch.clone()),
                author: text(event, "/actor/login"),
                title: text(event, "/requested_reviewer/login")
                    .or_else(|| text(event, "/requested_team/slug"))
                    .map_or_else(
                        || "Review requested".into(),
                        |who| format!("Requested review from {who}"),
                    ),
                body: String::new(),
                url: text(event, "/url"),
                at: text(event, "/created_at")?,
            })
        })
        .collect()
}

/// Only use comment endpoints in the notified repository.
fn comment_path<'a>(url: &'a str, repository: &str) -> Option<&'a str> {
    let path = url
        .strip_prefix("https://api.github.com/repos/")?
        .strip_prefix(repository)?
        .strip_prefix('/')?;
    let (resource, id) = path.rsplit_once('/')?;
    (matches!(resource, "pulls/comments" | "issues/comments") && id.parse::<u64>().is_ok())
        .then_some(path)
}

/// The head branch and commit of a `pulls/<n>` answer.
fn head_of(pull: &Json) -> Option<(String, String)> {
    let field = |pointer| text(pull, pointer).filter(|value| !value.is_empty());
    field("/head/ref").zip(field("/head/sha"))
}

fn text(value: &Json, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Json::as_str)
        .map(str::to_owned)
}

/// An id GitHub gives as a number or a string.
fn id(value: &Json) -> Option<String> {
    match value.get("id")? {
        Json::Number(number) => Some(number.to_string()),
        Json::String(text) => Some(text.clone()),
        _ => None,
    }
}

/// One answer as `gh api --include` prints it: the status line, the
/// headers, a blank line, the body.
#[derive(Debug, Default)]
struct Answer {
    status: u16,
    cursor: Option<String>,
    poll_interval: Option<Duration>,
    /// The path of the `rel="next"` page.
    next: Option<String>,
    body: String,
}

impl Answer {
    fn json(&self, path: &str) -> Result<Json, GithubError> {
        if !(200..=299).contains(&self.status) {
            return Err(GithubError(format!(
                "gh api {path} answered {}",
                self.status
            )));
        }
        serde_json::from_str(&self.body)
            .map_err(|error| GithubError(format!("gh api {path} answered: {error}")))
    }
}

fn parse_answer(output: &str) -> Result<Answer, GithubError> {
    let output = output.replace("\r\n", "\n");
    let (head, body) = output.split_once("\n\n").unwrap_or((output.as_str(), ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| GithubError("the answer has no status line".to_owned()))?;
    let mut answer = Answer {
        status,
        body: body.to_owned(),
        ..Answer::default()
    };
    for (name, value) in lines.filter_map(|line| line.split_once(':')) {
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "last-modified" if !value.is_empty() => answer.cursor = Some(value.to_owned()),
            "x-poll-interval" => {
                answer.poll_interval = value.parse().ok().map(Duration::from_secs);
            }
            "link" => answer.next = next_link(value),
            _ => {}
        }
    }
    Ok(answer)
}

/// The path, on the same host, of a `Link` header's `rel="next"`:
/// `<https://api.github.com/notifications?page=2>; rel="next"` gives
/// `notifications?page=2`.
fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        if !params
            .split(';')
            .any(|param| param.trim() == r#"rel="next""#)
        {
            return None;
        }
        let url = url.trim().strip_prefix('<')?.strip_suffix('>')?;
        let (_host, path) = url.strip_prefix("https://")?.split_once('/')?;
        let safe = !path.is_empty()
            && !path.starts_with(['/', '-'])
            && !path.chars().any(|c| c.is_control() || c.is_whitespace());
        safe.then(|| path.to_owned())
    })
}

fn feed_of(answer: Answer) -> Result<Feed, GithubError> {
    let mut feed = Feed {
        cursor: answer.cursor,
        poll_interval: answer.poll_interval,
        ..Feed::default()
    };
    match answer.status {
        304 => {
            feed.not_modified = true;
            Ok(feed)
        }
        200..=299 => {
            let body: Json = serde_json::from_str(&answer.body)
                .map_err(|error| GithubError(format!("the feed answered: {error}")))?;
            feed.items = body
                .as_array()
                .map(|items| items.iter().filter_map(notification).collect())
                .unwrap_or_default();
            Ok(feed)
        }
        other => Err(GithubError(format!("the feed answered {other}"))),
    }
}

/// One notification thread, at its latest update: a new comment on the
/// same thread is a new item.
fn notification(value: &Json) -> Option<GithubItem> {
    let thread = id(value)?;
    let at = text(value, "/updated_at")?;
    let reason = text(value, "/reason").unwrap_or_default();
    let subject_type = text(value, "/subject/type").unwrap_or_default();
    let mut title = text(value, "/subject/title").unwrap_or_default();
    let kind = match reason.as_str() {
        "review_requested" => GithubKind::ReviewRequest,
        "mention" | "team_mention" => GithubKind::Mention,
        "comment" => GithubKind::Comment,
        "ci_activity" => GithubKind::CheckResult,
        _ => GithubKind::Notification,
    };
    let pull_request = match subject_type.as_str() {
        "PullRequest" => text(value, "/subject/url").and_then(|url| pull_request_number(&url)),
        _ => None,
    };
    // A workflow run's subject is "<workflow> workflow run <result> for
    // <branch> branch", and names no pull request.
    let mut branch = None;
    if subject_type == "CheckSuite"
        && let Some((run, found)) = title
            .strip_suffix(" branch")
            .and_then(|rest| rest.rsplit_once(" for "))
    {
        // As a check run reads: "<workflow>: <result>".
        let renamed = run
            .split_once(" workflow run ")
            .map(|(workflow, result)| format!("{workflow}: {result}"));
        branch = Some(found.to_owned());
        if let Some(renamed) = renamed {
            title = renamed;
        }
    }
    if kind == GithubKind::Notification && !reason.is_empty() {
        title = format!("{title} ({})", reason.replace('_', " "));
    }
    let repository = text(value, "/repository/full_name")?;
    let url = text(value, "/subject/latest_comment_url")
        .filter(|url| pull_request.is_some() && comment_path(url, &repository).is_some());
    Some(GithubItem {
        id: format!("notification:{thread}:{at}"),
        kind,
        repository,
        pull_request,
        branch,
        author: None,
        title,
        body: String::new(),
        url,
        at,
    })
}

/// [`parse_check_runs`] for a task's own work. While another check of the
/// head is still running, the ones that passed are held back: one passed
/// check decides nothing, and each would wake the task's agent to no end.
/// They arrive together once the last check finishes; a failure is not held.
pub(crate) fn settled_check_runs(answer: &Json, target: &PullTarget) -> Vec<GithubItem> {
    let runs = answer.get("check_runs").and_then(Json::as_array);
    let running = runs
        .into_iter()
        .flatten()
        .any(|run| run["status"] != "completed");
    let mut items = parse_check_runs(answer, target);
    if running {
        items.retain(|item| {
            !["success", "skipped", "neutral"]
                .iter()
                .any(|conclusion| item.title.ends_with(&format!(": {conclusion}")))
        });
    }
    items
}

/// The finished check runs of a `check-runs` answer.
pub(crate) fn parse_check_runs(answer: &Json, target: &PullTarget) -> Vec<GithubItem> {
    let runs = answer.get("check_runs").and_then(Json::as_array);
    runs.into_iter()
        .flatten()
        .filter(|run| run["status"] == "completed")
        .filter_map(|run| {
            let output = [text(run, "/output/title"), text(run, "/output/summary")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n");
            Some(GithubItem {
                id: format!("check_run:{}", id(run)?),
                kind: GithubKind::CheckResult,
                repository: target.repository.clone(),
                pull_request: target.number,
                branch: Some(target.branch.clone()),
                author: text(run, "/app/slug"),
                title: format!(
                    "{}: {}",
                    text(run, "/name").unwrap_or_default(),
                    text(run, "/conclusion").unwrap_or_else(|| "finished".to_owned())
                ),
                body: output,
                url: text(run, "/html_url"),
                at: text(run, "/completed_at")?,
            })
        })
        .collect()
}

/// Submitted reviews from GitHub, excluding verdicts already delivered inside Threadlane.
pub(crate) fn parse_reviews(answer: &Json, target: &PullTarget) -> Vec<GithubItem> {
    submitted_reviews(answer, target)
        .into_iter()
        .filter(external_review)
        .collect()
}

fn external_review(review: &GithubItem) -> bool {
    let key = review
        .body
        .trim_end()
        .lines()
        .next_back()
        .and_then(|line| line.strip_prefix(REVIEW_MARKER_PREFIX))
        .and_then(|rest| rest.strip_suffix(" -->"));
    !key.is_some_and(|key| key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn submitted_reviews(answer: &Json, target: &PullTarget) -> Vec<GithubItem> {
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|review| {
            let body = text(review, "/body").unwrap_or_default();
            Some(GithubItem {
                id: format!("review:{}", id(review)?),
                kind: GithubKind::Review,
                repository: target.repository.clone(),
                pull_request: target.number,
                branch: Some(target.branch.clone()),
                author: text(review, "/user/login"),
                title: text(review, "/state")
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .replace('_', " "),
                body,
                url: text(review, "/html_url"),
                at: text(review, "/submitted_at")?,
            })
        })
        .collect()
}

/// The comments of an issue `comments` answer, at their latest edit.
pub(crate) fn parse_comments(answer: &Json, target: &PullTarget) -> Vec<GithubItem> {
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|comment| {
            Some(GithubItem {
                id: format!("comment:{}", id(comment)?),
                kind: GithubKind::Comment,
                repository: target.repository.clone(),
                pull_request: target.number,
                branch: Some(target.branch.clone()),
                author: text(comment, "/user/login"),
                title: "comment".to_owned(),
                body: text(comment, "/body").unwrap_or_default(),
                url: text(comment, "/html_url"),
                at: text(comment, "/updated_at").or_else(|| text(comment, "/created_at"))?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = include_str!("../tests/fixtures/github/notifications.http");
    const NOT_MODIFIED: &str = include_str!("../tests/fixtures/github/notifications-304.http");
    const CHECK_RUNS: &str = include_str!("../tests/fixtures/github/check-runs.json");
    const REVIEWS: &str = include_str!("../tests/fixtures/github/reviews.json");
    const COMMENTS: &str = include_str!("../tests/fixtures/github/comments.json");
    const PULL: &str = include_str!("../tests/fixtures/github/pull.json");
    const PAGE_1: &str = include_str!("../tests/fixtures/github/notifications-page1.http");
    const PAGE_2: &str = include_str!("../tests/fixtures/github/notifications-page2.http");
    const ENDLESS: &str = include_str!("../tests/fixtures/github/comments-endless.http");

    /// The feed's answer as `gh api --include` prints it.
    fn parse_feed(output: &str) -> Result<Feed, GithubError> {
        feed_of(parse_answer(output)?)
    }

    /// A JSON fixture as `gh api --include` prints it.
    fn ok(body: &str) -> String {
        format!("HTTP/2.0 200 OK\nContent-Type: application/json\n\n{body}")
    }

    fn target() -> PullTarget {
        PullTarget {
            repository: "acme/widgets".into(),
            branch: "threadlane/T-0001".into(),
            number: Some(42),
        }
    }

    #[test]
    fn the_feed_gives_its_items_cursor_and_interval() {
        let feed = parse_feed(FEED).unwrap();
        assert!(!feed.not_modified);
        assert_eq!(
            feed.cursor.as_deref(),
            Some("Tue, 17 Nov 2026 08:58:12 GMT")
        );
        assert_eq!(feed.poll_interval, Some(Duration::from_secs(60)));
        let kinds: Vec<_> = feed.items.iter().map(|item| item.kind).collect();
        assert_eq!(
            kinds,
            [
                GithubKind::ReviewRequest,
                GithubKind::Mention,
                GithubKind::CheckResult,
                GithubKind::Comment,
                GithubKind::Notification,
            ]
        );
        let review = &feed.items[0];
        assert_eq!(review.id, "notification:12000000001:2026-11-17T08:58:12Z");
        assert_eq!(review.repository, "acme/widgets");
        assert_eq!(review.pull_request, Some(42));
        assert_eq!(review.branch, None);
        assert_eq!(review.title, "Add the settings page");
        assert_eq!(review.at, "2026-11-17T08:58:12Z");
        assert_eq!(feed.items[1].pull_request, None, "an issue");
        assert_eq!(
            feed.items[2].branch.as_deref(),
            Some("threadlane/T-0001"),
            "a workflow run names its branch"
        );
        assert_eq!(feed.items[3].repository, "other/thing");
        assert_eq!(feed.items[4].title, "Document the API (assign)");
    }

    #[test]
    fn an_unchanged_feed_is_not_modified() {
        let feed = parse_feed(NOT_MODIFIED).unwrap();
        assert!(feed.not_modified);
        assert!(feed.items.is_empty());
        assert_eq!(feed.poll_interval, Some(Duration::from_secs(120)));
        assert!(parse_feed("HTTP/2.0 401 Unauthorized\n\n{}").is_err());
        assert!(parse_feed("").is_err());
    }

    #[test]
    fn finished_check_runs_become_check_results() {
        let answer: Json = serde_json::from_str(CHECK_RUNS).unwrap();
        assert_eq!(
            settled_check_runs(&answer, &target()).len(),
            1,
            "for a task, the passed check waits for the run in progress"
        );
        let items = parse_check_runs(&answer, &target());
        assert_eq!(items.len(), 2, "the run in progress is left out");
        let failed = &items[0];
        assert_eq!(failed.id, "check_run:31000000001");
        assert_eq!(failed.kind, GithubKind::CheckResult);
        assert_eq!(failed.title, "test: failure");
        assert_eq!(
            failed.body,
            "2 tests failed\nsettings::saves_the_form and settings::shows_errors failed."
        );
        assert_eq!(failed.author.as_deref(), Some("github-actions"));
        assert_eq!(failed.pull_request, Some(42));
        assert_eq!(failed.branch.as_deref(), Some("threadlane/T-0001"));
        assert_eq!(failed.at, "2026-11-17T08:30:40Z");
        assert_eq!(items[1].title, "lint: success");
        assert_eq!(items[1].body, "");

        // Once nothing is running, the passed check reaches the task too.
        let mut answer = answer;
        for run in answer["check_runs"].as_array_mut().unwrap() {
            if run["status"] != "completed" {
                run["status"] = "completed".into();
                run["conclusion"] = "success".into();
                run["completed_at"] = "2026-11-17T08:40:00Z".into();
            }
        }
        assert_eq!(settled_check_runs(&answer, &target()).len(), 3);
    }

    #[test]
    fn submitted_reviews_and_comments_are_items() {
        let reviews = parse_reviews(&serde_json::from_str(REVIEWS).unwrap(), &target());
        assert_eq!(
            reviews.len(),
            1,
            "only the external submitted review is ingested"
        );
        assert_eq!(reviews[0].id, "review:80");
        assert_eq!(reviews[0].kind, GithubKind::Review);
        assert_eq!(reviews[0].title, "changes requested");
        assert_eq!(reviews[0].author.as_deref(), Some("octocat"));
        assert_eq!(reviews[0].body, "Please keep the old setting working too.");
        let comments = parse_comments(&serde_json::from_str(COMMENTS).unwrap(), &target());
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, "comment:1700000001");
        assert_eq!(comments[0].kind, GithubKind::Comment);
        assert_eq!(comments[0].author.as_deref(), Some("stranger"));
        assert_eq!(comments[0].at, "2026-11-17T08:52:30Z", "at its latest edit");
    }

    #[test]
    fn names_and_branches_cannot_change_the_path() {
        assert!(valid_full_name("acme/widgets"));
        assert!(valid_full_name("a-b/c.d_e"));
        for bad in [
            "acme",
            "acme/widgets/x",
            "../x",
            "acme/..",
            "acme/w?x=1",
            "/x",
        ] {
            assert!(!valid_full_name(bad), "{bad}");
        }
        assert_eq!(encode("threadlane/T-0001"), "threadlane%2FT-0001");
        assert_eq!(encode("2026-11-17T08:00:00Z"), "2026-11-17T08%3A00%3A00Z");
    }

    /// A fake `gh` that logs its arguments and prints the file of the first
    /// `(shell pattern, file, content)` route its path matches, failing as
    /// `gh` does on a 304 when `exit1` exists.
    #[cfg(unix)]
    fn fake_gh_with(dir: &std::path::Path, routes: &[(&str, &str, String)]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let mut script = r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/calls.log"
for arg in "$@"; do last="$arg"; done
case "$last" in
"#
        .to_owned();
        for (pattern, file, content) in routes {
            script.push_str(&format!("  {pattern}) cat \"$dir/{file}\" ;;\n"));
            std::fs::write(dir.join(file), content).unwrap();
        }
        script.push_str("  *) exit 2 ;;\nesac\nif [ -f \"$dir/exit1\" ]; then exit 1; fi\n");
        let program = dir.join("gh");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    /// The fake `gh` of the plain fixtures, with `feed` as the feed.
    #[cfg(unix)]
    fn fake_gh(dir: &std::path::Path, feed: &str) -> PathBuf {
        fake_gh_with(
            dir,
            &[
                ("notifications*", "notifications.http", feed.to_owned()),
                ("*check-runs*", "check-runs.http", ok(CHECK_RUNS)),
                ("*/reviews*", "reviews.http", ok(REVIEWS)),
                ("*/comments*", "comments.http", ok(COMMENTS)),
            ],
        )
    }

    fn calls(dir: &std::path::Path) -> Vec<String> {
        let log = std::fs::read_to_string(dir.join("calls.log")).unwrap();
        log.lines().map(str::to_owned).collect()
    }

    #[test]
    fn only_a_next_link_on_the_same_host_is_followed() {
        assert_eq!(
            next_link(
                r#"<https://api.github.com/x?page=1>; rel="prev", <https://api.github.com/x?page=3>; rel="next""#
            )
            .as_deref(),
            Some("x?page=3")
        );
        assert_eq!(parse_answer(PAGE_2).unwrap().next, None, "the last page");
        for bad in [
            r#"<https://api.github.com/x?page=2>; rel="last""#,
            r#"<http://api.github.com/x?page=2>; rel="next""#,
            r#"<https://api.github.com/--method=POST>; rel="next""#,
            r#"<https://api.github.com//x>; rel="next""#,
            r#"<https://api.github.com/x y>; rel="next""#,
            r#"https://api.github.com/x; rel="next""#,
        ] {
            assert_eq!(next_link(bad), None, "{bad}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_feed_follows_next_to_its_last_page() {
        let dir = tempfile::tempdir().unwrap();
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[
                ("*page=2*", "page2.http", PAGE_2.to_owned()),
                ("notifications*", "page1.http", PAGE_1.to_owned()),
            ],
        ));
        let feed = source
            .notifications(Some("Tue, 17 Nov 2026 08:00:00 GMT"))
            .await
            .unwrap();
        let titles: Vec<_> = feed.items.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(titles, ["Page one", "Page two"]);
        assert_eq!(
            feed.cursor.as_deref(),
            Some("Tue, 17 Nov 2026 08:58:12 GMT")
        );
        assert_eq!(
            calls(dir.path()),
            [
                "api --method GET --include -H If-Modified-Since: Tue, 17 Nov 2026 08:00:00 GMT \
                 -H Accept: application/vnd.github+json notifications?per_page=50",
                "api --method GET --include -H Accept: application/vnd.github+json \
                 notifications?per_page=50&page=2",
            ],
            "the cursor goes with the first page only, and page 2 links nowhere next"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_call_reads_at_most_ten_pages() {
        let dir = tempfile::tempdir().unwrap();
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[
                ("*check-runs*", "check-runs.http", ok(CHECK_RUNS)),
                ("*/reviews*", "reviews.http", ok(REVIEWS)),
                ("*/comments*", "comments.http", ENDLESS.to_owned()),
            ],
        ));
        let items = source.pull_request(&target(), None).await.unwrap();
        let comments = items
            .iter()
            .filter(|item| item.kind == GithubKind::Comment)
            .count();
        assert_eq!(comments, MAX_PAGES);
        let calls = calls(dir.path());
        assert_eq!(calls.len(), 2 + MAX_PAGES);
        assert!(
            calls[3..].iter().all(|call| call
                .ends_with("repositories/1296269/issues/42/comments?per_page=100&page=2")),
            "{calls:?}"
        );
        for call in &calls {
            assert!(call.starts_with("api --method GET --include "), "{call}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pull_request_known_by_number_is_read_on_its_head_commit() {
        let dir = tempfile::tempdir().unwrap();
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[
                ("*/pulls/529", "pull.http", ok(PULL)),
                ("*check-runs*", "check-runs.http", ok(CHECK_RUNS)),
                ("*/reviews*", "reviews.http", ok(REVIEWS)),
                ("*/comments*", "comments.http", ok(COMMENTS)),
            ],
        ));
        let by_number = PullTarget {
            repository: "acme/widgets".into(),
            branch: String::new(),
            number: Some(529),
        };
        let items = source.pull_request(&by_number, None).await.unwrap();
        let check = items
            .iter()
            .find(|item| item.kind == GithubKind::CheckResult)
            .unwrap();
        assert_eq!(
            check.branch.as_deref(),
            Some("codex/agent-model-preferences")
        );
        assert_eq!(check.pull_request, Some(529));
        let first = calls(dir.path());
        assert_eq!(
            first[..2],
            [
                "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/pulls/529",
                "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/commits/6dcb09b5b57875f334f61aebed695e2e4193db5e/check-runs?per_page=100",
            ]
        );
        assert_eq!(first.len(), 4, "one lookup, then the usual three reads");

        source.pull_request(&target(), None).await.unwrap();
        assert_eq!(
            calls(dir.path())[4],
            "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/commits/threadlane%2FT-0001/check-runs?per_page=100",
            "a known branch needs no lookup"
        );
    }

    #[cfg(unix)]
    async fn activity(source: &GhSource, notification: &GithubItem) -> Vec<GithubItem> {
        let mut items = Vec::new();
        source.activity(notification, &mut items).await.unwrap();
        items
    }

    #[test]
    fn a_workflow_run_notification_names_its_check_and_result() {
        let feed = parse_feed(FEED).unwrap();
        assert_eq!(feed.items[2].title, "CI: failed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pull_request_notification_reports_its_head_review_and_comment() {
        let dir = tempfile::tempdir().unwrap();
        // The reviews without Threadlane's own, which is the latest.
        let mut human: Json = serde_json::from_str(REVIEWS).unwrap();
        human
            .as_array_mut()
            .unwrap()
            .retain(|review| review["id"] != 82);
        let human = human.to_string();
        let routes = |pull: &str, reviews: &str| {
            [
                ("*/pulls/42", "pull.http", ok(pull)),
                (
                    "*/events?per_page=100",
                    "events.http",
                    ok(
                        r#"[{"id":53,"event":"review_requested","actor":{"login":"ada"},"requested_reviewer":{"login":"pat"},"created_at":"2026-11-17T08:59:00Z"}]"#,
                    ),
                ),
                ("*/reviews*", "reviews.http", ok(reviews)),
                ("*/comments*", "comments.http", ok(COMMENTS)),
            ]
        };
        let source = GhSource::with_gh(fake_gh_with(dir.path(), &routes(PULL, &human)));
        let notification = parse_feed(FEED).unwrap().items.remove(0);
        let items = activity(&source, &notification).await;
        let sha = "6dcb09b5b57875f334f61aebed695e2e4193db5e";
        let ids: Vec<_> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                notification.id.as_str(),
                "review:80",
                "comment:1700000001",
                "review_request:53",
            ],
            "the notification, then the underlying GitHub activity ids"
        );
        let head = &items[0];
        assert_eq!(head.kind, GithubKind::PullRequestUpdate);
        assert_eq!(head.title, format!("head {sha}"));
        assert_eq!(head.pull_request, Some(42));
        assert_eq!(
            head.body,
            "Add the settings page (open)\nNotified as a review request."
        );
        let review = &items[1];
        assert_eq!(review.kind, GithubKind::Review);
        assert_eq!(review.author.as_deref(), Some("octocat"));
        assert_eq!(review.title, "changes requested");
        let comment = &items[2];
        assert_eq!(comment.kind, GithubKind::Comment);
        assert_eq!(comment.author.as_deref(), Some("stranger"));
        assert_eq!(
            calls(dir.path())[0],
            "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/pulls/42"
        );

        // When Threadlane's own review is the latest, an older one is no news.
        let source = GhSource::with_gh(fake_gh_with(dir.path(), &routes(PULL, REVIEWS)));
        let items = activity(&source, &notification).await;
        assert!(items.iter().all(|item| item.kind != GithubKind::Review));

        // The head as it is now: pushed, then merged.
        let pushed = PULL.replace(sha, "1111111111111111111111111111111111111111");
        let merged = pushed.replace(
            "\"state\": \"open\",",
            "\"state\": \"closed\", \"merged\": true,",
        );
        for (pull, state) in [(pushed, "open"), (merged, "merged")] {
            let source = GhSource::with_gh(fake_gh_with(dir.path(), &routes(&pull, &human)));
            let items = activity(&source, &notification).await;
            assert_eq!(
                items[0].title,
                "head 1111111111111111111111111111111111111111"
            );
            assert!(items[0].body.contains(&format!("({state})")));
        }

        // Anything not about a pull request reports itself.
        let issue = parse_feed(FEED).unwrap().items.remove(1);
        assert_eq!(activity(&source, &issue).await, [issue]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn comment_lookup_keeps_partial_results_and_uses_the_latest_edit_across_pages() {
        let dir = tempfile::tempdir().unwrap();
        let first = r#"[{"id":1,"user":{"login":"ada"},"updated_at":"2026-11-17T08:00:00Z"}]"#;
        let second = r#"[{"id":2,"user":{"login":"pat"},"updated_at":"2026-11-17T08:30:00Z"}]"#;
        let page_one = |body: &str| {
            format!(
                "HTTP/2.0 200 OK\nLink: <https://api.github.com/repos/acme/widgets/issues/42/comments?page=2>; rel=\"next\"\n\n{body}"
            )
        };
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[
                ("*/pulls/42", "pull.http", ok(PULL)),
                (
                    "*/reviews*",
                    "reviews.http",
                    ok(r#"[{"id":8,"state":"APPROVED","submitted_at":"2026-11-17T07:00:00Z"}]"#),
                ),
                ("*/comments?per_page=100", "first.http", page_one(first)),
                (
                    "*/comments?page=2",
                    "second.http",
                    "HTTP/2.0 503 Unavailable\n\n".into(),
                ),
            ],
        ));
        let mut notification = parse_feed(FEED).unwrap().items.remove(0);
        notification.kind = GithubKind::Mention;
        assert!(notification.url.is_none());
        let mut partial = Vec::new();
        source
            .activity(&notification, &mut partial)
            .await
            .unwrap_err();
        assert_eq!(
            partial.iter().map(|item| item.kind).collect::<Vec<_>>(),
            [GithubKind::PullRequestUpdate, GithubKind::Review]
        );
        std::fs::write(dir.path().join("second.http"), ok(second)).unwrap();
        let items = activity(&source, &notification).await;
        assert_eq!(items[2].author.as_deref(), Some("pat"));
        std::fs::write(
            dir.path().join("first.http"),
            page_one(&first.replace("08:00", "09:00")),
        )
        .unwrap();
        let items = activity(&source, &notification).await;
        assert_eq!(items[2].author.as_deref(), Some("ada"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pull_request_activity_names_checks_and_follows_the_notified_line_comment() {
        let dir = tempfile::tempdir().unwrap();
        let line = r#"{"id":9,"user":{"login":"ada"},"body":"On this line","updated_at":"2026-11-17T09:00:00Z"}"#;
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[
                ("*/pulls/42", "pull.http", ok(PULL)),
                ("*/check-runs*", "checks.http", ok(CHECK_RUNS)),
                ("*/reviews*", "reviews.http", ok("[]")),
                ("*/pulls/comments/9", "line.http", ok(line)),
            ],
        ));
        let mut notification = parse_feed(FEED).unwrap().items.remove(0);
        notification.kind = GithubKind::CheckResult;
        let items = activity(&source, &notification).await;
        let checks: Vec<_> = items
            .iter()
            .filter(|item| item.kind == GithubKind::CheckResult)
            .map(|item| item.title.as_str())
            .collect();
        assert_eq!(checks, ["test: failure", "lint: success"]);

        let feed = FEED
            .replace("other/thing", "acme/widgets")
            .replace("/pulls/7\"", "/pulls/42\"")
            .replace("/issues/comments/9\"", "/pulls/comments/9\"");
        let notification = parse_feed(&feed).unwrap().items.remove(3);
        let items = activity(&source, &notification).await;
        assert_eq!(items[1].kind, GithubKind::Comment);
        assert_eq!(items[1].title, "inline review comment");
        assert_eq!(items[1].author.as_deref(), Some("ada"));
        assert!(
            comment_path(
                "https://api.github.com/repos/other/thing/pulls/comments/9",
                "acme/widgets"
            )
            .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_issue_claim_reads_the_issue_its_open_referencing_pulls_and_the_viewer() {
        let dir = tempfile::tempdir().unwrap();
        let reference = |number: u64, state: &str, pull: bool, author: &str| {
            let pull = if pull { r#","pull_request":{}"# } else { "" };
            format!(
                r#"{{"event":"cross-referenced","source":{{"issue":{{"number":{number},"state":"{state}","user":{{"login":"{author}"}}{pull}}}}}}}"#
            )
        };
        let first = format!(
            "HTTP/2.0 200 OK\nLink: <https://api.github.com/repositories/1/issues/7/timeline?per_page=100&page=2>; rel=\"next\"\n\n[{},{},{}]",
            reference(12, "open", true, "bob"),
            reference(13, "closed", true, "carol"),
            reference(14, "open", false, "dave"),
        );
        let second = ok(&format!(
            r#"[{},{{"event":"labeled"}}]"#,
            reference(15, "open", true, "me")
        ));
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[
                (
                    "repos/acme/widgets/issues/7",
                    "issue.http",
                    ok(r#"{"state":"open","assignees":[{"login":"alice"}]}"#),
                ),
                ("*page=2", "timeline2.http", second),
                ("*/timeline*", "timeline1.http", first),
                ("user", "user.http", ok(r#"{"login":"me"}"#)),
            ],
        ));
        let claim = source.issue_claim("acme/widgets", 7).await.unwrap();
        assert_eq!(
            claim,
            IssueClaim {
                viewer: "me".into(),
                is_pull_request: false,
                open: true,
                assignees: vec!["alice".into()],
                open_pull_requests: vec![(12, "bob".into()), (15, "me".into())],
            }
        );
        let made = calls(dir.path());
        assert_eq!(made.len(), 4);
        for call in &made {
            assert!(
                call.starts_with("api --method GET --include -H Accept: "),
                "{call}"
            );
            for write in [
                " -f",
                " -F",
                "--field",
                "--raw-field",
                "--input",
                "-X",
                "POST",
                "PATCH",
            ] {
                assert!(!call.contains(write), "{call} contains {write}");
            }
        }
        assert!(
            source.issue_claim("acme/../x", 7).await.is_err(),
            "a bad name never reaches gh"
        );
        assert_eq!(calls(dir.path()).len(), 4);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_issue_view_reads_the_issue_and_its_comments_with_gets_only() {
        let dir = tempfile::tempdir().unwrap();
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[
                (
                    "repos/acme/widgets/issues/7",
                    "issue.http",
                    ok(r#"{"title":"Crash","state":"open","body":"It breaks.",
                        "html_url":"https://github.com/acme/widgets/pull/7","pull_request":{},
                        "user":{"login":"alice"},"labels":[{"name":"bug"}]}"#),
                ),
                (
                    "*/comments*",
                    "comments.http",
                    ok(
                        r#"[{"user":{"login":"bob"},"created_at":"2026-11-22T00:00:00Z",
                        "body":"Plan: fix it."}]"#,
                    ),
                ),
            ],
        ));
        let view = source.issue("acme/widgets", 7).await.unwrap();
        assert_eq!(
            view,
            serde_json::json!({
                "number": 7,
                "url": "https://github.com/acme/widgets/pull/7",
                "is_pull_request": true,
                "title": "Crash",
                "state": "open",
                "author": "alice",
                "labels": ["bug"],
                "body": "It breaks.",
                "comments": [
                    {"author": "bob", "at": "2026-11-22T00:00:00Z", "body": "Plan: fix it."}
                ],
            })
        );
        for call in calls(dir.path()) {
            assert!(
                call.starts_with("api --method GET --include -H Accept: "),
                "{call}"
            );
        }
        assert!(source.issue("acme/../x", 7).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pull_request_state_reads_its_head_mergeability_and_every_check_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut pull: Json = serde_json::from_str(PULL).unwrap();
        pull["head"]["repo"] = serde_json::json!({"full_name": "acme/widgets"});
        pull["draft"] = true.into();
        pull["mergeable_state"] = "CLEAN".into();
        pull["html_url"] = "https://github.com/acme/widgets/pull/529".into();
        pull["merged"] = false.into();
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[
                ("*/pulls/529", "pull.http", ok(&pull.to_string())),
                ("*check-runs*", "check-runs.http", ok(CHECK_RUNS)),
            ],
        ));
        let state = source
            .pull_request_state("acme/widgets", 529)
            .await
            .unwrap();
        assert_eq!(
            state,
            PullRequestState {
                head_repository: "acme/widgets".into(),
                head_ref: "codex/agent-model-preferences".into(),
                head_sha: "6dcb09b5b57875f334f61aebed695e2e4193db5e".into(),
                draft: true,
                mergeable_state: "clean".into(),
                checks: vec![
                    CheckRun {
                        name: "test".into(),
                        conclusion: Some("failure".into())
                    },
                    CheckRun {
                        name: "lint".into(),
                        conclusion: Some("success".into())
                    },
                    CheckRun {
                        name: "build (macos)".into(),
                        conclusion: None
                    },
                ],
                url: "https://github.com/acme/widgets/pull/529".into(),
                state: "open".into(),
                merged: false,
            }
        );
        assert!(!state.green(), "a failed and a running check");
        assert_eq!(
            calls(dir.path()),
            [
                "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/pulls/529",
                "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/commits/6dcb09b5b57875f334f61aebed695e2e4193db5e/check-runs?per_page=100",
            ]
        );
        assert!(source.pull_request_state("acme/../x", 529).await.is_err());
        assert!(source.pull_request_state("acme/widgets", 7).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unresolved_review_threads_come_from_the_fixed_graphql_query() {
        let dir = tempfile::tempdir().unwrap();
        let answer = serde_json::json!({"data": {"repository": {"pullRequest": {"reviewThreads": {"nodes": [
            {"id": "T1", "isResolved": false, "isOutdated": false, "path": "src/a.rs", "line": 3,
             "comments": {"nodes": [{"author": {"login": "rabbit"}, "body": "Handle None."}]}},
            {"id": "T2", "isResolved": true, "isOutdated": false, "path": "src/b.rs", "line": 9,
             "comments": {"nodes": [{"author": {"login": "rabbit"}, "body": "Done."}]}},
        ]}}}}});
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[("graphql", "graphql.json", answer.to_string())],
        ));
        assert_eq!(
            source
                .unresolved_review_threads("acme/widgets", 42)
                .await
                .unwrap(),
            [
                serde_json::json!({"id": "T1", "path": "src/a.rs", "line": 3, "outdated": false,
                "author": "rabbit", "body": "Handle None."})
            ]
        );
        let call = &calls(dir.path())[0];
        assert!(call.starts_with("api -f query=query("), "{call}");
        assert!(!call.contains("mutation"), "{call}");
        assert!(
            call.ends_with("-f owner=acme -f name=widgets -F number=42 graphql"),
            "{call}"
        );
        assert!(
            source
                .unresolved_review_threads("acme/../x", 42)
                .await
                .is_err()
        );
        let errors = serde_json::json!({"errors": [{"message": "no"}]});
        std::fs::write(dir.path().join("graphql.json"), errors.to_string()).unwrap();
        assert!(
            source
                .unresolved_review_threads("acme/widgets", 42)
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pull_request_files_read_each_path_status_count_and_patch() {
        let dir = tempfile::tempdir().unwrap();
        let files = serde_json::json!([
            {"filename": "src/a.rs", "status": "modified", "additions": 2, "deletions": 1,
             "patch": "@@ -1 +1,2 @@\n-a\n+b\n+c"},
            {"filename": "logo.png", "status": "renamed", "previous_filename": "old.png",
             "additions": 0, "deletions": 0},
        ]);
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[("*/files*", "files.http", ok(&files.to_string()))],
        ));
        assert_eq!(
            source
                .pull_request_files("acme/widgets", 529)
                .await
                .unwrap(),
            [
                PullRequestFile {
                    path: "src/a.rs".into(),
                    previous_path: None,
                    status: "modified".into(),
                    additions: 2,
                    deletions: 1,
                    patch: Some("@@ -1 +1,2 @@\n-a\n+b\n+c".into()),
                },
                PullRequestFile {
                    path: "logo.png".into(),
                    previous_path: Some("old.png".into()),
                    status: "renamed".into(),
                    additions: 0,
                    deletions: 0,
                    patch: None,
                },
            ]
        );
        assert_eq!(
            calls(dir.path()),
            [
                "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/pulls/529/files?per_page=100"
            ]
        );
        assert!(source.pull_request_files("acme/../x", 529).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pull_request_state_with_unread_check_pages_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut pull: Json = serde_json::from_str(PULL).unwrap();
        pull["head"]["repo"] = serde_json::json!({"full_name": "acme/widgets"});
        let source = GhSource::with_gh(fake_gh_with(
            dir.path(),
            &[
                ("*/pulls/529", "pull.http", ok(&pull.to_string())),
                ("*check-runs*", "check-runs.http", ENDLESS.to_owned()),
                // Where the endless fixture's next link points.
                ("*/comments*", "comments.http", ENDLESS.to_owned()),
            ],
        ));
        let error = source
            .pull_request_state("acme/widgets", 529)
            .await
            .unwrap_err();
        assert!(error.0.contains("not all were read"), "{error}");
        assert_eq!(calls(dir.path()).len(), 1 + MAX_PAGES);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn gh_source_only_reads_and_never_marks_anything_read() {
        let dir = tempfile::tempdir().unwrap();
        let source = GhSource::with_gh(fake_gh(dir.path(), FEED));
        let feed = source.notifications(None).await.unwrap();
        assert_eq!(feed.items.len(), 5);
        let since = "2026-11-17T08:30:00Z";
        let items = source.pull_request(&target(), Some(since)).await.unwrap();
        let ids: Vec<_> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            ["check_run:31000000001", "review:80", "comment:1700000001"],
            "since the cursor, oldest first"
        );
        let feed = source.notifications(feed.cursor.as_deref()).await.unwrap();
        assert_eq!(feed.items.len(), 5);

        std::fs::write(dir.path().join("notifications.http"), NOT_MODIFIED).unwrap();
        std::fs::write(dir.path().join("exit1"), "").unwrap();
        let feed = source
            .notifications(Some("Tue, 17 Nov 2026 08:58:12 GMT"))
            .await
            .unwrap();
        assert!(feed.not_modified, "gh fails on a 304, which is an answer");
        assert!(source.pull_request(&target(), None).await.is_err());

        let calls = calls(dir.path());
        assert_eq!(
            calls[..4],
            [
                "api --method GET --include -H Accept: application/vnd.github+json notifications?per_page=50",
                "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/commits/threadlane%2FT-0001/check-runs?per_page=100",
                "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/pulls/42/reviews?per_page=100",
                "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/issues/42/comments?per_page=100&since=2026-11-17T08%3A30%3A00Z",
            ]
        );
        assert_eq!(
            calls[4],
            "api --method GET --include -H If-Modified-Since: Tue, 17 Nov 2026 08:58:12 GMT \
             -H Accept: application/vnd.github+json notifications?per_page=50"
        );
        for call in &calls {
            assert!(call.starts_with("api --method GET "), "{call}");
            for write in [
                " -f",
                " -F",
                "--field",
                "--raw-field",
                "--input",
                "PATCH",
                "PUT",
                "POST",
                "DELETE",
                "threads",
                "-X",
            ] {
                assert!(!call.contains(write), "{call} contains {write}");
            }
        }
    }
}
