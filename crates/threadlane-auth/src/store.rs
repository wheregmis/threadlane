//! Injectable credential storage locations.
//!
//! Every auth module historically resolved its own files from `$HOME` (or
//! `%USERPROFILE%`) plus a hardcoded `.threadlane` directory name. That ties
//! the crate to one application's layout and makes isolated testing depend on
//! swapping process environment variables.
//!
//! [`CredentialStore`] carries those locations explicitly. The default matches
//! the historical behavior exactly; hosts with their own layout (or tests that
//! want an isolated directory) construct one with [`CredentialStore::new`]
//! and call the `*_in` variants each module exposes. The existing zero-argument
//! functions remain as thin wrappers over the default so current callers keep
//! working unchanged.

use std::path::{Path, PathBuf};

/// Where credential files live.
///
/// `threadlane_dir` is the application settings directory (historically
/// `$HOME/.threadlane`); `home_dir` is the user's home, used only for
/// third-party fallbacks such as `~/.codex/auth.json` that live outside the
/// application directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStore {
    threadlane_dir: PathBuf,
    home_dir: PathBuf,
}

impl CredentialStore {
    /// Uses explicit directories without creating anything.
    pub fn new(threadlane_dir: PathBuf, home_dir: PathBuf) -> Self {
        Self {
            threadlane_dir,
            home_dir,
        }
    }

    /// Points at an isolated `threadlane_dir` under `root` with `root` itself
    /// as the home dir. Intended for tests; creates nothing.
    pub fn isolated(root: PathBuf) -> Self {
        Self {
            threadlane_dir: root.join(".threadlane"),
            home_dir: root,
        }
    }

    /// The application settings directory, creating it like the historical
    /// helpers did.
    pub fn ensure_threadlane_dir(&self) -> PathBuf {
        let _ = std::fs::create_dir_all(&self.threadlane_dir);
        self.threadlane_dir.clone()
    }

    pub fn threadlane_dir(&self) -> &Path {
        &self.threadlane_dir
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    fn file(&self, name: &str) -> PathBuf {
        self.threadlane_dir.join(name)
    }

    /// `credentials.json`: the multi-account Codex/ChatGPT store.
    pub fn credentials_path(&self) -> PathBuf {
        self.file("credentials.json")
    }

    /// `openai_api_key`: the plain OpenAI API key file.
    pub fn openai_api_key_path(&self) -> PathBuf {
        self.file("openai_api_key")
    }

    /// `opencode_api_key`: the plain OpenCode API key file.
    pub fn opencode_api_key_path(&self) -> PathBuf {
        self.file("opencode_api_key")
    }

    /// `antigravity_credentials.json`: Google OAuth tokens for Antigravity.
    pub fn antigravity_credentials_path(&self) -> PathBuf {
        self.file("antigravity_credentials.json")
    }

    /// `github_credentials.json`: stored GitHub token.
    pub fn github_credentials_path(&self) -> PathBuf {
        self.file("github_credentials.json")
    }

    /// `gitlab_credentials.json`: stored GitLab token.
    pub fn gitlab_credentials_path(&self) -> PathBuf {
        self.file("gitlab_credentials.json")
    }

    /// Third-party Codex CLI fallback (`~/.codex/auth.json`), read-only.
    pub fn codex_cli_auth_path(&self) -> PathBuf {
        self.home_dir.join(".codex").join("auth.json")
    }
}

fn env_home() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string())
}

impl Default for CredentialStore {
    /// Matches the historical resolution exactly: `$HOME/.threadlane`
    /// (created on demand) plus `$HOME` for third-party fallbacks.
    fn default() -> Self {
        let store = Self {
            threadlane_dir: PathBuf::from(env_home()).join(".threadlane"),
            home_dir: PathBuf::from(env_home()),
        };
        store.ensure_threadlane_dir();
        store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_store_keeps_everything_under_root() {
        let root = PathBuf::from("/tmp/threadlane-store-test");
        let store = CredentialStore::isolated(root.clone());
        assert_eq!(store.threadlane_dir(), root.join(".threadlane"));
        assert_eq!(store.home_dir(), root.as_path());
        assert_eq!(
            store.credentials_path(),
            root.join(".threadlane").join("credentials.json")
        );
        assert_eq!(
            store.codex_cli_auth_path(),
            root.join(".codex").join("auth.json")
        );
    }

    #[test]
    fn injected_store_isolates_credential_round_trips() {
        let _guard = crate::test_env_guard_lock();
        let root = std::env::temp_dir().join(format!(
            "threadlane-auth-store-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let locations = CredentialStore::isolated(root.clone());

        crate::openai_auth::save_openai_api_key_in("sk-test-key", &locations).unwrap();
        assert_eq!(
            crate::openai_auth::load_openai_api_key_in(&locations).as_deref(),
            Some("sk-test-key")
        );

        crate::github_auth::save_github_token_in("ghp-x", None, "token", &locations).unwrap();
        assert!(locations.github_credentials_path().exists());

        crate::opencode_auth::save_opencode_api_key_in("opencode-key", &locations).unwrap();
        assert_eq!(
            crate::opencode_auth::load_opencode_api_key_in(&locations).as_deref(),
            Some("opencode-key")
        );

        // Nothing leaked into the real default locations.
        assert!(!root.join(".codex").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
