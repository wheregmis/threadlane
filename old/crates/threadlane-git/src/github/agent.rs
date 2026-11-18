//! Credential-aware, uncached operations for the agent's PR follow-through loop.
use super::{
    current_branch, execute_gh, execute_gh_json, github_api_args, github_graphql_args,
    github_pr_comment_args, graphql_errors, invalidate_github_cache, parse_pull_request_url,
    reply_to_pull_request_review_comment, validate_github_number, validated_text,
    GitHubRepository,
};
use crate::GitError;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

fn validate_ready_checkout(
    work_dir: &Path,
    expected_branch: Option<&str>,
    expected_head_oid: Option<&str>,
) -> Result<(), GitError> {
    let branch = current_branch(work_dir)?;
    if branch.as_deref() != expected_branch || branch.is_none() {
        return Err(GitError::new(
            work_dir,
            "Check out the PR head branch before marking it ready",
        ));
    }

    let expected_head_oid =
        expected_head_oid.ok_or_else(|| GitError::new(work_dir, "Missing PR head SHA"))?;
    let local_head_oid = crate::git::command(work_dir, &["rev-parse", "--verify", "HEAD"])?;
    if local_head_oid.trim() != expected_head_oid {
        return Err(GitError::new(
            work_dir,
            "Check out the PR head commit before marking it ready",
        ));
    }

    Ok(())
}

fn safe_repository_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.starts_with('-')
        && !component.chars().any(|character| {
            character.is_whitespace()
                || matches!(character, '@' | '?' | '#' | '\\' | '%' | ':')
        })
}

fn parse_pr_repository(value: &str) -> Result<GitHubRepository, String> {
    let components = value.split('/').collect::<Vec<_>>();
    let (host, owner, repo) = match components.as_slice() {
        [owner, repo] => ("github.com", *owner, *repo),
        [host, owner, repo] => (*host, *owner, *repo),
        _ => return Err("repository must be OWNER/REPO or HOST/OWNER/REPO".into()),
    };
    if !safe_repository_component(host)
        || !safe_repository_component(owner)
        || !safe_repository_component(repo)
    {
        return Err("repository must be OWNER/REPO or HOST/OWNER/REPO".into());
    }
    Ok(GitHubRepository {
        host: host.to_owned(),
        owner: owner.to_owned(),
        repo: repo.to_owned(),
    })
}

fn repository_selector(repository: &GitHubRepository) -> String {
    format!(
        "{}/{}/{}",
        repository.host, repository.owner, repository.repo
    )
}

fn same_repository(left: &GitHubRepository, right: &GitHubRepository) -> bool {
    left.host.eq_ignore_ascii_case(&right.host)
        && left.owner.eq_ignore_ascii_case(&right.owner)
        && left.repo.eq_ignore_ascii_case(&right.repo)
}

fn parse_snapshot_pr_url(url: &str) -> Result<(GitHubRepository, u64), String> {
    let (repository, number) = parse_pull_request_url(url)?;
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| format!("invalid GitHub pull request URL: {url}"))?;
    let (host, path) = url
        .split_once('/')
        .ok_or_else(|| format!("invalid GitHub pull request URL: {url}"))?;
    let components = path.split('/').collect::<Vec<_>>();
    if !safe_repository_component(host)
        || components.len() != 4
        || !safe_repository_component(components[0])
        || !safe_repository_component(components[1])
        || components[2] != "pull"
        || components[3].parse::<u64>().ok() != Some(number)
        || host != repository.host
        || components[0] != repository.owner
        || components[1] != repository.repo
    {
        return Err("invalid GitHub pull request URL".into());
    }
    Ok((repository, number))
}

fn validate_snapshot_identity(
    work_dir: &Path,
    snapshot: &Value,
    repository: &GitHubRepository,
    number: u64,
) -> Result<(), GitError> {
    if snapshot["number"].as_u64() != Some(number) {
        return Err(GitError::new(
            work_dir,
            "GitHub returned a PR with a different number",
        ));
    }
    let url = snapshot["url"]
        .as_str()
        .ok_or_else(|| GitError::new(work_dir, "Missing PR URL"))?;
    let (snapshot_repository, snapshot_number) =
        parse_snapshot_pr_url(url).map_err(|message| GitError::new(work_dir, message))?;
    if snapshot_number != number || !same_repository(&snapshot_repository, repository) {
        return Err(GitError::new(
            work_dir,
            "GitHub returned a PR from a different repository or number",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PrWorkflowAction {
    Status,
    Feedback,
    Wait,
    Logs,
    Comment,
    Reply,
    Ready,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrWorkflowRequest {
    action: PrWorkflowAction,
    repository: String,
    number: u64,
    body: Option<String>,
    comment_id: Option<u64>,
    run_id: Option<u64>,
    cursor: Option<String>,
    timeout_seconds: Option<u64>,
}

pub struct PrWaitSettings {
    pub cursor: Option<String>,
    pub timeout_seconds: u64,
}

impl PrWorkflowRequest {
    pub fn wait_settings(&self) -> Result<Option<PrWaitSettings>, String> {
        self.validate()?;
        Ok(
            matches!(self.action, PrWorkflowAction::Wait).then(|| PrWaitSettings {
                cursor: self.cursor.clone(),
                timeout_seconds: self.timeout_seconds.unwrap_or(1800),
            }),
        )
    }

    pub fn mutates(&self) -> bool {
        matches!(
            self.action,
            PrWorkflowAction::Comment | PrWorkflowAction::Reply | PrWorkflowAction::Ready
        )
    }

    fn validate(&self) -> Result<GitHubRepository, String> {
        let repository = parse_pr_repository(&self.repository)?;
        validate_github_number(self.number, "pull request")?;
        if let Some(cursor) = &self.cursor {
            if cursor.len() != 64 || !cursor.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(
                    "cursor must be the 64-character watch_cursor from feedback or wait".into(),
                );
            }
        }
        if self
            .timeout_seconds
            .is_some_and(|seconds| !(1..=1800).contains(&seconds))
        {
            return Err("timeout_seconds must be between 1 and 1800".into());
        }
        if !matches!(self.action, PrWorkflowAction::Wait)
            && (self.cursor.is_some() || self.timeout_seconds.is_some())
        {
            return Err("cursor and timeout_seconds are only supported for wait".into());
        }
        if matches!(
            self.action,
            PrWorkflowAction::Comment | PrWorkflowAction::Reply
        ) {
            validated_text(self.body.as_deref().unwrap_or(""), "reply body")?;
        }
        if matches!(self.action, PrWorkflowAction::Reply) {
            validate_github_number(self.comment_id.unwrap_or(0), "review comment")?;
        }
        if matches!(self.action, PrWorkflowAction::Logs) {
            validate_github_number(self.run_id.unwrap_or(0), "workflow run")?;
        }
        Ok(repository)
    }
}

const STATUS_FIELDS: &str = "number,url,state,isDraft,headRefName,headRefOid,baseRefName,mergeable,mergeStateStatus,reviewDecision,reviewRequests,statusCheckRollup";
const THREADS_QUERY: &str = "query($owner: String!, $repo: String!, $number: Int!, $cursor: String) { \
    repository(owner: $owner, name: $repo) { pullRequest(number: $number) { \
        reviewThreads(first: 100, after: $cursor) { \
            pageInfo { hasNextPage endCursor } \
            nodes { id isResolved isOutdated comments(first: 1) { nodes { databaseId url } } } \
        } \
    } } \
}";

fn parse_json(work_dir: &Path, text: &str) -> Result<Value, GitError> {
    serde_json::from_str(text)
        .map_err(|error| GitError::new(work_dir, format!("Invalid GitHub response: {error}")))
}

fn status_args(repository: &GitHubRepository, number: u64) -> Vec<String> {
    vec![
        "pr".into(),
        "view".into(),
        number.to_string(),
        "--repo".into(),
        repository_selector(repository),
        "--json".into(),
        STATUS_FIELDS.into(),
    ]
}

fn ready_args(repository: &GitHubRepository, number: u64) -> Vec<String> {
    vec![
        "pr".into(),
        "ready".into(),
        number.to_string(),
        "--repo".into(),
        repository_selector(repository),
    ]
}

fn logs_args(repository: &GitHubRepository, run_id: u64) -> Vec<String> {
    vec![
        "run".into(),
        "view".into(),
        run_id.to_string(),
        "--log-failed".into(),
        "--repo".into(),
        repository_selector(repository),
    ]
}

fn comment_args(
    repository: &GitHubRepository,
    number: u64,
    body: &str,
) -> Result<Vec<String>, String> {
    let mut args = github_pr_comment_args(number, body)?;
    args.extend(["--repo".into(), repository_selector(repository)]);
    Ok(args)
}

fn pull_request_url(repository: &GitHubRepository, number: u64) -> String {
    format!(
        "https://{}/{}/{}/pull/{number}",
        repository.host, repository.owner, repository.repo
    )
}

fn status(
    work_dir: &Path,
    repository: &GitHubRepository,
    number: u64,
) -> Result<Value, GitError> {
    let output = execute_gh(work_dir, &status_args(repository, number))?;
    let value = parse_json(work_dir, &output)?;
    if value["number"].as_u64() != Some(number)
        || value["headRefOid"].as_str().unwrap_or("").is_empty()
    {
        return Err(GitError::new(
            work_dir,
            "GitHub did not confirm the PR identity and head SHA",
        ));
    }
    Ok(value)
}

fn collect_threads(
    work_dir: &Path,
    mut fetch: impl FnMut(Option<&str>) -> Result<Value, GitError>,
) -> Result<Vec<Value>, GitError> {
    let mut threads = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let value = fetch(cursor.as_deref())?;
        if let Some(message) = graphql_errors(&value) {
            return Err(GitError::new(work_dir, message));
        }
        let connection = &value["data"]["repository"]["pullRequest"]["reviewThreads"];
        let nodes = connection["nodes"]
            .as_array()
            .ok_or_else(|| GitError::new(work_dir, "Missing review threads"))?;
        threads.extend(nodes.iter().cloned());
        match connection["pageInfo"]["hasNextPage"].as_bool() {
            Some(false) => return Ok(threads),
            Some(true) => {
                let next = connection["pageInfo"]["endCursor"]
                    .as_str()
                    .filter(|next| !next.is_empty() && Some(*next) != cursor.as_deref())
                    .ok_or_else(|| {
                        GitError::new(work_dir, "Review thread pagination did not advance")
                    })?;
                cursor = Some(next.to_owned());
            }
            None => {
                return Err(GitError::new(
                    work_dir,
                    "Missing review thread pagination state",
                ));
            }
        }
    }
    Err(GitError::new(
        work_dir,
        "Review thread page limit reached; feedback is incomplete",
    ))
}

pub fn execute_pr_workflow(
    work_dir: &Path,
    request: PrWorkflowRequest,
) -> Result<String, GitError> {
    let repository = request
        .validate()
        .map_err(|message| GitError::new(work_dir, message))?;
    let number = request.number;
    let snapshot = status(work_dir, &repository, number)?;
    validate_snapshot_identity(work_dir, &snapshot, &repository, number)?;
    let url = snapshot["url"].as_str().unwrap();
    match request.action {
        PrWorkflowAction::Status => Ok(snapshot.to_string()),
        // The session adapter owns the cancellable async wait; this function
        // samples the same complete feedback for both actions.
        PrWorkflowAction::Feedback | PrWorkflowAction::Wait => {
            let repo = format!("repos/{}/{}", repository.owner, repository.repo);
            let mut feedback = json!({"status": snapshot});
            for (key, endpoint) in [
                (
                    "conversation_comments",
                    format!("{repo}/issues/{number}/comments"),
                ),
                ("reviews", format!("{repo}/pulls/{number}/reviews")),
                ("inline_comments", format!("{repo}/pulls/{number}/comments")),
            ] {
                let output = execute_gh(
                    work_dir,
                    &github_api_args(&repository.host, &[&endpoint, "--paginate", "--slurp"]),
                )?;
                let pages = parse_json(work_dir, &output)?;
                let pages = pages
                    .as_array()
                    .ok_or_else(|| GitError::new(work_dir, "Missing feedback pages"))?;
                let mut items = Vec::new();
                for page in pages {
                    items.extend(
                        page.as_array()
                            .ok_or_else(|| GitError::new(work_dir, "Invalid feedback page"))?
                            .iter()
                            .cloned(),
                    );
                }
                feedback[key] = Value::Array(items);
            }
            let threads = collect_threads(work_dir, |cursor| {
                let payload = json!({"query": THREADS_QUERY, "variables": {
                    "owner": repository.owner, "repo": repository.repo, "number": number, "cursor": cursor,
                }});
                let output =
                    execute_gh_json(work_dir, &github_graphql_args(&repository.host), &payload)?;
                parse_json(work_dir, &output)
            })?;
            feedback["threads"] = Value::Array(threads);
            Ok(feedback.to_string())
        }
        PrWorkflowAction::Logs => execute_gh(
            work_dir,
            &logs_args(&repository, request.run_id.unwrap()),
        ),
        PrWorkflowAction::Comment => {
            let args = comment_args(
                &repository,
                number,
                request.body.as_deref().unwrap(),
            )
            .map_err(|message| GitError::new(work_dir, message))?;
            invalidate_github_cache(work_dir);
            execute_gh(work_dir, &args)
        }
        PrWorkflowAction::Reply => reply_to_pull_request_review_comment(
            work_dir,
            &pull_request_url(&repository, number),
            request.comment_id.unwrap(),
            request.body.as_deref().unwrap(),
        ),
        PrWorkflowAction::Ready => {
            validate_ready_checkout(
                work_dir,
                snapshot["headRefName"].as_str(),
                snapshot["headRefOid"].as_str(),
            )?;
            if snapshot["state"].as_str() != Some("OPEN") {
                return Err(GitError::new(
                    work_dir,
                    "Only an open PR can be marked ready",
                ));
            }
            match snapshot["isDraft"].as_bool() {
                Some(false) => return Ok(format!("PR is already ready for review: {url}")),
                Some(true) => {}
                None => return Err(GitError::new(work_dir, "Missing PR draft state")),
            }
            let result = execute_gh(work_dir, &ready_args(&repository, number));
            invalidate_github_cache(work_dir);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        collect_threads, comment_args, logs_args, parse_pr_repository, ready_args, status_args,
        validate_ready_checkout, validate_snapshot_identity, PrWorkflowRequest,
    };
    use serde_json::json;
    use std::{fs, path::Path, process::Command};

    fn run_git(work_dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn ready_checkout() -> (tempfile::TempDir, String, String) {
        let directory = tempfile::tempdir().unwrap();
        run_git(directory.path(), &["init", "-q"]);
        run_git(
            directory.path(),
            &["config", "user.name", "Threadlane Tests"],
        );
        run_git(
            directory.path(),
            &["config", "user.email", "threadlane-tests@example.com"],
        );
        fs::write(directory.path().join("file.txt"), "first\n").unwrap();
        run_git(directory.path(), &["add", "file.txt"]);
        run_git(directory.path(), &["commit", "-qm", "first"]);
        let branch = run_git(directory.path(), &["branch", "--show-current"]);
        let head_oid = run_git(directory.path(), &["rev-parse", "--verify", "HEAD"]);
        (directory, branch, head_oid)
    }

    #[test]
    fn workflow_requests_validate_before_accessing_git() {
        for args in [
            json!({"action":"status", "repository":"owner/repo", "number":0}),
            json!({"action":"reply", "repository":"owner/repo", "number":1, "body":"fix"}),
            json!({"action":"comment", "repository":"owner/repo", "number":1, "body":" "}),
            json!({"action":"logs", "repository":"owner/repo", "number":1, "run_id":0}),
            json!({"action":"wait", "repository":"owner/repo", "number":1, "timeout_seconds":0}),
            json!({"action":"wait", "repository":"owner/repo", "number":1, "timeout_seconds":1801}),
            json!({"action":"wait", "repository":"owner/repo", "number":1, "cursor":"invalid"}),
            json!({"action":"feedback", "repository":"owner/repo", "number":1, "timeout_seconds":60}),
        ] {
            assert!(
                serde_json::from_value::<PrWorkflowRequest>(args)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(
            serde_json::from_value::<PrWorkflowRequest>(
                json!({"action":"merge", "repository":"owner/repo", "number":1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<PrWorkflowRequest>(json!({"action":"status", "number":1}))
                .is_err()
        );
        for (action, mutates) in [
            ("status", false),
            ("feedback", false),
            ("wait", false),
            ("logs", false),
            ("comment", true),
            ("reply", true),
            ("ready", true),
        ] {
            let request: PrWorkflowRequest =
                serde_json::from_value(
                    json!({"action":action, "repository":"owner/repo", "number":1}),
                )
                .unwrap();
            assert_eq!(request.mutates(), mutates);
        }
    }

    #[test]
    fn workflow_repository_requires_exact_safe_coordinates() {
        let github = parse_pr_repository("owner/repo").unwrap();
        assert_eq!(github.host, "github.com");
        assert_eq!(github.owner, "owner");
        assert_eq!(github.repo, "repo");

        let enterprise = parse_pr_repository("github.example.com/owner/repo").unwrap();
        assert_eq!(enterprise.host, "github.example.com");
        assert_eq!(enterprise.owner, "owner");
        assert_eq!(enterprise.repo, "repo");

        for value in [
            "",
            "owner",
            "/owner/repo",
            "owner//repo",
            "owner/repo/",
            "host/owner/repo/path",
            "-H/owner/repo",
            "host/-owner/repo",
            "owner/-repo",
            "host/owner/..",
            "owner/../repo",
            "owner/repo?query",
            "owner/repo#fragment",
            "user@host/owner/repo",
            "host/owner/re po",
            "host/owner/repo\n",
            "https:/owner/repo",
        ] {
            assert!(
                parse_pr_repository(value).is_err(),
                "unexpectedly accepted repository {value:?}"
            );
        }
    }

    #[test]
    fn workflow_command_arguments_pin_the_requested_repository() {
        let repository = parse_pr_repository("github.example.com/owner/repo").unwrap();
        let expected = "github.example.com/owner/repo";
        let arguments = [
            status_args(&repository, 42),
            ready_args(&repository, 42),
            logs_args(&repository, 7),
            comment_args(&repository, 42, "Please fix this").unwrap(),
        ];
        for args in arguments {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "--repo" && pair[1] == expected),
                "missing pinned repository in {args:?}"
            );
        }
    }

    #[test]
    fn workflow_snapshot_must_match_repository_and_number() {
        let repository = parse_pr_repository("owner/repo").unwrap();
        let matching = json!({
            "number": 42,
            "url": "https://GITHUB.com/OWNER/REPO/pull/42"
        });
        assert!(
            validate_snapshot_identity(Path::new("."), &matching, &repository, 42).is_ok()
        );

        let different_repository = json!({
            "number": 42,
            "url": "https://github.com/fork/repo/pull/42"
        });
        assert!(
            validate_snapshot_identity(
                Path::new("."),
                &different_repository,
                &repository,
                42
            )
            .is_err()
        );

        let extra_path = json!({
            "number": 42,
            "url": "https://github.com/owner/repo/extra/pull/42"
        });
        assert!(validate_snapshot_identity(Path::new("."), &extra_path, &repository, 42).is_err());
    }

    #[test]
    fn workflow_threads_follow_pages_and_fail_closed() {
        let mut calls = 0;
        let threads = collect_threads(Path::new("."), |cursor| {
            calls += 1;
            assert_eq!(cursor, (calls == 2).then_some("next"));
            Ok(
                json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                    "nodes":[{"id":calls}], "pageInfo":{"hasNextPage":calls == 1,"endCursor":"next"}
                }}}}}),
            )
        })
        .unwrap();
        assert_eq!(threads.len(), 2);
        for value in [
            json!({"errors":[{"message":"denied"}]}),
            json!({"data":null}),
            json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                "nodes":[], "pageInfo":{"hasNextPage":true,"endCursor":null}
            }}}}}),
        ] {
            assert!(collect_threads(Path::new("."), |_| Ok(value.clone())).is_err());
        }
    }

    #[test]
    fn ready_checkout_requires_matching_branch_and_head() {
        let (directory, branch, head_oid) = ready_checkout();
        assert!(validate_ready_checkout(directory.path(), Some(&branch), Some(&head_oid)).is_ok());

        fs::write(directory.path().join("file.txt"), "second\n").unwrap();
        run_git(directory.path(), &["add", "file.txt"]);
        run_git(directory.path(), &["commit", "-qm", "second"]);
        assert_eq!(
            run_git(directory.path(), &["branch", "--show-current"]),
            branch
        );
        assert_ne!(
            run_git(directory.path(), &["rev-parse", "--verify", "HEAD"]),
            head_oid
        );
        assert!(validate_ready_checkout(directory.path(), Some(&branch), Some(&head_oid)).is_err());
        assert!(validate_ready_checkout(directory.path(), Some(&branch), None).is_err());
    }

    #[test]
    fn ready_checkout_rejects_detached_and_wrong_branches() {
        let (directory, branch, head_oid) = ready_checkout();
        run_git(directory.path(), &["checkout", "-b", "other"]);
        assert!(validate_ready_checkout(directory.path(), Some(&branch), Some(&head_oid)).is_err());

        run_git(directory.path(), &["checkout", "--detach", "HEAD"]);
        assert!(validate_ready_checkout(directory.path(), Some(&branch), Some(&head_oid)).is_err());
    }

    #[test]
    fn ready_checkout_rejects_an_unborn_head() {
        let directory = tempfile::tempdir().unwrap();
        run_git(directory.path(), &["init", "-q"]);
        let branch = run_git(directory.path(), &["branch", "--show-current"]);
        assert!(
            validate_ready_checkout(directory.path(), Some(&branch), Some("head-sha")).is_err()
        );
    }
}
