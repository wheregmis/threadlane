use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Credentials for remote forge reads (`pr://`, `mr://`, `issue://`, and
/// GitHub/GitLab URLs).
///
/// Historically this module resolved tokens itself from a hardcoded
/// application credential store. That store now belongs to the host: callers
/// pass stored tokens here, and this module only falls back to ambient
/// environment behavior (`gh`/`glab` CLIs, then `GITHUB_TOKEN`/`GH_TOKEN` /
/// `GITLAB_TOKEN`/`GL_TOKEN`). `Default` is empty (ambient fallbacks only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteCredentials {
    pub github_token: Option<String>,
    pub gitlab_token: Option<String>,
}

impl RemoteCredentials {
    /// Ambient environment only: `GITHUB_TOKEN`/`GH_TOKEN` and
    /// `GITLAB_TOKEN`/`GL_TOKEN` when set and non-blank.
    pub fn from_env() -> Self {
        fn env_token(keys: &[&str]) -> Option<String> {
            keys.iter()
                .filter_map(|key| std::env::var(key).ok())
                .map(|value| value.trim().to_string())
                .find(|value| !value.is_empty())
        }
        Self {
            github_token: env_token(&["GITHUB_TOKEN", "GH_TOKEN"]),
            gitlab_token: env_token(&["GITLAB_TOKEN", "GL_TOKEN"]),
        }
    }
}

/// Resolves the GitHub token: injected stored token first, then the `gh` CLI,
/// then the environment. The stored-file lookup lives with the host's
/// credential store, not here.
fn resolve_github_token(injected: Option<&str>) -> Option<String> {
    if let Some(token) = injected.map(str::trim).filter(|t| !t.is_empty()) {
        return Some(token.to_string());
    }
    if let Ok(output) = Command::new("gh").args(["auth", "token"]).output() {
        if output.status.success() {
            let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    RemoteCredentials::from_env().github_token
}

/// Resolves the GitLab token: injected stored token first, then the
/// environment. (There is no `glab` token probe; `glab` authenticates its own
/// CLI calls in Strategy 1.)
fn resolve_gitlab_token(injected: Option<&str>) -> Option<String> {
    if let Some(token) = injected.map(str::trim).filter(|t| !t.is_empty()) {
        return Some(token.to_string());
    }
    RemoteCredentials::from_env().gitlab_token
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoProvider {
    GitHub,
    GitLab,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRemoteRef {
    pub provider: Option<RepoProvider>,
    pub host: Option<String>,
    pub owner_repo: Option<String>,
    pub kind: String, // "pr", "mr", or "issue"
    pub number: String,
}

pub fn parse_remote_ref(input: &str) -> Option<ParsedRemoteRef> {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix("pr://") {
        parse_scheme_or_path(Some(RepoProvider::GitHub), "pr", rest)
    } else if let Some(rest) = input.strip_prefix("mr://") {
        parse_scheme_or_path(Some(RepoProvider::GitLab), "mr", rest)
    } else if let Some(rest) = input.strip_prefix("issue://") {
        parse_scheme_or_path(None, "issue", rest)
    } else if let Some(rest) = input
        .strip_prefix("https://github.com/")
        .or_else(|| input.strip_prefix("http://github.com/"))
    {
        parse_github_url_path(rest)
    } else if input.starts_with("https://") || input.starts_with("http://") {
        parse_gitlab_or_generic_url(input)
    } else {
        None
    }
}

#[allow(dead_code)]
// Backward compatibility alias for external callers
pub fn parse_github_ref(input: &str) -> Option<ParsedGitHubRef> {
    let parsed = parse_remote_ref(input)?;
    let (owner, repo) = if let Some(ref or) = parsed.owner_repo {
        let parts: Vec<&str> = or.split('/').collect();
        if parts.len() == 2 {
            (Some(parts[0].to_string()), Some(parts[1].to_string()))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    Some(ParsedGitHubRef {
        owner,
        repo,
        kind: if parsed.kind == "mr" {
            "pr".to_string()
        } else {
            parsed.kind
        },
        number: parsed.number,
    })
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedGitHubRef {
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub kind: String,
    pub number: String,
}

fn parse_scheme_or_path(
    provider: Option<RepoProvider>,
    default_kind: &str,
    rest: &str,
) -> Option<ParsedRemoteRef> {
    let rest = rest.trim_matches('/');
    if rest.is_empty() {
        return None;
    }

    if rest.bytes().all(|b| b.is_ascii_digit()) {
        return Some(ParsedRemoteRef {
            provider,
            host: None,
            owner_repo: None,
            kind: default_kind.to_string(),
            number: rest.to_string(),
        });
    }

    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 3 {
        let owner = parts[0];
        let repo = parts[1];
        let owner_repo = format!("{owner}/{repo}");
        if parts.len() >= 4
            && (parts[2] == "pull"
                || parts[2] == "pulls"
                || parts[2] == "merge_requests"
                || parts[2] == "issues"
                || parts[2] == "issue")
        {
            let kind = match parts[2] {
                "pull" | "pulls" => "pr",
                "merge_requests" => "mr",
                _ => "issue",
            };
            let number = parts[3];
            if number.bytes().all(|b| b.is_ascii_digit()) {
                return Some(ParsedRemoteRef {
                    provider,
                    host: None,
                    owner_repo: Some(owner_repo),
                    kind: kind.to_string(),
                    number: number.to_string(),
                });
            }
        } else {
            let number = parts[2];
            if number.bytes().all(|b| b.is_ascii_digit()) {
                return Some(ParsedRemoteRef {
                    provider,
                    host: None,
                    owner_repo: Some(owner_repo),
                    kind: default_kind.to_string(),
                    number: number.to_string(),
                });
            }
        }
    }

    None
}

fn parse_github_url_path(rest: &str) -> Option<ParsedRemoteRef> {
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 4 {
        let owner = parts[0];
        let repo = parts[1];
        let kind = match parts[2] {
            "pull" | "pulls" => "pr",
            "issues" | "issue" => "issue",
            _ => return None,
        };
        let number = parts[3];
        if number.bytes().all(|b| b.is_ascii_digit()) {
            return Some(ParsedRemoteRef {
                provider: Some(RepoProvider::GitHub),
                host: Some("github.com".to_string()),
                owner_repo: Some(format!("{owner}/{repo}")),
                kind: kind.to_string(),
                number: number.to_string(),
            });
        }
    }
    None
}

fn parse_gitlab_or_generic_url(url: &str) -> Option<ParsedRemoteRef> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;

    let (host, path) = without_scheme.split_once('/')?;
    let is_gitlab = host.contains("gitlab")
        || path.contains("/-/merge_requests/")
        || path.contains("/-/issues/");

    if is_gitlab {
        let (project_path, rest_path) = if let Some(idx) = path.find("/-/") {
            (&path[..idx], &path[idx + 3..])
        } else {
            (path, "")
        };

        let rest_parts: Vec<&str> = rest_path.split('/').filter(|s| !s.is_empty()).collect();
        if rest_parts.len() >= 2 {
            let kind = match rest_parts[0] {
                "merge_requests" => "mr",
                "issues" => "issue",
                _ => return None,
            };
            let number = rest_parts[1];
            if number.bytes().all(|b| b.is_ascii_digit()) {
                return Some(ParsedRemoteRef {
                    provider: Some(RepoProvider::GitLab),
                    host: Some(host.to_string()),
                    owner_repo: Some(project_path.trim_matches('/').to_string()),
                    kind: kind.to_string(),
                    number: number.to_string(),
                });
            }
        }
    }

    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRemoteInfo {
    pub provider: RepoProvider,
    pub host: String,
    pub owner_repo: String,
}

pub fn get_git_remote_info(root: &Path) -> Option<GitRemoteInfo> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    parse_git_remote_url(&remote)
}

pub fn parse_git_remote_url(remote: &str) -> Option<GitRemoteInfo> {
    let remote = remote.strip_suffix(".git").unwrap_or(remote).trim();

    // SSH forms: git@github.com:owner/repo or git@gitlab.com:group/subgroup/repo
    if let Some(rest) = remote.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            let provider = if host.contains("gitlab") {
                RepoProvider::GitLab
            } else {
                RepoProvider::GitHub
            };
            return Some(GitRemoteInfo {
                provider,
                host: host.to_string(),
                owner_repo: path.trim_matches('/').to_string(),
            });
        }
    }

    // HTTPS forms: https://github.com/owner/repo or https://gitlab.com/group/repo
    if let Some(rest) = remote
        .strip_prefix("https://")
        .or_else(|| remote.strip_prefix("http://"))
    {
        if let Some((host, path)) = rest.split_once('/') {
            let provider = if host.contains("gitlab") {
                RepoProvider::GitLab
            } else {
                RepoProvider::GitHub
            };
            return Some(GitRemoteInfo {
                provider,
                host: host.to_string(),
                owner_repo: path.trim_matches('/').to_string(),
            });
        }
    }

    None
}

#[allow(dead_code)]
// Backward compatibility helper
pub fn github_owner_repo(remote: &str) -> Option<(&str, &str)> {
    let remote_clean = remote.strip_suffix(".git").unwrap_or(remote).trim();
    if remote_clean.contains("gitlab") {
        return None;
    }
    let path = if let Some(rest) = remote_clean.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = remote_clean
        .strip_prefix("https://github.com/")
        .or_else(|| remote_clean.strip_prefix("http://github.com/"))
    {
        rest
    } else {
        return None;
    };
    path.split_once('/')
}

pub fn remote_ref_path(root: &Path, reference: &str) -> String {
    try_remote_ref_path(root, reference).unwrap_or_else(|error| error)
}

pub fn try_remote_ref_path(root: &Path, reference: &str) -> Result<String, String> {
    try_remote_ref_path_with(root, reference, &RemoteCredentials::default())
}

/// Reads a remote `pr://`/`mr://`/`issue://` reference or forge URL using the
/// injected credentials plus ambient (`gh`/`glab`/env) fallbacks.
pub fn try_remote_ref_path_with(
    root: &Path,
    reference: &str,
    credentials: &RemoteCredentials,
) -> Result<String, String> {
    let parsed = parse_remote_ref(reference).ok_or_else(|| {
        format!(
            "Invalid repository reference '{reference}': expected pr://<num>, issue://<num>, mr://<num>, or GitHub/GitLab URL"
        )
    })?;

    let remote_info = get_git_remote_info(root);

    let provider = parsed
        .provider
        .or_else(|| remote_info.as_ref().map(|r| r.provider))
        .unwrap_or_else(|| {
            if parsed.kind == "mr" {
                RepoProvider::GitLab
            } else {
                RepoProvider::GitHub
            }
        });

    let host = parsed
        .host
        .or_else(|| remote_info.as_ref().map(|r| r.host.clone()))
        .unwrap_or_else(|| match provider {
            RepoProvider::GitHub => "github.com".to_string(),
            RepoProvider::GitLab => "gitlab.com".to_string(),
        });

    let owner_repo = match parsed.owner_repo {
        Some(or) => or,
        None => match remote_info {
            Some(info) => info.owner_repo,
            None => {
                return Err(format!(
                    "{}://{} requires a git origin remote or an explicit repository URL (e.g. pr://owner/repo/{})",
                    parsed.kind, parsed.number, parsed.number
                ));
            }
        },
    };

    match provider {
        RepoProvider::GitHub => fetch_github(
            root,
            &host,
            &owner_repo,
            &parsed.kind,
            &parsed.number,
            credentials.github_token.as_deref(),
        ),
        RepoProvider::GitLab => fetch_gitlab(
            root,
            &host,
            &owner_repo,
            &parsed.kind,
            &parsed.number,
            credentials.gitlab_token.as_deref(),
        ),
    }
}

#[allow(dead_code)]
pub fn github_path(root: &Path, reference: &str) -> String {
    github_path_with(root, reference, &RemoteCredentials::default())
}

#[allow(dead_code)]
pub fn github_path_with(
    root: &Path,
    reference: &str,
    credentials: &RemoteCredentials,
) -> String {
    try_remote_ref_path_with(root, reference, credentials).unwrap_or_else(|error| error)
}

fn fetch_github(
    root: &Path,
    host: &str,
    owner_repo: &str,
    kind: &str,
    number: &str,
    github_token: Option<&str>,
) -> Result<String, String> {
    let endpoint = match kind {
        "pr" | "mr" => format!("repos/{owner_repo}/pulls/{number}"),
        _ => format!("repos/{owner_repo}/issues/{number}"),
    };
    let web_url = match kind {
        "pr" | "mr" => format!("https://{host}/{owner_repo}/pull/{number}"),
        _ => format!("https://{host}/{owner_repo}/issues/{number}"),
    };

    // Strategy 1: gh CLI
    if let Ok(output) = Command::new("gh")
        .args(["api", "--hostname", host, &endpoint])
        .current_dir(root)
        .output()
    {
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            return Ok(format_github_markdown(kind, number, &raw));
        }
    }

    // Strategy 2: Direct curl HTTP API fallback
    let url = if host.eq_ignore_ascii_case("github.com") {
        format!("https://api.github.com/{endpoint}")
    } else {
        format!("https://{host}/api/v3/{endpoint}")
    };
    let mut cmd = Command::new("curl");
    cmd.args([
        "-s",
        "-L",
        "-H",
        "User-Agent: Threadlane",
        "-H",
        "Accept: application/vnd.github+json",
    ]);

    if let Some(token) = resolve_github_token(github_token) {
        cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    cmd.arg(&url);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            let message = serde_json::from_str::<Value>(&raw)
                .ok()
                .and_then(|value| value.get("message")?.as_str().map(str::to_owned));
            match message.as_deref() {
                Some("Not Found") => Err(format!(
                    "GitHub {kind} #{number} not found on {owner_repo}. (If private, set GITHUB_TOKEN, connect GitHub in Settings, or run 'gh auth login')"
                )),
                Some("Bad credentials") => Err("GitHub API error: Bad credentials. Check your GITHUB_TOKEN, Settings > Integrations, or 'gh auth status'.".to_string()),
                Some(_) => Err(format_github_markdown(kind, number, &raw)),
                None => Ok(format_github_markdown(kind, number, &raw)),
            }
        }
        Ok(output) => Err(format!(
            "GitHub API request for {web_url} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(e) => Err(format!(
            "{}://{number} requires 'gh' CLI or 'curl' with GITHUB_TOKEN: {e}",
            kind
        )),
    }
}

fn fetch_gitlab(
    root: &Path,
    host: &str,
    project_path: &str,
    kind: &str,
    number: &str,
    gitlab_token: Option<&str>,
) -> Result<String, String> {
    let encoded_project = project_path.replace('/', "%2F");
    let endpoint = match kind {
        "pr" | "mr" => format!("projects/{encoded_project}/merge_requests/{number}"),
        _ => format!("projects/{encoded_project}/issues/{number}"),
    };
    let web_url = match kind {
        "pr" | "mr" => format!("https://{host}/{project_path}/-/merge_requests/{number}"),
        _ => format!("https://{host}/{project_path}/-/issues/{number}"),
    };

    // Strategy 1: glab CLI
    let mut glab_cmd = Command::new("glab");
    glab_cmd.args(["api", &endpoint]).current_dir(root);
    if host != "gitlab.com" {
        glab_cmd.args(["--hostname", host]);
    }

    if let Ok(output) = glab_cmd.output() {
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            return Ok(format_gitlab_markdown(kind, number, &raw));
        }
    }

    // Strategy 2: Direct curl HTTP API fallback
    let url = format!("https://{host}/api/v4/{endpoint}");
    let mut cmd = Command::new("curl");
    cmd.args(["-s", "-L", "-H", "User-Agent: Threadlane"]);

    if let Some(tok) = resolve_gitlab_token(gitlab_token) {
        cmd.args(["-H", &format!("PRIVATE-TOKEN: {tok}")]);
    }
    cmd.arg(&url);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            let message = serde_json::from_str::<Value>(&raw)
                .ok()
                .and_then(|value| value.get("message")?.as_str().map(str::to_owned));
            match message.as_deref() {
                Some("404 Project Not Found") | Some("404 Not Found") => Err(format!(
                    "GitLab {kind} #{number} not found on {project_path}. (If private, set GITLAB_TOKEN or run 'glab auth login')"
                )),
                Some(_) => Err(format_gitlab_markdown(kind, number, &raw)),
                None => Ok(format_gitlab_markdown(kind, number, &raw)),
            }
        }
        Ok(output) => Err(format!(
            "GitLab API request for {web_url} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(e) => Err(format!(
            "{}://{number} requires 'glab' CLI or 'curl' with GITLAB_TOKEN: {e}",
            kind
        )),
    }
}

pub fn format_github_markdown(kind: &str, number: &str, raw_json: &str) -> String {
    let val: Value = match serde_json::from_str(raw_json) {
        Ok(v) => v,
        Err(_) => return raw_json.to_string(),
    };

    let title = val["title"].as_str().unwrap_or("Untitled");
    let state = val["state"].as_str().unwrap_or("unknown").to_uppercase();
    let author = val["user"]["login"].as_str().unwrap_or("unknown");
    let created_at = val["created_at"].as_str().unwrap_or("");
    let url = val["html_url"].as_str().unwrap_or("");
    let body = val["body"].as_str().unwrap_or("").trim();

    let mut out = String::new();
    if kind == "pr" || kind == "mr" {
        let head = val["head"]["ref"].as_str().unwrap_or("unknown");
        let base = val["base"]["ref"].as_str().unwrap_or("unknown");
        let additions = val["additions"].as_u64().unwrap_or(0);
        let deletions = val["deletions"].as_u64().unwrap_or(0);
        let changed_files = val["changed_files"].as_u64().unwrap_or(0);
        let merged = val["merged"].as_bool().unwrap_or(false);
        let status_str = if merged { "MERGED" } else { &state };

        out.push_str(&format!("# Pull Request #{number}: {title}\n\n"));
        out.push_str(&format!("- **Status:** {status_str}\n"));
        out.push_str(&format!("- **Author:** @{author}\n"));
        out.push_str(&format!("- **Branches:** `{head}` -> `{base}`\n"));
        if !created_at.is_empty() {
            out.push_str(&format!("- **Created:** {created_at}\n"));
        }
        if !url.is_empty() {
            out.push_str(&format!("- **URL:** {url}\n"));
        }
        out.push_str(&format!(
            "- **Changes:** +{additions} / -{deletions} ({changed_files} files changed)\n\n"
        ));
    } else {
        out.push_str(&format!("# Issue #{number}: {title}\n\n"));
        out.push_str(&format!("- **Status:** {state}\n"));
        out.push_str(&format!("- **Author:** @{author}\n"));
        if !created_at.is_empty() {
            out.push_str(&format!("- **Created:** {created_at}\n"));
        }
        if !url.is_empty() {
            out.push_str(&format!("- **URL:** {url}\n"));
        }
        let comments = val["comments"].as_u64().unwrap_or(0);
        out.push_str(&format!("- **Comments:** {comments}\n\n"));
    }

    out.push_str("## Description\n\n");
    if body.is_empty() {
        out.push_str("*No description provided.*");
    } else {
        out.push_str(body);
    }

    out
}

pub fn format_gitlab_markdown(kind: &str, number: &str, raw_json: &str) -> String {
    let val: Value = match serde_json::from_str(raw_json) {
        Ok(v) => v,
        Err(_) => return raw_json.to_string(),
    };

    let title = val["title"].as_str().unwrap_or("Untitled");
    let state = val["state"].as_str().unwrap_or("unknown").to_uppercase();
    let author = val["author"]["username"].as_str().unwrap_or("unknown");
    let created_at = val["created_at"].as_str().unwrap_or("");
    let url = val["web_url"].as_str().unwrap_or("");
    let body = val["description"].as_str().unwrap_or("").trim();

    let mut out = String::new();
    if kind == "mr" || kind == "pr" {
        let source_branch = val["source_branch"].as_str().unwrap_or("unknown");
        let target_branch = val["target_branch"].as_str().unwrap_or("unknown");

        out.push_str(&format!("# Merge Request !{number}: {title}\n\n"));
        out.push_str(&format!("- **Status:** {state}\n"));
        out.push_str(&format!("- **Author:** @{author}\n"));
        out.push_str(&format!(
            "- **Branches:** `{source_branch}` -> `{target_branch}`\n"
        ));
        if !created_at.is_empty() {
            out.push_str(&format!("- **Created:** {created_at}\n"));
        }
        if !url.is_empty() {
            out.push_str(&format!("- **URL:** {url}\n\n"));
        }
    } else {
        out.push_str(&format!("# Issue #{number}: {title}\n\n"));
        out.push_str(&format!("- **Status:** {state}\n"));
        out.push_str(&format!("- **Author:** @{author}\n"));
        if !created_at.is_empty() {
            out.push_str(&format!("- **Created:** {created_at}\n"));
        }
        if !url.is_empty() {
            out.push_str(&format!("- **URL:** {url}\n\n"));
        }
    }

    out.push_str("## Description\n\n");
    if body.is_empty() {
        out.push_str("*No description provided.*");
    } else {
        out.push_str(body);
    }

    out
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn try_skill(root: &Path, name: &str) -> Result<String, String> {
    let clean_name = name.trim_matches('/');
    if clean_name.is_empty() {
        return Err("Error: 'skill://' reference requires a skill name".to_string());
    }

    let mut search_dirs = vec![
        root.join(".threadlane/skills"),
        root.join(".agents/skills"),
        root.join(".pi/skills"),
    ];

    if let Some(home) = dirs_home() {
        search_dirs.push(home.join(".threadlane/skills"));
        search_dirs.push(home.join(".agents/skills"));
        search_dirs.push(home.join(".pi/agent/skills"));
    }

    for dir in search_dirs {
        let candidates = vec![
            dir.join(clean_name),
            dir.join(format!("{clean_name}.md")),
            dir.join(clean_name).join("SKILL.md"),
            dir.join(clean_name).join("skill.md"),
        ];

        for candidate in candidates {
            if candidate.is_file() {
                if let Ok(content) = fs::read_to_string(&candidate) {
                    return Ok(content);
                }
            }
        }
    }

    Err(format!(
        "Unknown skill reference '{clean_name}': No skill file found in workspace or user skills directories"
    ))
}

pub fn try_agent(root: &Path, name: &str) -> Result<String, String> {
    let clean_name = name.trim_matches('/');
    if clean_name.is_empty() {
        return Err("Error: 'agent://' reference requires an agent name".to_string());
    }

    let mut search_dirs = vec![root.join(".threadlane/agents"), root.join(".agents/agents")];

    if let Some(home) = dirs_home() {
        search_dirs.push(home.join(".threadlane/agents"));
        search_dirs.push(home.join(".agents/agents"));
    }

    for dir in search_dirs {
        let candidates = vec![dir.join(clean_name), dir.join(format!("{clean_name}.md"))];

        for candidate in candidates {
            if candidate.is_file() {
                if let Ok(content) = fs::read_to_string(&candidate) {
                    return Ok(content);
                }
            }
        }
    }

    Err(format!(
        "Unknown agent reference '{clean_name}': No agent file found in workspace or user agent directories"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_remote_forms() {
        assert_eq!(
            parse_git_remote_url("git@github.com:owner/repo.git"),
            Some(GitRemoteInfo {
                provider: RepoProvider::GitHub,
                host: "github.com".to_string(),
                owner_repo: "owner/repo".to_string(),
            })
        );
        assert_eq!(
            parse_git_remote_url("https://github.com/owner/repo"),
            Some(GitRemoteInfo {
                provider: RepoProvider::GitHub,
                host: "github.com".to_string(),
                owner_repo: "owner/repo".to_string(),
            })
        );
        assert_eq!(
            parse_git_remote_url("git@gitlab.com:gitlab-org/gitlab.git"),
            Some(GitRemoteInfo {
                provider: RepoProvider::GitLab,
                host: "gitlab.com".to_string(),
                owner_repo: "gitlab-org/gitlab".to_string(),
            })
        );
        assert_eq!(
            parse_git_remote_url("https://gitlab.mycorp.io/eng/subteam/service.git"),
            Some(GitRemoteInfo {
                provider: RepoProvider::GitLab,
                host: "gitlab.mycorp.io".to_string(),
                owner_repo: "eng/subteam/service".to_string(),
            })
        );
    }

    #[test]
    fn parses_remote_refs_correctly() {
        assert_eq!(
            parse_remote_ref("pr://70"),
            Some(ParsedRemoteRef {
                provider: Some(RepoProvider::GitHub),
                host: None,
                owner_repo: None,
                kind: "pr".to_string(),
                number: "70".to_string(),
            })
        );
        assert_eq!(
            parse_remote_ref("mr://15"),
            Some(ParsedRemoteRef {
                provider: Some(RepoProvider::GitLab),
                host: None,
                owner_repo: None,
                kind: "mr".to_string(),
                number: "15".to_string(),
            })
        );
        assert_eq!(
            parse_remote_ref("issue://12"),
            Some(ParsedRemoteRef {
                provider: None,
                host: None,
                owner_repo: None,
                kind: "issue".to_string(),
                number: "12".to_string(),
            })
        );
        assert_eq!(
            parse_remote_ref("pr://wheregmis/threadlane/70"),
            Some(ParsedRemoteRef {
                provider: Some(RepoProvider::GitHub),
                host: None,
                owner_repo: Some("wheregmis/threadlane".to_string()),
                kind: "pr".to_string(),
                number: "70".to_string(),
            })
        );
        assert_eq!(
            parse_remote_ref("https://github.com/wheregmis/threadlane/pull/70"),
            Some(ParsedRemoteRef {
                provider: Some(RepoProvider::GitHub),
                host: Some("github.com".to_string()),
                owner_repo: Some("wheregmis/threadlane".to_string()),
                kind: "pr".to_string(),
                number: "70".to_string(),
            })
        );
        assert_eq!(
            parse_remote_ref("https://gitlab.com/gitlab-org/gitlab/-/merge_requests/99"),
            Some(ParsedRemoteRef {
                provider: Some(RepoProvider::GitLab),
                host: Some("gitlab.com".to_string()),
                owner_repo: Some("gitlab-org/gitlab".to_string()),
                kind: "mr".to_string(),
                number: "99".to_string(),
            })
        );
        assert_eq!(
            parse_remote_ref("https://gitlab.example.com/company/project/-/issues/404"),
            Some(ParsedRemoteRef {
                provider: Some(RepoProvider::GitLab),
                host: Some("gitlab.example.com".to_string()),
                owner_repo: Some("company/project".to_string()),
                kind: "issue".to_string(),
                number: "404".to_string(),
            })
        );
    }

    #[test]
    fn formats_github_pr_markdown() {
        let json = r#"{
            "title": "Add virtual repository schemes",
            "state": "open",
            "user": { "login": "alice" },
            "created_at": "2026-08-19T20:00:00Z",
            "html_url": "https://github.com/owner/repo/pull/70",
            "body": "This PR adds pr:// and issue:// connectors.",
            "head": { "ref": "feature/pr-schemes" },
            "base": { "ref": "main" },
            "additions": 150,
            "deletions": 20,
            "changed_files": 3,
            "merged": false
        }"#;

        let formatted = format_github_markdown("pr", "70", json);
        assert!(formatted.contains("# Pull Request #70: Add virtual repository schemes"));
        assert!(formatted.contains("- **Status:** OPEN"));
        assert!(formatted.contains("- **Author:** @alice"));
        assert!(formatted.contains("- **Branches:** `feature/pr-schemes` -> `main`"));
        assert!(formatted.contains("- **Changes:** +150 / -20 (3 files changed)"));
        assert!(formatted.contains("This PR adds pr:// and issue:// connectors."));
    }

    #[test]
    fn formats_gitlab_mr_markdown() {
        let json = r#"{
            "title": "Fix pipeline timeout",
            "state": "opened",
            "author": { "username": "bob" },
            "created_at": "2026-08-19T21:00:00Z",
            "web_url": "https://gitlab.com/owner/repo/-/merge_requests/15",
            "description": "Increases runner timeout to 60m.",
            "source_branch": "fix/runner",
            "target_branch": "main"
        }"#;

        let formatted = format_gitlab_markdown("mr", "15", json);
        assert!(formatted.contains("# Merge Request !15: Fix pipeline timeout"));
        assert!(formatted.contains("- **Status:** OPENED"));
        assert!(formatted.contains("- **Author:** @bob"));
        assert!(formatted.contains("- **Branches:** `fix/runner` -> `main`"));
        assert!(formatted.contains("Increases runner timeout to 60m."));
    }

    #[test]
    fn discovers_skills_in_subdirectories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        let skill_dir = root.join(".agents/skills/my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "my skill content").unwrap();

        assert_eq!(try_skill(root, "my-skill").unwrap(), "my skill content");
    }

    #[test]
    fn injected_tokens_take_precedence_and_are_trimmed() {
        assert_eq!(
            resolve_github_token(Some("  stored-token  ")).as_deref(),
            Some("stored-token")
        );
        assert_eq!(
            resolve_gitlab_token(Some("stored-token")).as_deref(),
            Some("stored-token")
        );
    }

    #[test]
    fn invalid_remote_references_fail_before_any_credential_lookup() {
        let credentials = RemoteCredentials {
            github_token: Some("stored-token".to_string()),
            gitlab_token: None,
        };
        let error = try_remote_ref_path_with(Path::new("."), "not-a-ref", &credentials)
            .expect_err("invalid reference must fail");
        assert!(error.contains("Invalid repository reference"), "{error}");
    }
}
