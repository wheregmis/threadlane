use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

mod cache;
use cache::ResponseCache;

use crate::error::GitError;
use crate::git::{command, current_branch, push};
use crate::types::{
    GitHubIssueComment, GitHubIssueDetail, GitHubIssueRef, GitHubIssueSummary, GitHubLabel,
    GitHubPrCommit, GitHubPrFile, GitHubPrInfo, GitHubPullRequestSummary, GitHubRepository,
    PrCheckStatus, PrConversationComment, PrReview, PrReviewComment, PullRequestReviewCommentDraft,
    PullRequestReviewVerdict,
};

// Background readers share results; explicit refresh and mutations invalidate them.
const PR_INSPECTION_TTL: Duration = Duration::from_secs(120);
const GITHUB_RESPONSE_TTL: Duration = Duration::from_secs(300);

type PrCacheKey = (PathBuf, String);
type GithubListCacheKey = (PathBuf, String);
type GithubIssueCacheKey = (PathBuf, u64);

static PR_CACHE: OnceLock<ResponseCache<PrCacheKey, Option<GitHubPrInfo>>> = OnceLock::new();
static ISSUE_LIST_CACHE: OnceLock<ResponseCache<GithubListCacheKey, Vec<GitHubIssueSummary>>> =
    OnceLock::new();
static PR_LIST_CACHE: OnceLock<ResponseCache<GithubListCacheKey, Vec<GitHubPullRequestSummary>>> =
    OnceLock::new();
static ISSUE_DETAIL_CACHE: OnceLock<ResponseCache<GithubIssueCacheKey, GitHubIssueDetail>> =
    OnceLock::new();

pub(crate) fn fresh_cache_value<T: Clone>(
    entry: &(Instant, T),
    now: Instant,
    ttl: Duration,
) -> Option<T> {
    (now.duration_since(entry.0) <= ttl).then(|| entry.1.clone())
}

pub(crate) fn repository_key(work_dir: &Path) -> PathBuf {
    work_dir
        .canonicalize()
        .unwrap_or_else(|_| work_dir.to_path_buf())
}

pub(crate) fn pr_cache_key(work_dir: &Path, branch: &str) -> PrCacheKey {
    (repository_key(work_dir), branch.to_owned())
}

pub(crate) fn invalidate_pr_cache(work_dir: &Path, branch: &str) {
    if let Some(cache) = PR_CACHE.get() {
        let key = pr_cache_key(work_dir, branch);
        cache.invalidate(|candidate| candidate == &key);
    }
}

/// Refresh the visible lists without expiring every session's PR inspection.
pub fn invalidate_github_list_cache(work_dir: &Path) {
    let repository = repository_key(work_dir);
    if let Some(cache) = ISSUE_LIST_CACHE.get() {
        cache.invalidate(|(path, _)| path == &repository);
    }
    if let Some(cache) = PR_LIST_CACHE.get() {
        cache.invalidate(|(path, _)| path == &repository);
    }
}

pub fn invalidate_github_detail_cache(work_dir: &Path, number: u64) {
    let repository = repository_key(work_dir);
    if let Some(cache) = ISSUE_DETAIL_CACHE.get() {
        cache.invalidate(|key| key == &(repository.clone(), number));
    }
    invalidate_pr_cache(work_dir, &number.to_string());
}

pub fn invalidate_github_cache(work_dir: &Path) {
    let repository = repository_key(work_dir);
    invalidate_github_list_cache(work_dir);
    if let Some(cache) = PR_CACHE.get() {
        cache.invalidate(|(path, _)| path == &repository);
    }
    if let Some(cache) = ISSUE_DETAIL_CACHE.get() {
        cache.invalidate(|(path, _)| path == &repository);
    }
}

fn gh_command_with_captured_token(
    work_dir: &Path,
    args: &[&str],
) -> (Command, Option<String>) {
    let stored_token = stored_github_token();
    gh_command_with_token_capture(work_dir, args, stored_token)
}

fn stored_github_token() -> Option<String> {
    threadlane_auth::load_github_credentials()
        .map(|credentials| credentials.token)
        .filter(|token| !token.trim().is_empty())
}

pub(crate) fn gh_command_with_token(
    work_dir: &Path,
    args: &[&str],
    stored_token: Option<&str>,
) -> Command {
    let mut command = Command::new("gh");
    command
        .args(args)
        .current_dir(work_dir)
        .env("GH_PROMPT_DISABLED", "1");
    if let Some(token) = stored_token.filter(|token| !token.trim().is_empty()) {
        command.env("GH_TOKEN", token);
    }
    command
}

pub(crate) fn gh_command_with_token_capture(
    work_dir: &Path,
    args: &[&str],
    stored_token: Option<String>,
) -> (Command, Option<String>) {
    let command = gh_command_with_token(work_dir, args, stored_token.as_deref());
    (command, stored_token)
}

pub(crate) fn redact_gh_failure(message: &str, stored_token: Option<&str>) -> String {
    let mut redacted = message.to_owned();
    if let Some(token) = stored_token.filter(|token| !token.is_empty()) {
        redacted = redacted.replace(token, "<redacted>");
    }
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        let prefix = format!("{name}=");
        while let Some(start) = redacted.find(&prefix) {
            let value_start = start + prefix.len();
            let value_end = redacted[value_start..]
                .find(char::is_whitespace)
                .map(|offset| value_start + offset)
                .unwrap_or(redacted.len());
            redacted.replace_range(start..value_end, &format!("{name} <redacted>"));
        }
    }
    redacted
}

pub(crate) fn gh_failure_message(stderr: &str, stored_token: Option<&str>) -> String {
    redact_gh_failure(stderr.trim(), stored_token)
}

fn gh_failure(
    work_dir: &Path,
    status: std::process::ExitStatus,
    stderr: &str,
    stored_token: Option<&str>,
) -> GitError {
    if let Some(guidance) = rate_limit_message(stderr) {
        return GitError::new(work_dir, redact_gh_failure(&guidance, stored_token));
    }
    let stderr = gh_failure_message(stderr, stored_token);
    GitError::new(
        work_dir,
        if stderr.is_empty() {
            format!("gh exited with {status}")
        } else {
            stderr
        },
    )
}

pub(crate) fn parse_gh_pr_json(json_str: &str) -> Result<GitHubPrInfo, String> {
    let val: serde_json::Value = serde_json::from_str(json_str).map_err(|e| e.to_string())?;

    let number = val["number"].as_u64().unwrap_or(0);
    let title = val["title"].as_str().unwrap_or("").to_string();
    let url = val["url"].as_str().unwrap_or("").to_string();
    let state = val["state"].as_str().unwrap_or("").to_string();
    let is_draft = val["isDraft"].as_bool().unwrap_or(false);
    let head_ref = val["headRefName"].as_str().unwrap_or("").to_string();
    let base_ref = val["baseRefName"].as_str().unwrap_or("").to_string();
    let body = val["body"].as_str().unwrap_or("").to_string();
    let author = github_login(&val["author"]);
    let updated_at = val["updatedAt"].as_str().unwrap_or("").to_string();
    let review_decision = val["reviewDecision"].as_str().map(str::to_owned);
    let head_oid = val["headRefOid"].as_str().unwrap_or("").to_string();

    let issue_comments = parse_pr_conversation_comments(&val["comments"]);
    let comments_count = issue_comments.len();

    let mut checks = Vec::new();
    let mut failing_checks = 0;
    let mut pending_checks = 0;
    let mut passing_checks = 0;

    if let Some(checks_arr) = val["statusCheckRollup"].as_array() {
        for check in checks_arr {
            let name = check["name"]
                .as_str()
                .or_else(|| check["context"].as_str())
                .unwrap_or("check")
                .to_string();
            let status = check["status"]
                .as_str()
                .or_else(|| check["state"].as_str())
                .unwrap_or("COMPLETED")
                .to_string();
            let conclusion = check["conclusion"]
                .as_str()
                .or_else(|| check["state"].as_str())
                .map(|s| s.to_string());
            let details_url = check["detailsUrl"]
                .as_str()
                .or_else(|| check["targetUrl"].as_str())
                .map(|s| s.to_string());

            let conclusion_upper = conclusion.as_deref().unwrap_or("").to_uppercase();
            let status_upper = status.to_uppercase();

            if matches!(
                conclusion_upper.as_str(),
                "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED" | "ERROR"
            ) {
                failing_checks += 1;
            } else if matches!(
                status_upper.as_str(),
                "IN_PROGRESS" | "QUEUED" | "PENDING" | "EXPECTED"
            ) || conclusion.is_none()
            {
                pending_checks += 1;
            } else if matches!(conclusion_upper.as_str(), "SUCCESS" | "NEUTRAL" | "SKIPPED") {
                passing_checks += 1;
            }

            checks.push(PrCheckStatus {
                name,
                status,
                conclusion,
                details_url,
            });
        }
    }

    let total_checks = checks.len();

    let reviews = val["reviews"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|review| PrReview {
            remote_id: github_id(&review["id"]),
            author: github_login(&review["author"]),
            body: review["body"].as_str().unwrap_or("").to_owned(),
            state: review["state"].as_str().unwrap_or("").to_owned(),
            submitted_at: review["submittedAt"].as_str().unwrap_or("").to_owned(),
        })
        .collect();
    let commits = val["commits"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|commit| GitHubPrCommit {
            oid: commit["oid"].as_str().unwrap_or("").to_owned(),
            message: commit["messageHeadline"].as_str().unwrap_or("").to_owned(),
            author: github_login(&commit["author"]),
            committed_at: commit["committedDate"].as_str().unwrap_or("").to_owned(),
        })
        .collect();
    let files = val["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|file| GitHubPrFile {
            path: file["path"].as_str().unwrap_or("").to_owned(),
            additions: file["additions"].as_u64().unwrap_or(0),
            deletions: file["deletions"].as_u64().unwrap_or(0),
            change_type: file["changeType"].as_str().unwrap_or("").to_owned(),
        })
        .collect();

    Ok(GitHubPrInfo {
        number,
        title,
        url,
        state,
        is_draft,
        head_ref,
        base_ref,
        comments_count,
        review_comments: Vec::new(),
        commits,
        review_comments_complete: false,
        checks,
        total_checks,
        failing_checks,
        pending_checks,
        passing_checks,
        body,
        author,
        updated_at,
        review_decision,
        head_oid,
        issue_comments,
        reviews,
        files,
    })
}

fn github_login(value: &serde_json::Value) -> String {
    value["login"]
        .as_str()
        .or_else(|| value.as_str())
        .unwrap_or("unknown")
        .to_owned()
}

fn github_id(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|id| id.to_string()))
        .unwrap_or_default()
}

fn parse_pr_conversation_comments(
    value: &serde_json::Value,
) -> Vec<PrConversationComment> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|comment| PrConversationComment {
            remote_id: github_id(&comment["id"]),
            author: github_login(&comment["author"]),
            body: comment["body"].as_str().unwrap_or("").to_owned(),
            created_at: comment["createdAt"].as_str().unwrap_or("").to_owned(),
            url: comment["url"].as_str().unwrap_or("").to_owned(),
        })
        .collect()
}

pub(crate) fn enrich_pr_review_comments(info: &mut GitHubPrInfo, json: &str) -> Result<(), String> {
    info.review_comments_complete = false;
    let value: serde_json::Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let items = value
        .as_array()
        .ok_or_else(|| "GitHub review comments response must be an array".to_owned())?;
    let mut review_comments = Vec::new();
    for item in items {
        let page = item
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(std::slice::from_ref(item));
        for comment in page {
            let remote_id = github_id(&comment["id"]);
            if !comment.is_object() || remote_id.is_empty() {
                return Err("GitHub review comment is missing an ID".to_owned());
            }
            let body = comment["body"]
                .as_str()
                .ok_or_else(|| "GitHub review comment is missing a body".to_owned())?;
            let in_reply_to_id = match &comment["in_reply_to_id"] {
                serde_json::Value::Null => None,
                value => {
                    let id = github_id(value);
                    if id.is_empty() {
                        return Err("GitHub review comment has an invalid parent ID".to_owned());
                    }
                    Some(id)
                }
            };
            review_comments.push(PrReviewComment {
                remote_id,
                in_reply_to_id,
                author: github_login(&comment["user"]),
                body: body.to_owned(),
                path: comment["path"].as_str().map(str::to_owned),
                line: comment["line"]
                    .as_u64()
                    .or_else(|| comment["original_line"].as_u64()),
                created_at: comment["created_at"].as_str().unwrap_or("").to_owned(),
            });
        }
    }
    info.review_comments = review_comments;
    info.review_comments_complete = true;
    Ok(())
}

fn parse_github_repository(url: &str) -> Result<GitHubRepository, String> {
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| format!("invalid GitHub URL: {url}"))?;
    let mut parts = url.split('/');
    let host = parts.next().filter(|part| !part.is_empty());
    let owner = parts.next().filter(|part| !part.is_empty());
    let repo = parts.next().filter(|part| !part.is_empty());
    match (host, owner, repo) {
        (Some(host), Some(owner), Some(repo)) => Ok(GitHubRepository {
            host: host.to_owned(),
            owner: owner.to_owned(),
            repo: repo.to_owned(),
        }),
        _ => Err(format!("invalid GitHub URL: {url}")),
    }
}

pub(crate) fn parse_github_issue_json(json_str: &str) -> Result<GitHubIssueDetail, String> {
    let value: serde_json::Value =
        serde_json::from_str(json_str).map_err(|error| error.to_string())?;
    let number = value["number"]
        .as_u64()
        .ok_or_else(|| "GitHub issue is missing a number".to_owned())?;
    let url = value["url"]
        .as_str()
        .ok_or_else(|| "GitHub issue is missing a URL".to_owned())?
        .to_owned();
    let repository = parse_github_repository(&url)?;
    let comments = value["comments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|comment| GitHubIssueComment {
            remote_id: github_id(&comment["id"]),
            author: github_login(&comment["author"]),
            body: comment["body"].as_str().unwrap_or("").to_owned(),
            created_at: comment["createdAt"].as_str().unwrap_or("").to_owned(),
            url: comment["url"].as_str().unwrap_or("").to_owned(),
        })
        .collect::<Vec<_>>();
    let labels = value["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|label| GitHubLabel {
            name: label["name"].as_str().unwrap_or("").to_owned(),
            color: label["color"].as_str().unwrap_or("").to_owned(),
            description: label["description"].as_str().map(str::to_owned),
        })
        .collect();
    let assignees = value["assignees"]
        .as_array()
        .into_iter()
        .flatten()
        .map(github_login)
        .collect();
    Ok(GitHubIssueDetail {
        summary: GitHubIssueSummary {
            issue: GitHubIssueRef {
                host: repository.host,
                owner: repository.owner,
                repo: repository.repo,
                number,
                url,
            },
            title: value["title"].as_str().unwrap_or("").to_owned(),
            state: value["state"].as_str().unwrap_or("").to_owned(),
            author: github_login(&value["author"]),
            updated_at: value["updatedAt"].as_str().unwrap_or("").to_owned(),
            labels,
            assignees,
            comments_count: comments.len(),
        },
        body: value["body"].as_str().unwrap_or("").to_owned(),
        comments,
    })
}

fn inspect_pr_uncached(work_dir: &Path, branch: &str) -> Result<Option<GitHubPrInfo>, GitError> {
    let args = [
        "pr", "view", branch, "--json",
        "number,title,url,state,isDraft,body,comments,reviews,commits,files,statusCheckRollup,headRefName,headRefOid,baseRefName,updatedAt,author,reviewDecision",
    ].map(str::to_owned);
    let stdout = match execute_gh(work_dir, &args) {
        Ok(stdout) => stdout,
        Err(error)
            if error
                .message
                .to_ascii_lowercase()
                .contains("no pull request") =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };

    let mut info = parse_gh_pr_json(&stdout).map_err(|error| {
        GitError::new(
            work_dir,
            format!("could not parse gh pull request response: {error}"),
        )
    })?;

    // `gh pr view --json comments` exposes issue comments only. Inline
    // review comments live on the REST review-comments endpoint. Every
    // failure stage warns with context: a silent skip leaves
    // `review_comments_complete` false with no trace of why.
    if let Ok((repository, number)) = parse_pull_request_url(&info.url) {
        let api_path = format!(
            "repos/{}/{}/pulls/{number}/comments",
            repository.owner, repository.repo
        );
        let api_args = github_api_args(&repository.host, &[&api_path, "--paginate", "--slurp"]);
        match execute_gh(work_dir, &api_args) {
            Err(error) => tracing::warn!("review comments for PR #{number} unavailable: {error}"),
            Ok(pages) => {
                if let Err(error) = enrich_pr_review_comments(&mut info, &pages) {
                    tracing::warn!("review comments for PR #{number} failed to parse: {error}");
                }
            }
        }
    } else {
        tracing::warn!(
            "cannot fetch review comments: unparseable PR url '{}'",
            info.url
        );
    }

    if info.number == 0 {
        return Err(GitError::new(
            work_dir,
            "gh returned a pull request without a number".to_owned(),
        ));
    }
    Ok(Some(info))
}

pub(crate) fn inspect_pr(work_dir: &Path) -> Result<Option<GitHubPrInfo>, GitError> {
    let Some(branch) = current_branch(work_dir)? else {
        return Ok(None);
    };
    inspect_pr_for_branch(work_dir, &branch)
}

pub fn inspect_pr_for_branch(
    work_dir: &Path,
    branch: &str,
) -> Result<Option<GitHubPrInfo>, GitError> {
    PR_CACHE.get_or_init(ResponseCache::new).get_or_fetch(
        pr_cache_key(work_dir, branch),
        PR_INSPECTION_TTL,
        || inspect_pr_uncached(work_dir, branch),
    )
}

pub(crate) fn github_issue_list_args(
    state: &str,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<String>, String> {
    let state = validated_github_list_state(state)?;
    if limit == 0 {
        return Err("GitHub list limit must be greater than zero".into());
    }
    let mut args = vec![
        "issue".into(),
        "list".into(),
        "--state".into(),
        state.into(),
    ];
    if let Some(query) = query {
        args.extend(["--search".into(), validated_text(query, "GitHub query")?]);
    }
    args.extend([
        "--limit".into(),
        limit.to_string(),
        "--json".into(),
        "number,title,state,url,updatedAt,author,assignees,labels,comments".into(),
    ]);
    Ok(args)
}

pub(crate) fn github_issue_view_args(number: u64) -> Result<Vec<String>, String> {
    validate_github_number(number, "issue")?;
    Ok(vec![
        "issue".to_owned(),
        "view".to_owned(),
        number.to_string(),
        "--json".to_owned(),
        "number,title,state,url,body,updatedAt,author,assignees,labels,comments".to_owned(),
    ])
}

pub(crate) fn github_pr_list_args(
    state: &str,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<String>, String> {
    let state = validated_github_pr_list_state(state)?;
    if limit == 0 {
        return Err("GitHub list limit must be greater than zero".into());
    }
    let mut args = vec!["pr".into(), "list".into(), "--state".into(), state.into()];
    if let Some(query) = query {
        args.extend(["--search".into(), validated_text(query, "GitHub query")?]);
    }
    args.extend([
        "--limit".into(),
        limit.to_string(),
        "--json".into(),
        "number,title,state,url,isDraft,headRefName,baseRefName,updatedAt,author,reviewDecision,statusCheckRollup".into(),
    ]);
    Ok(args)
}

pub(crate) fn github_api_args(host: &str, args: &[&str]) -> Vec<String> {
    let mut result = vec!["api".into(), "--hostname".into(), host.into()];
    result.extend(args.iter().map(|arg| (*arg).into()));
    result
}

fn validated_github_list_state(state: &str) -> Result<&str, String> {
    match state {
        "open" | "closed" => Ok(state),
        _ => Err("GitHub list state must be open or closed".into()),
    }
}

fn validated_github_pr_list_state(state: &str) -> Result<&str, String> {
    match state {
        "open" | "closed" | "merged" => Ok(state),
        _ => Err("GitHub pull request list state must be open, closed, or merged".into()),
    }
}

pub(crate) fn create_draft_pr_args(
    base: &str,
    title: &str,
    body: &str,
) -> Result<Vec<String>, String> {
    Ok(vec![
        "pr".to_owned(),
        "create".to_owned(),
        "--draft".to_owned(),
        "--base".to_owned(),
        validated_text(base, "base branch")?,
        "--title".to_owned(),
        validated_text(title, "pull request title")?,
        "--body".to_owned(),
        validated_text(body, "pull request body")?,
    ])
}

pub(crate) fn github_issue_create_args(title: &str, body: &str) -> Result<Vec<String>, String> {
    Ok(vec![
        "issue".to_owned(),
        "create".to_owned(),
        "--title".to_owned(),
        validated_text(title, "issue title")?,
        "--body".to_owned(),
        body.to_owned(),
    ])
}

pub(crate) fn parse_gh_issue_create_output(output: &str) -> Result<u64, String> {
    let output = output.trim();
    let number = output
        .rsplit_once("/issues/")
        .and_then(|(_, number)| number.parse::<u64>().ok())
        .filter(|number| *number > 0)
        .ok_or_else(|| "gh returned an invalid created issue URL".to_owned())?;
    Ok(number)
}

pub(crate) fn github_pr_comment_args(number: u64, body: &str) -> Result<Vec<String>, String> {
    validate_github_number(number, "pull request")?;
    Ok(vec![
        "pr".to_owned(),
        "comment".to_owned(),
        number.to_string(),
        "--body".to_owned(),
        validated_text(body, "comment body")?,
    ])
}

pub(crate) fn github_pr_review_args(
    number: u64,
    verdict: PullRequestReviewVerdict,
    body: &str,
) -> Result<Vec<String>, String> {
    validate_github_number(number, "pull request")?;
    Ok(vec![
        "pr".to_owned(),
        "review".to_owned(),
        number.to_string(),
        review_verdict_flag(verdict).to_owned(),
        "--body".to_owned(),
        validated_text(body, "review body")?,
    ])
}

fn validate_github_number(number: u64, resource: &str) -> Result<(), String> {
    (number != 0)
        .then_some(())
        .ok_or_else(|| format!("{resource} number must be greater than zero"))
}

fn validated_text(value: &str, name: &str) -> Result<String, String> {
    let value = value.trim();
    (!value.is_empty())
        .then(|| value.to_owned())
        .ok_or_else(|| format!("{name} cannot be empty"))
}

fn review_verdict_flag(verdict: PullRequestReviewVerdict) -> &'static str {
    match verdict {
        PullRequestReviewVerdict::Comment => "--comment",
        PullRequestReviewVerdict::Approve => "--approve",
        PullRequestReviewVerdict::RequestChanges => "--request-changes",
    }
}

fn validate_review_path(path: &str) -> Result<&str, String> {
    let path = path.trim();
    let candidate = Path::new(path);
    (!path.is_empty()
        && !candidate.is_absolute()
        && !candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir)))
    .then_some(path)
    .ok_or_else(|| format!("review path must be repository-relative: {path}"))
}

pub(crate) fn review_comment_payloads(
    commit_id: &str,
    comments: &[PullRequestReviewCommentDraft],
) -> Result<Vec<serde_json::Value>, String> {
    let commit_id = validated_text(commit_id, "head commit")?;
    comments
        .iter()
        .map(|comment| {
            Ok(serde_json::json!({
                "commit_id": commit_id,
                "path": validate_review_path(&comment.path)?,
                "body": validated_text(&comment.body, "review comment body")?,
                "subject_type": "file",
            }))
        })
        .collect()
}

fn execute_gh(work_dir: &Path, args: &[String]) -> Result<String, GitError> {
    execute_gh_command(work_dir, args, None)
}

// ponytail: one process-wide gate is conservative across accounts/hosts; split
// by authenticated host if independent GitHub Enterprise traffic needs concurrency.
#[derive(Default)]
struct GhRequestState {
    busy: bool,
    limited_until: Option<Instant>,
    consecutive_limits: u32,
}

#[derive(Default)]
struct GhRequestGate {
    state: Mutex<GhRequestState>,
    changed: Condvar,
}

struct GhRequestPermit<'a>(&'a GhRequestGate);

impl GhRequestGate {
    fn acquire(&self, work_dir: &Path) -> Result<GhRequestPermit<'_>, GitError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self
            .changed
            .wait_while(state, |state| state.busy)
            .unwrap_or_else(|e| e.into_inner());
        if let Some(remaining) = state
            .limited_until
            .and_then(|until| until.checked_duration_since(Instant::now()))
        {
            return Err(GitError::new(
                work_dir,
                format!(
                    "GitHub API rate limit reached; retry in {} seconds",
                    remaining.as_secs() + 1
                ),
            ));
        }
        state.busy = true;
        Ok(GhRequestPermit(self))
    }
}

impl GhRequestPermit<'_> {
    fn record(&self, success: bool, stderr: &str) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if rate_limit_message(stderr).is_some() {
            state.consecutive_limits = state.consecutive_limits.saturating_add(1);
            let delay = rate_limit_delay(state.consecutive_limits);
            state.limited_until = Some(Instant::now() + delay);
        } else if success {
            state.consecutive_limits = 0;
            state.limited_until = None;
        }
    }
}

impl Drop for GhRequestPermit<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner()).busy = false;
        self.0.changed.notify_all();
    }
}

fn rate_limit_delay(consecutive_limits: u32) -> Duration {
    Duration::from_secs((60u64 << consecutive_limits.saturating_sub(1).min(6)).min(3600))
}

#[cfg(test)]
mod request_tests {
    use super::{rate_limit_delay, run_gh_command, GhRequestGate};
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    #[cfg(unix)]
    fn rate_limit_blocks_subsequent_reads_and_writes_without_retrying() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("calls");
        let gate = GhRequestGate::default();
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "echo call >> \"$1\"; echo 'HTTP 429' >&2; exit 1",
                "test",
            ])
            .arg(&marker);
        assert!(run_gh_command(dir.path(), &mut command, None, None, &gate).is_err());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "call\n");
        for payload in [None, Some(serde_json::json!({"body": "test"}))] {
            assert!(
                run_gh_command(dir.path(), &mut command, None, payload.as_ref(), &gate)
                    .unwrap_err()
                    .message
                    .contains("retry in")
            );
        }
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "call\n");
        gate.state.lock().unwrap().limited_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(run_gh_command(dir.path(), &mut command, None, None, &gate).is_err());
        assert_eq!(gate.state.lock().unwrap().consecutive_limits, 2);
        assert_eq!(rate_limit_delay(1).as_secs(), 60);
        assert_eq!(rate_limit_delay(2).as_secs(), 120);
        assert_eq!(rate_limit_delay(u32::MAX).as_secs(), 3600);
    }

    #[test]
    fn github_gate_serializes_requests_and_releases_on_drop() {
        let gate = GhRequestGate::default();
        let concurrent = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..12 {
                scope.spawn(|| {
                    let _permit = gate.acquire(std::path::Path::new("repo")).unwrap();
                    assert_eq!(
                        concurrent.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                        0
                    );
                    std::thread::yield_now();
                    assert_eq!(
                        concurrent.fetch_sub(1, std::sync::atomic::Ordering::SeqCst),
                        1
                    );
                });
            }
        });
    }
}

fn execute_gh_command(
    work_dir: &Path,
    args: &[String],
    payload: Option<&serde_json::Value>,
) -> Result<String, GitError> {
    static GATE: OnceLock<GhRequestGate> = OnceLock::new();
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let (mut command, stored_token) = gh_command_with_captured_token(work_dir, &refs);
    run_gh_command(
        work_dir,
        &mut command,
        stored_token.as_deref(),
        payload,
        GATE.get_or_init(GhRequestGate::default),
    )
}

fn run_gh_command(
    work_dir: &Path,
    command: &mut Command,
    stored_token: Option<&str>,
    payload: Option<&serde_json::Value>,
    gate: &GhRequestGate,
) -> Result<String, GitError> {
    let permit = gate.acquire(work_dir)?;
    let output = if let Some(payload) = payload {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| GitError::new(work_dir, format!("could not start gh: {error}")))?;
        let write_result = child
            .stdin
            .take()
            .ok_or_else(|| GitError::new(work_dir, "could not open gh input"))?
            .write_all(payload.to_string().as_bytes());
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitError::new(
                work_dir,
                format!("could not write gh input: {error}"),
            ));
        }
        child.wait_with_output()
    } else {
        command.output()
    }
    .map_err(|error| GitError::new(work_dir, format!("could not run gh: {error}")))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    permit.record(output.status.success(), &stderr);
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(gh_failure(work_dir, output.status, &stderr, stored_token))
    }
}

/// Matches `gh` rate-limit failures (primary 429, secondary limits, 403
/// quota responses). Case-insensitive; anchored on "rate limit" phrasing so
/// unrelated 4xx text never matches.
pub(crate) fn rate_limit_message(stderr: &str) -> Option<String> {
    let folded = stderr.to_ascii_lowercase();
    let limited = folded.contains("api rate limit exceeded")
        || folded.contains("secondary rate limit")
        || folded.contains("exceeded a rate limit")
        || folded.contains("rate limit exceeded")
        || folded.contains("http 429")
        || folded.contains("(429)")
        || folded.contains("abuse detection");
    if !limited {
        return None;
    }
    // Surface gh's own reset hint when it carries one.
    let reset = stderr
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("reset"))
        .map(str::trim)
        .unwrap_or("GitHub API rate limit reached");
    Some(format!(
        "{reset}. Requests are paused; wait before retrying."
    ))
}

fn execute_gh_json(
    work_dir: &Path,
    args: &[String],
    payload: &serde_json::Value,
) -> Result<String, GitError> {
    execute_gh_command(work_dir, args, Some(payload))
}

pub fn list_github_issues(
    work_dir: &Path,
    state: &str,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<GitHubIssueSummary>, GitError> {
    let args = github_issue_list_args(state, query, limit)
        .map_err(|message| GitError::new(work_dir, message))?;
    let key = (repository_key(work_dir), args.join("\0"));
    ISSUE_LIST_CACHE
        .get_or_init(ResponseCache::new)
        .get_or_fetch(key, GITHUB_RESPONSE_TTL, || {
            let output = execute_gh(work_dir, &args)?;
            let values: Vec<serde_json::Value> =
                serde_json::from_str(&output).map_err(|error| {
                    GitError::new(
                        work_dir,
                        format!("could not parse GitHub issue list: {error}"),
                    )
                })?;
            let rows = values
                .into_iter()
                .filter_map(|value| match parse_github_issue_json(&value.to_string()) {
                    Ok(detail) => Some(detail.summary),
                    Err(error) => {
                        tracing::warn!("skipping unparseable issue entry: {error}");
                        None
                    }
                })
                .collect::<Vec<_>>();
            Ok(rows)
        })
}
pub fn inspect_github_issue(work_dir: &Path, number: u64) -> Result<GitHubIssueDetail, GitError> {
    let key = (repository_key(work_dir), number);
    ISSUE_DETAIL_CACHE
        .get_or_init(ResponseCache::new)
        .get_or_fetch(key, GITHUB_RESPONSE_TTL, || {
            let args = github_issue_view_args(number)
                .map_err(|message| GitError::new(work_dir, message))?;
            let output = execute_gh(work_dir, &args)?;
            let detail = parse_github_issue_json(&output).map_err(|message| {
                GitError::new(work_dir, format!("could not parse GitHub issue: {message}"))
            })?;
            Ok(detail)
        })
}
pub fn list_github_pull_requests(
    work_dir: &Path,
    state: &str,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<GitHubPullRequestSummary>, GitError> {
    let args = github_pr_list_args(state, query, limit)
        .map_err(|message| GitError::new(work_dir, message))?;
    let key = (repository_key(work_dir), args.join("\0"));
    PR_LIST_CACHE
        .get_or_init(ResponseCache::new)
        .get_or_fetch(key, GITHUB_RESPONSE_TTL, || {
            let output = execute_gh(work_dir, &args)?;
            let values: Vec<serde_json::Value> =
                serde_json::from_str(&output).map_err(|error| {
                    GitError::new(
                        work_dir,
                        format!("could not parse GitHub pull request list: {error}"),
                    )
                })?;
            let rows = summarize_pr_list(values);
            Ok(rows)
        })
}

/// Maps raw `gh pr list` entries onto summaries, skipping malformed ones.
///
/// One bad entry (empty url, number 0, deleted repo, unparseable shape) must
/// never poison the whole list: it is warned about and dropped, the rest
/// survive. Pure for testing; the `gh` call stays in the caller.
pub(crate) fn summarize_pr_list(values: Vec<serde_json::Value>) -> Vec<GitHubPullRequestSummary> {
    values
        .into_iter()
        .filter_map(|value| {
            let pr = match parse_gh_pr_json(&value.to_string()) {
                Ok(pr) => pr,
                Err(error) => {
                    tracing::warn!("skipping unparseable pull request entry: {error}");
                    return None;
                }
            };
            // Defaults (number 0, empty url) mark a malformed entry: skip it
            // rather than poisoning the whole list.
            if pr.number == 0 || pr.url.is_empty() {
                tracing::warn!("skipping pull request entry with no number/url");
                return None;
            }
            let repository = match parse_github_repository(&pr.url) {
                Ok(repository) => repository,
                Err(error) => {
                    tracing::warn!("skipping pull request with bad url '{}': {error}", pr.url);
                    return None;
                }
            };
            Some(GitHubPullRequestSummary {
                repository,
                number: pr.number,
                title: pr.title,
                state: pr.state,
                url: pr.url,
                is_draft: pr.is_draft,
                head_ref: pr.head_ref,
                base_ref: pr.base_ref,
                author: pr.author,
                updated_at: pr.updated_at,
                review_decision: pr.review_decision,
                checks: pr.checks,
            })
        })
        .collect::<Vec<_>>()
}

pub fn inspect_pr_number(work_dir: &Path, number: u64) -> Result<GitHubPrInfo, GitError> {
    validate_github_number(number, "pull request")
        .map_err(|message| GitError::new(work_dir, message))?;
    inspect_pr_for_branch(work_dir, &number.to_string())?.ok_or_else(|| {
        GitError::new(
            work_dir,
            format!("GitHub pull request #{number} was not found"),
        )
    })
}

pub fn pull_request_diff(work_dir: &Path, number: u64) -> Result<String, GitError> {
    validate_github_number(number, "pull request")
        .map_err(|message| GitError::new(work_dir, message))?;
    execute_gh(work_dir, &["pr".into(), "diff".into(), number.to_string()])
}

pub fn create_pull_request(work_dir: &Path) -> Result<String, GitError> {
    let branch = current_branch(work_dir)?.ok_or_else(|| {
        GitError::new(
            work_dir,
            "cannot create a pull request from a detached HEAD; check out a named branch first"
                .to_owned(),
        )
    })?;

    push(work_dir)?;
    invalidate_pr_cache(work_dir, &branch);

    let result = execute_gh(work_dir, &["pr".into(), "create".into(), "--fill".into()]);
    invalidate_github_cache(work_dir);
    result
}

/// Creates a draft pull request for an already-published branch without pushing it.
pub fn create_draft_pull_request(
    work_dir: &Path,
    base: &str,
    title: &str,
    body: &str,
) -> Result<String, GitError> {
    current_branch(work_dir)?.ok_or_else(|| {
        GitError::new(
            work_dir,
            "cannot create a pull request from a detached HEAD; check out a named branch first"
                .to_owned(),
        )
    })?;
    let has_upstream = command(
        work_dir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .ok()
    .is_some_and(|upstream| !upstream.trim().is_empty());
    if !has_upstream {
        return Err(GitError::new(
            work_dir,
            "cannot create a draft pull request before publishing the current branch".to_owned(),
        ));
    }
    let args = create_draft_pr_args(base, title, body)
        .map_err(|message| GitError::new(work_dir, message))?;
    let result = execute_gh(work_dir, &args);
    invalidate_github_cache(work_dir);
    result
}

pub fn comment_on_github_issue(
    work_dir: &Path,
    number: u64,
    body: &str,
) -> Result<String, GitError> {
    validate_github_number(number, "issue").map_err(|message| GitError::new(work_dir, message))?;
    let body =
        validated_text(body, "comment body").map_err(|message| GitError::new(work_dir, message))?;
    invalidate_github_cache(work_dir);
    execute_gh(
        work_dir,
        &[
            "issue".into(),
            "comment".into(),
            number.to_string(),
            "--body".into(),
            body,
        ],
    )
}

/// Creates a GitHub issue, returning its number.
pub fn create_github_issue(
    work_dir: &Path,
    title: &str,
    body: &str,
) -> Result<u64, GitError> {
    let args = github_issue_create_args(title, body)
        .map_err(|message| GitError::new(work_dir, message))?;
    invalidate_github_cache(work_dir);
    let output = execute_gh(work_dir, &args)?;
    parse_gh_issue_create_output(&output)
        .map_err(|message| GitError::new(work_dir, message))
}

/// Closes (`close=true`) or reopens an issue.
pub fn set_github_issue_state(
    work_dir: &Path,
    number: u64,
    close: bool,
) -> Result<(), GitError> {
    validate_github_number(number, "issue").map_err(|message| GitError::new(work_dir, message))?;
    invalidate_github_cache(work_dir);
    execute_gh(
        work_dir,
        &[
            "issue".into(),
            (if close { "close" } else { "reopen" }).into(),
            number.to_string(),
        ],
    )?;
    Ok(())
}

/// Permanently deletes an issue. There is no undo; callers confirm first.
pub fn delete_github_issue(work_dir: &Path, number: u64) -> Result<(), GitError> {
    validate_github_number(number, "issue").map_err(|message| GitError::new(work_dir, message))?;
    invalidate_github_cache(work_dir);
    execute_gh(
        work_dir,
        &[
            "issue".into(),
            "delete".into(),
            number.to_string(),
            "--yes".into(),
        ],
    )?;
    Ok(())
}

/// Repository labels available for issues.
pub fn list_github_labels(work_dir: &Path) -> Result<Vec<GitHubLabel>, GitError> {
    let output = execute_gh(
        work_dir,
        &[
            "label".into(),
            "list".into(),
            "--json".into(),
            "name,color,description".into(),
        ],
    )?;
    serde_json::from_str::<Vec<serde_json::Value>>(&output)
        .map(|values| {
            values
                .iter()
                .map(|label| GitHubLabel {
                    name: label["name"].as_str().unwrap_or("").to_owned(),
                    color: label["color"].as_str().unwrap_or("").to_owned(),
                    description: label["description"].as_str().map(str::to_owned),
                })
                .filter(|label| !label.name.is_empty())
                .collect()
        })
        .map_err(|error| GitError::new(work_dir, format!("could not parse label list: {error}")))
}

/// Adds and/or removes issue labels.
pub fn edit_github_issue_labels(
    work_dir: &Path,
    number: u64,
    add: &[String],
    remove: &[String],
) -> Result<(), GitError> {
    validate_github_number(number, "issue").map_err(|message| GitError::new(work_dir, message))?;
    if add.is_empty() && remove.is_empty() {
        return Ok(());
    }
    invalidate_github_cache(work_dir);
    let mut args = vec![
        "issue".into(),
        "edit".into(),
        number.to_string(),
    ];
    if !add.is_empty() {
        args.push("--add-label".into());
        args.push(add.join(","));
    }
    if !remove.is_empty() {
        args.push("--remove-label".into());
        args.push(remove.join(","));
    }
    execute_gh(work_dir, &args)?;
    Ok(())
}

pub fn comment_on_pull_request(
    work_dir: &Path,
    number: u64,
    body: &str,
) -> Result<String, GitError> {
    let args =
        github_pr_comment_args(number, body).map_err(|message| GitError::new(work_dir, message))?;
    invalidate_github_cache(work_dir);
    execute_gh(work_dir, &args)
}

fn parse_pull_request_url(url: &str) -> Result<(GitHubRepository, u64), String> {
    let repository = parse_github_repository(url)?;
    let number = url
        .trim_end_matches('/')
        .rsplit_once("/pull/")
        .and_then(|(_, number)| number.parse().ok())
        .ok_or_else(|| format!("invalid GitHub pull request URL: {url}"))?;
    validate_github_number(number, "pull request")?;
    Ok((repository, number))
}

pub(crate) fn validated_review_endpoint(
    pull_request: &GitHubPrInfo,
) -> Result<(GitHubRepository, String), String> {
    let (repository, number) = parse_pull_request_url(&pull_request.url)?;
    if number != pull_request.number {
        return Err(format!(
            "pull request URL number #{number} does not match DTO number #{}",
            pull_request.number
        ));
    }
    let endpoint = format!(
        "repos/{}/{}/pulls/{number}/reviews",
        repository.owner, repository.repo
    );
    Ok((repository, endpoint))
}

pub fn reply_to_pull_request_review_comment(
    work_dir: &Path,
    pull_request_url: &str,
    comment_id: u64,
    body: &str,
) -> Result<String, GitError> {
    validate_github_number(comment_id, "review comment")
        .map_err(|message| GitError::new(work_dir, message))?;
    let body =
        validated_text(body, "reply body").map_err(|message| GitError::new(work_dir, message))?;
    let (repository, number) = parse_pull_request_url(pull_request_url)
        .map_err(|message| GitError::new(work_dir, message))?;
    let endpoint = format!(
        "repos/{}/{}/pulls/{number}/comments/{comment_id}/replies",
        repository.owner, repository.repo
    );
    let body = format!("body={body}");
    invalidate_github_cache(work_dir);
    execute_gh(
        work_dir,
        &github_api_args(
            &repository.host,
            &["--method", "POST", &endpoint, "-f", &body],
        ),
    )
}

pub fn submit_pull_request_review(
    work_dir: &Path,
    pull_request: &GitHubPrInfo,
    verdict: PullRequestReviewVerdict,
    body: &str,
    comments: &[PullRequestReviewCommentDraft],
) -> Result<String, GitError> {
    validate_github_number(pull_request.number, "pull request")
        .map_err(|message| GitError::new(work_dir, message))?;
    let args = github_pr_review_args(pull_request.number, verdict, body)
        .map_err(|message| GitError::new(work_dir, message))?;
    let comment_payloads = if comments.is_empty() {
        Vec::new()
    } else {
        review_comment_payloads(&pull_request.head_oid, comments)
            .map_err(|message| GitError::new(work_dir, message))?
    };
    let (repository, _review_endpoint) = validated_review_endpoint(pull_request)
        .map_err(|message| GitError::new(work_dir, message))?;
    invalidate_github_cache(work_dir);
    let review = execute_gh(work_dir, &args)?;
    let endpoint = format!(
        "repos/{}/{}/pulls/{}/comments",
        repository.owner, repository.repo, pull_request.number
    );
    for payload in comment_payloads {
        let args = github_api_args(
            &repository.host,
            &["--method", "POST", &endpoint, "--input", "-"],
        );
        execute_gh_json(work_dir, &args, &payload)?;
    }
    Ok(review)
}
