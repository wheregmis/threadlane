use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubCredentials {
    pub token: String,
    pub username: Option<String>,
    auth_type: String, // "cli", "token", "oauth"
    updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitLabCredentials {
    token: String,
    username: Option<String>,
    host: Option<String>,
    updated_at: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn get_threadlane_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(home);
    path.push(".threadlane");
    let _ = fs::create_dir_all(&path);
    path
}

fn get_github_credentials_path() -> PathBuf {
    let mut path = get_threadlane_dir();
    path.push("github_credentials.json");
    path
}

fn get_gitlab_credentials_path() -> PathBuf {
    let mut path = get_threadlane_dir();
    path.push("gitlab_credentials.json");
    path
}

fn write_secure_file(path: &PathBuf, content: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("Failed to create credentials file: {e}"))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("Failed to write credentials: {e}"))?;
    }

    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|e| format!("Failed to create credentials file: {e}"))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("Failed to write credentials: {e}"))?;
    }

    Ok(())
}

// ── GitHub ─────────────────────────────────────────────────────────────

pub fn load_github_credentials() -> Option<GitHubCredentials> {
    let path = get_github_credentials_path();
    if !path.exists() {
        return None;
    }
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn save_github_token(
    token: &str,
    username: Option<&str>,
    auth_type: &str,
) -> Result<GitHubCredentials, String> {
    let creds = GitHubCredentials {
        token: token.trim().to_string(),
        username: username.map(|s| s.trim().to_string()),
        auth_type: auth_type.to_string(),
        updated_at: now_secs(),
    };
    let json = serde_json::to_string_pretty(&creds)
        .map_err(|e| format!("Failed to serialize GitHub credentials: {e}"))?;
    write_secure_file(&get_github_credentials_path(), &json)?;
    Ok(creds)
}

pub fn remove_github_credentials() -> Result<(), String> {
    let path = get_github_credentials_path();
    if path.exists() {
        fs::remove_file(path).map_err(|e| format!("Failed to remove GitHub credentials: {e}"))?;
    }
    Ok(())
}

pub fn get_github_token() -> Option<String> {
    // 1. Check stored credentials
    if let Some(creds) = load_github_credentials() {
        if !creds.token.trim().is_empty() {
            return Some(creds.token);
        }
    }

    // 2. Check gh CLI
    if let Ok(output) = Command::new("gh").args(["auth", "token"]).output() {
        if output.status.success() {
            let tok = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !tok.is_empty() {
                return Some(tok);
            }
        }
    }

    // 3. Check environment variables
    std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()
        .filter(|t| !t.trim().is_empty())
}

pub fn get_github_auth_status() -> Option<String> {
    if let Some(creds) = load_github_credentials() {
        if let Some(user) = creds.username {
            return Some(format!("@{user} ({})", creds.auth_type));
        }
        return Some(format!("Token configured ({})", creds.auth_type));
    }

    // Check gh CLI
    if let Ok(output) = Command::new("gh").args(["auth", "status"]).output() {
        if output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let combined = format!("{stdout}\n{stderr}");
            // Parse "Logged in to github.com account <username>"
            if let Some(line) = combined.lines().find(|l| l.contains("account ")) {
                if let Some(user) = line.split("account ").nth(1) {
                    let user_clean = user.split_whitespace().next().unwrap_or(user);
                    return Some(format!("@{user_clean} (via gh CLI)"));
                }
            }
            return Some("Connected (via gh CLI)".to_string());
        }
    }

    if std::env::var("GITHUB_TOKEN").is_ok() || std::env::var("GH_TOKEN").is_ok() {
        return Some("Environment Token ($GITHUB_TOKEN)".to_string());
    }

    None
}

pub fn sync_from_gh_cli() -> Result<GitHubCredentials, String> {
    let token_output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|e| format!("GitHub CLI ('gh') is not installed or not in PATH: {e}"))?;

    if !token_output.status.success() {
        return Err(
            "GitHub CLI is not authenticated. Run 'gh auth login' in your terminal first."
                .to_string(),
        );
    }

    let token = String::from_utf8_lossy(&token_output.stdout)
        .trim()
        .to_string();
    if token.is_empty() {
        return Err("No token returned by 'gh auth token'.".to_string());
    }

    // Try fetching username
    let user_output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output();

    let username = if let Ok(out) = user_output {
        if out.status.success() {
            let u = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !u.is_empty() {
                Some(u)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    save_github_token(&token, username.as_deref(), "cli")
}

// ── GitLab ─────────────────────────────────────────────────────────────

fn load_gitlab_credentials() -> Option<GitLabCredentials> {
    let path = get_gitlab_credentials_path();
    if !path.exists() {
        return None;
    }
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn save_gitlab_token(
    token: &str,
    username: Option<&str>,
    host: Option<&str>,
) -> Result<GitLabCredentials, String> {
    let creds = GitLabCredentials {
        token: token.trim().to_string(),
        username: username.map(|s| s.trim().to_string()),
        host: host.map(|h| h.trim().to_string()),
        updated_at: now_secs(),
    };
    let json = serde_json::to_string_pretty(&creds)
        .map_err(|e| format!("Failed to serialize GitLab credentials: {e}"))?;
    write_secure_file(&get_gitlab_credentials_path(), &json)?;
    Ok(creds)
}

pub fn remove_gitlab_credentials() -> Result<(), String> {
    let path = get_gitlab_credentials_path();
    if path.exists() {
        fs::remove_file(path).map_err(|e| format!("Failed to remove GitLab credentials: {e}"))?;
    }
    Ok(())
}

pub fn get_gitlab_token() -> Option<String> {
    if let Some(creds) = load_gitlab_credentials() {
        if !creds.token.trim().is_empty() {
            return Some(creds.token);
        }
    }

    std::env::var("GITLAB_TOKEN")
        .or_else(|_| std::env::var("GL_TOKEN"))
        .ok()
        .filter(|t| !t.trim().is_empty())
}

pub fn get_gitlab_auth_status() -> Option<String> {
    if let Some(creds) = load_gitlab_credentials() {
        let host_str = creds.host.as_deref().unwrap_or("gitlab.com");
        if let Some(user) = creds.username {
            return Some(format!("@{user} ({host_str})"));
        }
        return Some(format!("Token configured ({host_str})"));
    }

    if std::env::var("GITLAB_TOKEN").is_ok() || std::env::var("GL_TOKEN").is_ok() {
        return Some("Environment Token ($GITLAB_TOKEN)".to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_github_credentials_serde() {
        let creds = GitHubCredentials {
            token: "ghp_test12345".to_string(),
            username: Some("octocat".to_string()),
            auth_type: "token".to_string(),
            updated_at: 1234567890,
        };

        let json = serde_json::to_string(&creds).unwrap();
        let deserialized: GitHubCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(creds, deserialized);
    }

    #[test]
    fn test_gitlab_credentials_serde() {
        let creds = GitLabCredentials {
            token: "glpat-test12345".to_string(),
            username: Some("tanuki".to_string()),
            host: Some("gitlab.example.com".to_string()),
            updated_at: 1234567890,
        };

        let json = serde_json::to_string(&creds).unwrap();
        let deserialized: GitLabCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(creds, deserialized);
    }
}
