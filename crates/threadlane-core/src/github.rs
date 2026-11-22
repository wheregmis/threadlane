//! The GitHub source (Plan.md §6.6, docs/milestone-8.md §1 decisions 6 to 8,
//! §3): what the hub pulls from GitHub, and how a remote names a repository.
//!
//! The hub reaches GitHub only through a [`GithubSource`]; the real one runs
//! `gh api` with read-only calls. It never marks a notification read and
//! never writes to GitHub.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The `source` of every event the GitHub source inserts.
pub const GITHUB_SOURCE: &str = "github";

/// Prefix of the hidden footer the runner appends to each published review.
pub const REVIEW_MARKER_PREFIX: &str = "<!-- threadlane-review-";

/// How long the poller waits between passes when the service names no
/// interval.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// What a GitHub item is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubKind {
    /// A check run finished, or a notification about workflow runs.
    CheckResult,
    /// A review on a pull request.
    Review,
    /// A comment on a pull request or issue.
    Comment,
    /// You were asked to review a pull request.
    ReviewRequest,
    /// You or your team were mentioned.
    Mention,
    /// Any other notification: assigned, state changed, subscribed, ...
    Notification,
    /// A pull request's head commit or state as a notification found it.
    PullRequestUpdate,
    /// The observed head commit of a pull request changed.
    Push,
}

impl GithubKind {
    /// The kind as a run reads it, with its article.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::CheckResult => "a check result",
            Self::Review => "a review",
            Self::Comment => "a comment",
            Self::ReviewRequest => "a review request",
            Self::Mention => "a mention",
            Self::Notification => "a notification",
            Self::PullRequestUpdate => "a pull request update",
            Self::Push => "a push",
        }
    }
}

/// One thing that happened on GitHub, normalized. Stored as the payload of
/// its event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubItem {
    /// The service's own identifier, unique per item: the event's
    /// `source_event_id`.
    pub id: String,
    pub kind: GithubKind,
    /// `owner/name`, as GitHub names it.
    pub repository: String,
    /// The pull request's number, when the item is about one.
    pub pull_request: Option<u64>,
    /// The branch, when the item is about one (a check on a branch).
    pub branch: Option<String>,
    /// Who did it: a login, or a check's app.
    pub author: Option<String>,
    /// One line: a notification's subject, a check's name and conclusion, a
    /// review's state.
    pub title: String,
    /// The content, as written on GitHub.
    pub body: String,
    /// Where to see it on GitHub.
    pub url: Option<String>,
    /// When it happened, as GitHub states it (RFC 3339, UTC).
    pub at: String,
}

/// One answer of the notifications feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feed {
    /// The feed has not changed since the cursor; `items` is empty.
    pub not_modified: bool,
    pub items: Vec<GithubItem>,
    /// The cursor to ask with next time, if the answer gave one.
    pub cursor: Option<String>,
    /// How long the service asks to be left alone before the next request.
    pub poll_interval: Option<Duration>,
}

/// A task's work on GitHub: its pushed branch and, once opened, its pull
/// request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PullTarget {
    /// `owner/name`.
    pub repository: String,
    /// Empty when only the pull request's number is known; the source
    /// looks up its head branch.
    pub branch: String,
    /// The pull request's number, once one is open.
    pub number: Option<u64>,
}

/// Who holds an issue, as `assign_issue` checks it before asking.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IssueClaim {
    /// The login `gh` reads as.
    pub viewer: String,
    /// The number names a pull request, not an issue.
    pub is_pull_request: bool,
    /// The issue is open.
    pub open: bool,
    /// The logins it is assigned to.
    pub assignees: Vec<String>,
    /// Open pull requests that reference it, as `(number, author login)`.
    pub open_pull_requests: Vec<(u64, String)>,
}

/// A pull request as the gate checks it before a task's own step on it
/// (docs/autonomy-rework.md §3 stage A).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PullRequestState {
    /// `owner/name` of the repository the head branch is in, which differs
    /// from the pull request's for a fork.
    pub head_repository: String,
    pub head_ref: String,
    /// The head commit's full SHA.
    pub head_sha: String,
    pub draft: bool,
    /// GitHub's `mergeable_state`, lowercased: `clean` merges without
    /// conflicts and with every status passing.
    pub mergeable_state: String,
    /// The conclusion of each check run on the head commit, `None` while
    /// one has not finished.
    pub checks: Vec<Option<String>>,
}

impl PullRequestState {
    /// It merges cleanly and has checks, each finished as success, neutral
    /// or skipped. A repository without checks is never green.
    #[must_use]
    pub fn green(&self) -> bool {
        self.mergeable_state == "clean"
            && !self.checks.is_empty()
            && self.checks.iter().all(|conclusion| {
                matches!(
                    conclusion.as_deref(),
                    Some("success" | "neutral" | "skipped")
                )
            })
    }
}

/// Why a call to GitHub failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("github: {0}")]
pub struct GithubError(pub String);

/// Reads GitHub for the hub (Plan.md §6.6). Read-only: no implementation
/// marks anything read or writes anything.
#[async_trait]
pub trait GithubSource: Send + Sync + fmt::Debug {
    /// Your notifications feed, as it is since `cursor` (the previous
    /// answer's [`Feed::cursor`]), unread or not.
    async fn notifications(&self, cursor: Option<&str>) -> Result<Feed, GithubError>;

    /// The finished check results on the target's branch, and, if it has a
    /// pull request, its reviews and comments, each at or after `since` (an
    /// RFC 3339 time, every one when `None`), oldest first.
    async fn pull_request(
        &self,
        target: &PullTarget,
        since: Option<&str>,
    ) -> Result<Vec<GithubItem>, GithubError>;

    /// Append the PR head under the notification's id, then any known details
    /// under their GitHub activity ids. Keep the items already read on error
    /// so the hub can deliver them and retry the missing details.
    async fn activity(
        &self,
        notification: &GithubItem,
        items: &mut Vec<GithubItem>,
    ) -> Result<(), GithubError> {
        items.push(notification.clone());
        Ok(())
    }

    /// Who holds issue `number` of `repository` (`owner/name`).
    async fn issue_claim(&self, repository: &str, number: u64) -> Result<IssueClaim, GithubError> {
        let _ = (repository, number);
        Err(GithubError("this source cannot read issues".to_owned()))
    }

    /// Issue or pull request `number` of `repository` (`owner/name`) for
    /// reading: its title, state, author, labels, body and comments, as JSON.
    async fn issue(&self, repository: &str, number: u64) -> Result<serde_json::Value, GithubError> {
        let _ = (repository, number);
        Err(GithubError("this source cannot read issues".to_owned()))
    }

    /// Head, draft flag, mergeability and check runs of pull request
    /// `number` of `repository` (`owner/name`).
    async fn pull_request_state(
        &self,
        repository: &str,
        number: u64,
    ) -> Result<PullRequestState, GithubError> {
        let _ = (repository, number);
        Err(GithubError(
            "this source cannot read pull requests".to_owned(),
        ))
    }
}

/// A remote URL in the one form two spellings of the same repository share:
/// `host/owner/name`, lowercased, without a scheme, user, port or `.git`.
/// So `https://github.com/Acme/Widgets.git`, `git@github.com:acme/widgets`
/// and `ssh://git@github.com/acme/widgets` are all `github.com/acme/widgets`.
/// `None` for anything else, such as a local path.
#[must_use]
pub fn normalize_remote(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let (host, path) = if let Some((scheme, rest)) = remote.split_once("://") {
        if !matches!(
            scheme.to_ascii_lowercase().as_str(),
            "https" | "http" | "ssh" | "git" | "git+ssh"
        ) {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        (host.split(':').next()?, path)
    } else {
        // The scp-like form, `[user@]host:owner/name`.
        let (authority, path) = remote.split_once(':')?;
        if authority.contains('/') {
            return None;
        }
        (authority.rsplit('@').next()?, path)
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None)
            if !host.is_empty() && !owner.is_empty() && !name.is_empty() =>
        {
            Some(format!("{host}/{owner}/{name}").to_ascii_lowercase())
        }
        _ => None,
    }
}

/// The number at the end of a pull request URL, such as
/// `https://github.com/acme/widgets/pull/7` or its API form `.../pulls/7`.
#[must_use]
pub fn pull_request_number(url: &str) -> Option<u64> {
    let (rest, number) = url.trim().trim_end_matches('/').rsplit_once('/')?;
    if rest.ends_with("/pull") || rest.ends_with("/pulls") {
        number.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_and_ssh_forms_of_one_repository_are_the_same() {
        for remote in [
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets.git",
            "https://github.com/Acme/Widgets/",
            "http://github.com/acme/widgets",
            "https://someone@github.com/acme/widgets.git",
            "git@github.com:acme/widgets.git",
            "git@github.com:Acme/widgets",
            "github.com:acme/widgets",
            "ssh://git@github.com/acme/widgets.git",
            "ssh://git@github.com:22/acme/widgets",
            "git://github.com/acme/widgets.git",
            "  https://GitHub.com/acme/widgets.git\n",
        ] {
            assert_eq!(
                normalize_remote(remote).as_deref(),
                Some("github.com/acme/widgets"),
                "{remote}"
            );
        }
        assert_eq!(
            normalize_remote("git@gitlab.example:acme/widgets").as_deref(),
            Some("gitlab.example/acme/widgets")
        );
        assert_ne!(
            normalize_remote("https://github.com/acme/widgets"),
            normalize_remote("https://github.com/acme/gadgets")
        );
        for not_one in [
            "/tmp/remotes/app.git",
            "./app",
            "file:///tmp/app.git",
            "https://github.com/acme",
            "https://github.com/acme/widgets/tree/main",
            "",
        ] {
            assert_eq!(normalize_remote(not_one), None, "{not_one}");
        }
    }

    #[test]
    fn a_pull_request_is_green_only_when_clean_with_every_check_passed() {
        let green = PullRequestState {
            mergeable_state: "clean".into(),
            checks: vec![
                Some("success".into()),
                Some("skipped".into()),
                Some("neutral".into()),
            ],
            ..PullRequestState::default()
        };
        assert!(green.green());
        for (state, checks) in [
            ("unstable", vec![Some("success")]),
            ("dirty", vec![Some("success")]),
            ("blocked", vec![Some("success")]),
            ("unknown", vec![Some("success")]),
            ("clean", vec![]),
            ("clean", vec![Some("success"), None]),
            ("clean", vec![Some("success"), Some("failure")]),
            ("clean", vec![Some("cancelled")]),
            ("clean", vec![Some("timed_out")]),
            ("clean", vec![Some("action_required")]),
        ] {
            let pull = PullRequestState {
                mergeable_state: state.into(),
                checks: checks.into_iter().map(|c| c.map(str::to_owned)).collect(),
                ..PullRequestState::default()
            };
            assert!(!pull.green(), "{pull:?}");
        }
    }

    #[test]
    fn a_pull_request_number_comes_from_its_url() {
        assert_eq!(
            pull_request_number("https://github.com/acme/widgets/pull/7"),
            Some(7)
        );
        assert_eq!(
            pull_request_number("https://api.github.com/repos/acme/widgets/pulls/12"),
            Some(12)
        );
        assert_eq!(pull_request_number("https://forge.test/pull/3"), Some(3));
        assert_eq!(
            pull_request_number("https://github.com/acme/widgets/issues/7"),
            None
        );
        assert_eq!(
            pull_request_number("https://github.com/acme/widgets/pull/x"),
            None
        );
    }
}
