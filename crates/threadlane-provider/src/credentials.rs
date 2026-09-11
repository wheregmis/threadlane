//! Host-injected credential resolution.
//!
//! The provider layer translates requests and parses responses; it must not
//! own credential storage. Hosts implement [`CodexAccountResolver`] and
//! [`AntigravityCredentialSource`] against their own stores (in Threadlane,
//! `threadlane-session` bridges these to `threadlane-auth`) and hand them to
//! [`ProviderClient`](crate::router::ProviderClient) builders. Callers that
//! need no stored credentials use the `Noop` implementations, which resolve
//! from explicit arguments and ambient environment only.

use std::sync::Arc;

/// Minimal Codex account identity for provider fallback construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexBackupAccount {
    pub access_token: String,
    pub account_id: Option<String>,
}

/// Resolves Codex account identity without owning credential storage.
#[async_trait::async_trait]
pub trait CodexAccountResolver: Send + Sync + std::fmt::Debug {
    /// Finds the owning account id for a token, if the host knows it.
    fn account_id_for_token(&self, token: &str) -> Option<String>;

    /// Returns a usable (refreshed if needed) token for an account id.
    async fn valid_token_for_account(&self, account_id: &str) -> Result<String, String>;

    /// Non-active accounts eligible as request fallbacks. Defaults to none.
    fn backup_accounts(&self) -> Vec<CodexBackupAccount> {
        Vec::new()
    }
}

/// Resolver with no stored credentials: tokens are used as-is.
#[derive(Debug, Default)]
pub struct NoopCodexResolver;

#[async_trait::async_trait]
impl CodexAccountResolver for NoopCodexResolver {
    fn account_id_for_token(&self, _token: &str) -> Option<String> {
        None
    }

    async fn valid_token_for_account(&self, _account_id: &str) -> Result<String, String> {
        Err("No stored Codex credentials".to_string())
    }
}

/// Point-in-time Antigravity credential snapshot for diagnostics and project
/// resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AntigravityCredentialSnapshot {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
    pub account_email: Option<String>,
    pub project_id: Option<String>,
}

/// Supplies Antigravity OAuth credentials without owning their storage.
#[async_trait::async_trait]
pub trait AntigravityCredentialSource: Send + Sync + std::fmt::Debug {
    /// Returns a usable (refreshed if needed) access token.
    async fn valid_token(&self) -> Result<String, String>;

    /// Stored credential snapshot for diagnostics, if any.
    fn stored_snapshot(&self) -> Option<AntigravityCredentialSnapshot>;
}

/// Credential source with nothing stored: every request fails fast.
#[derive(Debug, Default)]
pub struct NoopAntigravityCredentials;

#[async_trait::async_trait]
impl AntigravityCredentialSource for NoopAntigravityCredentials {
    async fn valid_token(&self) -> Result<String, String> {
        Err("No stored Google Antigravity credentials found. Please run /login antigravity"
            .to_string())
    }

    fn stored_snapshot(&self) -> Option<AntigravityCredentialSnapshot> {
        None
    }
}

/// Shared handle type used by provider clients.
pub type SharedCodexResolver = Arc<dyn CodexAccountResolver>;
/// Shared handle type used by provider clients.
pub type SharedAntigravityCredentials = Arc<dyn AntigravityCredentialSource>;
