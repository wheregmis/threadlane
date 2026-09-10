use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::GitError;
use crate::git::{command, current_branch, push};
use crate::types::{
    GitHubIssueComment, GitHubIssueDetail, GitHubIssueRef, GitHubIssueSummary, GitHubLabel,
    GitHubPrCommit, GitHubPrFile, GitHubPrInfo, GitHubPullRequestSummary, GitHubRepository,
    PrCheckStatus, PrConversationComment, PrReview, PrReviewComment, PullRequestReviewCommentDraft,
    PullRequestReviewVerdict,
};

const PR_INSPECTION_TTL: Duration = Duration::from_secs(30);

type PrCacheKey = (PathBuf, String);

static PR_CACHE: OnceLock<Mutex<HashMap<PrCacheKey, (Instant, Option<GitHubPrInfo>)>>> =
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
        if let Ok(mut cache) = cache.lock() {
            cache.remove(&pr_cache_key(work_dir, branch));
        }
    }
}

pub(crate) fn gh_command(work_dir: &Path, args: &[&str]) -> Command {
    gh_command_with_captured_token(work_dir, args).0
}

pub(crate) fn gh_command_with_captured_token(
    work_dir: &Path,
    args: &[&str],
) -> (Command, Option<String>) {
    let stored_token = stored_github_token();
    gh_command_with_token_capture(work_dir, args, stored_token)
}

pub(crate) fn stored_github_token() -> Option<String> {
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

pub(crate) fn gh_failure(
    work_dir: &Path,
    status: std::process::ExitStatus,
    stderr: &str,
    stored_token: Option<&str>,
) -> GitError {
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

pub(crate) fn github_login(value: &serde_json::Value) -> String {
    value["login"]
        .as_str()
        .or_else(|| value.as_str())
        .unwrap_or("unknown")
        .to_owned()
}

pub(crate) fn github_id(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|id| id.to_string()))
        .unwrap_or_default()
}

pub(crate) fn parse_pr_conversation_comments(
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

pub(crate) fn parse_github_repository(url: &str) -> Result<GitHubRepository, String> {
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

pub(crate) fn inspect_pr_uncached(
    work_dir: &Path,
    branch: &str,
) -> Result<Option<GitHubPrInfo>, GitError> {
    let (mut command, stored_token) = gh_command_with_captured_token(
        work_dir,
        &[
            "pr",
            "view",
            branch,
            "--json",
            "number,title,url,state,isDraft,body,comments,reviews,commits,files,statusCheckRollup,headRefName,headRefOid,baseRefName,updatedAt,author,reviewDecision",
        ],
    );
    let output = command
        .output()
        .map_err(|error| GitError::new(work_dir, format!("could not start gh: {error}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if stderr.to_ascii_lowercase().contains("no pull request") {
            return Ok(None);
        }
        return Err(gh_failure(
            work_dir,
            output.status,
            &stderr,
            stored_token.as_deref(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut info = parse_gh_pr_json(&stdout).map_err(|error| {
        GitError::new(
            work_dir,
            format!("could not parse gh pull request response: {error}"),
        )
    })?;

    // `gh pr view --json comments` exposes issue comments only. Inline
    // review comments live on the REST review-comments endpoint.
    if let Ok((repository, number)) = parse_pull_request_url(&info.url) {
        let api_path = format!(
            "repos/{}/{}/pulls/{number}/comments",
            repository.owner, repository.repo
        );
        let api_args = github_api_args(&repository.host, &[&api_path, "--paginate", "--slurp"]);
        let api_args = api_args.iter().map(String::as_str).collect::<Vec<_>>();
        if let Ok(review_output) = gh_command(work_dir, &api_args).output() {
            if review_output.status.success() {
                let pages = String::from_utf8_lossy(&review_output.stdout);
                let _ = enrich_pr_review_comments(&mut info, &pages);
            }
        }
    }

    if info.number == 0 {
        return Err(GitError::new(
            work_dir,
            "gh returned a pull request without a number".to_owned(),
        ));
    }
    Ok(Some(info))
}

pub fn inspect_pr(work_dir: &Path) -> Result<Option<GitHubPrInfo>, GitError> {
    let Some(branch) = current_branch(work_dir)? else {
        return Ok(None);
    };
    inspect_pr_for_branch(work_dir, &branch)
}

pub fn inspect_pr_for_branch(
    work_dir: &Path,
    branch: &str,
) -> Result<Option<GitHubPrInfo>, GitError> {
    let key = pr_cache_key(work_dir, branch);
    let now = Instant::now();
    let cache = PR_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(info) = cache.lock().ok().and_then(|cache| {
        cache
            .get(&key)
            .and_then(|entry| fresh_cache_value(entry, now, PR_INSPECTION_TTL))
    }) {
        return Ok(info);
    }
    let info = inspect_pr_uncached(work_dir, branch)?;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, (now, info.clone()));
    }
    Ok(info)
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

pub(crate) fn validated_github_list_state(state: &str) -> Result<&str, String> {
    match state {
        "open" | "closed" => Ok(state),
        _ => Err("GitHub list state must be open or closed".into()),
    }
}

pub(crate) fn validated_github_pr_list_state(state: &str) -> Result<&str, String> {
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

pub(crate) fn validate_github_number(number: u64, resource: &str) -> Result<(), String> {
    (number != 0)
        .then_some(())
        .ok_or_else(|| format!("{resource} number must be greater than zero"))
}

pub(crate) fn validated_text(value: &str, name: &str) -> Result<String, String> {
    let value = value.trim();
    (!value.is_empty())
        .then(|| value.to_owned())
        .ok_or_else(|| format!("{name} cannot be empty"))
}

pub(crate) fn review_verdict_flag(verdict: PullRequestReviewVerdict) -> &'static str {
    match verdict {
        PullRequestReviewVerdict::Comment => "--comment",
        PullRequestReviewVerdict::Approve => "--approve",
        PullRequestReviewVerdict::RequestChanges => "--request-changes",
    }
}

pub(crate) fn validate_review_path(path: &str) -> Result<&str, String> {
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

pub(crate) fn execute_gh(work_dir: &Path, args: &[String]) -> Result<String, GitError> {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let (mut command, stored_token) = gh_command_with_captured_token(work_dir, &refs);
    let output = command
        .output()
        .map_err(|error| GitError::new(work_dir, format!("could not start gh: {error}")))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(gh_failure(
        work_dir,
        output.status,
        &stderr,
        stored_token.as_deref(),
    ))
}

pub(crate) fn execute_gh_json(
    work_dir: &Path,
    args: &[String],
    payload: &serde_json::Value,
) -> Result<String, GitError> {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let (mut command, stored_token) = gh_command_with_captured_token(work_dir, &refs);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| GitError::new(work_dir, format!("could not start gh: {error}")))?;
    child
        .stdin
        .take()
        .ok_or_else(|| GitError::new(work_dir, "could not open gh input".to_owned()))?
        .write_all(payload.to_string().as_bytes())
        .map_err(|error| GitError::new(work_dir, format!("could not write gh input: {error}")))?;
    let output = child
        .wait_with_output()
        .map_err(|error| GitError::new(work_dir, format!("could not wait for gh: {error}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(gh_failure(
            work_dir,
            output.status,
            &stderr,
            stored_token.as_deref(),
        ))
    }
}

pub fn list_github_issues(
    work_dir: &Path,
    state: &str,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<GitHubIssueSummary>, GitError> {
    let args = github_issue_list_args(state, query, limit)
        .map_err(|message| GitError::new(work_dir, message))?;
    let output = execute_gh(work_dir, &args)?;
    let values: Vec<serde_json::Value> = serde_json::from_str(&output).map_err(|error| {
        GitError::new(
            work_dir,
            format!("could not parse GitHub issue list: {error}"),
        )
    })?;
    values
        .into_iter()
        .map(|value| parse_github_issue_json(&value.to_string()).map(|detail| detail.summary))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| GitError::new(work_dir, format!("could not parse GitHub issue: {error}")))
}

pub fn inspect_github_issue(work_dir: &Path, number: u64) -> Result<GitHubIssueDetail, GitError> {
    let args =
        github_issue_view_args(number).map_err(|message| GitError::new(work_dir, message))?;
    let output = execute_gh(work_dir, &args)?;
    parse_github_issue_json(&output).map_err(|message| {
        GitError::new(work_dir, format!("could not parse GitHub issue: {message}"))
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
    let output = execute_gh(work_dir, &args)?;
    let values: Vec<serde_json::Value> = serde_json::from_str(&output).map_err(|error| {
        GitError::new(
            work_dir,
            format!("could not parse GitHub pull request list: {error}"),
        )
    })?;
    values
        .into_iter()
        .map(|value| {
            let pr = parse_gh_pr_json(&value.to_string())?;
            let repository = parse_github_repository(&pr.url)?;
            Ok(GitHubPullRequestSummary {
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
        .collect::<Result<Vec<_>, String>>()
        .map_err(|message| {
            GitError::new(
                work_dir,
                format!("could not parse GitHub pull request: {message}"),
            )
        })
}

pub fn inspect_pr_number(work_dir: &Path, number: u64) -> Result<GitHubPrInfo, GitError> {
    validate_github_number(number, "pull request")
        .map_err(|message| GitError::new(work_dir, message))?;
    inspect_pr_uncached(work_dir, &number.to_string())?.ok_or_else(|| {
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

    let (mut command, stored_token) =
        gh_command_with_captured_token(work_dir, &["pr", "create", "--fill"]);
    let output = command
        .output()
        .map_err(|error| GitError::new(work_dir, format!("could not start gh: {error}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(gh_failure(
            work_dir,
            output.status,
            &stderr,
            stored_token.as_deref(),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Creates a draft pull request for an already-published branch without pushing it.
pub fn create_draft_pull_request(
    work_dir: &Path,
    base: &str,
    title: &str,
    body: &str,
) -> Result<String, GitError> {
    let branch = current_branch(work_dir)?.ok_or_else(|| {
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
    invalidate_pr_cache(work_dir, &branch);
    execute_gh(work_dir, &args)
}

pub fn comment_on_github_issue(
    work_dir: &Path,
    number: u64,
    body: &str,
) -> Result<String, GitError> {
    validate_github_number(number, "issue").map_err(|message| GitError::new(work_dir, message))?;
    let body =
        validated_text(body, "comment body").map_err(|message| GitError::new(work_dir, message))?;
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

pub fn comment_on_pull_request(
    work_dir: &Path,
    number: u64,
    body: &str,
) -> Result<String, GitError> {
    let args =
        github_pr_comment_args(number, body).map_err(|message| GitError::new(work_dir, message))?;
    execute_gh(work_dir, &args)
}

pub(crate) fn parse_pull_request_url(url: &str) -> Result<(GitHubRepository, u64), String> {
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
