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
//! - a pull request's head branch, when only its number is known:
//!   `repos/<owner>/<name>/pulls/<n>`, reading `head.ref`;
//! - a branch's check runs: `repos/<owner>/<name>/commits/<branch>/check-runs?per_page=100`;
//! - a pull request's reviews: `repos/<owner>/<name>/pulls/<n>/reviews?per_page=100`;
//! - its comments: `repos/<owner>/<name>/issues/<n>/comments?per_page=100[&since=<time>]`.
//!
//! With `--include` each answer starts with the status line and headers. A
//! `Link` header's `rel="next"` is followed, as another such GET of the path
//! it names, for at most [`MAX_PAGES`] pages per call.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value as Json;
use threadlane_core::github::{
    Feed, GithubError, GithubItem, GithubKind, GithubSource, PullTarget, REVIEW_MARKER_PREFIX,
    pull_request_number,
};
use tokio::process::Command;

/// The longest one `gh api` call may take.
const GH_TIMEOUT: Duration = Duration::from_secs(60);
/// What every call accepts.
const ACCEPT: &str = "Accept: application/vnd.github+json";
/// The most pages one call reads.
const MAX_PAGES: usize = 10;

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
        let mut command = Command::new(&self.gh);
        command
            .args(["api", "--method", "GET", "--include"])
            .args(extra)
            .args(["-H", ACCEPT, path])
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
            let (ok, output) = self.get(extra, &path).await?;
            let page = match parse_answer(&output) {
                Ok(page) if ok || page.status == 304 => page,
                Ok(_) => return Err(GithubError(format!("gh api {path} failed"))),
                Err(error) if ok => return Err(error),
                Err(_) => return Err(GithubError(format!("gh api {path} failed"))),
            };
            next = page.next.clone().filter(|_| pages.len() + 1 < MAX_PAGES);
            pages.push(page);
        }
        Ok(pages)
    }

    /// The head branch of pull request `number`, read from `pulls/<n>`.
    async fn head_branch(&self, repository: &str, number: u64) -> Result<String, GithubError> {
        let path = format!("repos/{repository}/pulls/{number}");
        let (ok, output) = self.get(&[], &path).await?;
        let answer = parse_answer(&output)?;
        if !ok || !(200..=299).contains(&answer.status) {
            return Err(GithubError(format!("gh api {path} failed")));
        }
        let body: Json = serde_json::from_str(&answer.body)
            .map_err(|error| GithubError(format!("gh api {path} answered: {error}")))?;
        text(&body, "/head/ref")
            .filter(|branch| !branch.is_empty())
            .ok_or_else(|| GithubError(format!("gh api {path} names no head branch")))
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
            if !(200..=299).contains(&page.status) {
                return Err(GithubError(format!(
                    "gh api {path} answered {}",
                    page.status
                )));
            }
            let body: Json = serde_json::from_str(&page.body)
                .map_err(|error| GithubError(format!("gh api {path} answered: {error}")))?;
            items.extend(parse(&body, target));
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
        let target = match target.number {
            Some(number) if target.branch.is_empty() => {
                resolved = PullTarget {
                    branch: self.head_branch(repository, number).await?,
                    ..target.clone()
                };
                &resolved
            }
            _ => target,
        };
        let branch = encode(&target.branch);
        let mut items = self
            .get_items(
                &format!("repos/{repository}/commits/{branch}/check-runs?per_page=100"),
                target,
                parse_check_runs,
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
    let branch = match subject_type.as_str() {
        "CheckSuite" => title
            .strip_suffix(" branch")
            .and_then(|rest| rest.rsplit_once(" for "))
            .map(|(_, branch)| branch.to_owned()),
        _ => None,
    };
    if kind == GithubKind::Notification && !reason.is_empty() {
        title = format!("{title} ({})", reason.replace('_', " "));
    }
    Some(GithubItem {
        id: format!("notification:{thread}:{at}"),
        kind,
        repository: text(value, "/repository/full_name")?,
        pull_request,
        branch,
        author: None,
        title,
        body: String::new(),
        url: None,
        at,
    })
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
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|review| {
            let body = text(review, "/body").unwrap_or_default();
            let delivery_key = body
                .trim_end()
                .lines()
                .next_back()
                .and_then(|line| line.strip_prefix(REVIEW_MARKER_PREFIX))
                .and_then(|rest| rest.strip_suffix(" -->"));
            if delivery_key
                .is_some_and(|key| key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return None;
            }
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
        let items = parse_check_runs(&serde_json::from_str(CHECK_RUNS).unwrap(), &target());
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
    async fn a_pull_request_known_by_number_is_read_on_its_head_branch() {
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
                "api --method GET --include -H Accept: application/vnd.github+json repos/acme/widgets/commits/codex%2Fagent-model-preferences/check-runs?per_page=100",
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
