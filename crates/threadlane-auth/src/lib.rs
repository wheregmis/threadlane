pub mod antigravity_auth;
pub mod auth;
pub mod codex_auth;
pub mod openai_auth;
pub mod opencode_auth;
pub mod traits;

pub use antigravity_auth::*;
pub use codex_auth::*;
pub use openai_auth::*;
pub use opencode_auth::*;
pub use traits::AuthProvider;

use serde::de::DeserializeOwned;
use std::sync::Arc;

pub(crate) fn parse_oauth_response<T: DeserializeOwned>(body: &str) -> Result<T, String> {
    serde_json::from_str(body).map_err(|_| "OAuth provider returned an invalid response".into())
}

#[cfg(test)]
pub(crate) fn test_env_guard_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};

    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resolves a concrete `AuthProvider` instance by provider ID.
pub fn resolve_auth_provider(provider_id: &str) -> Option<Arc<dyn AuthProvider>> {
    match provider_id.to_lowercase().as_str() {
        "openai" => Some(Arc::new(OpenAiAuthProvider)),
        "codex" => Some(Arc::new(CodexAuthProvider)),
        "antigravity" | "google" => Some(Arc::new(AntigravityAuthProvider)),
        "opencode-go" | "opencode" => Some(Arc::new(OpencodeAuthProvider)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_auth_provider() {
        assert!(resolve_auth_provider("openai").is_some());
        assert_eq!(
            resolve_auth_provider("openai").unwrap().provider_id(),
            "openai"
        );
        assert!(resolve_auth_provider("codex").is_some());
        assert_eq!(
            resolve_auth_provider("codex").unwrap().provider_id(),
            "codex"
        );
        assert!(resolve_auth_provider("antigravity").is_some());
        assert_eq!(
            resolve_auth_provider("antigravity").unwrap().provider_id(),
            "antigravity"
        );
        assert!(resolve_auth_provider("google").is_some());
        assert_eq!(
            resolve_auth_provider("google").unwrap().provider_id(),
            "antigravity"
        );
        assert_eq!(
            resolve_auth_provider("opencode-go").unwrap().provider_id(),
            "opencode-go"
        );
        assert_eq!(
            resolve_auth_provider("opencode").unwrap().provider_id(),
            "opencode-go"
        );
        assert!(resolve_auth_provider("unknown").is_none());
    }

    #[test]
    fn oauth_parse_errors_do_not_include_response_bodies() {
        let secret_body = r#"{"access_token":"secret-token""#;

        let error = parse_oauth_response::<serde_json::Value>(secret_body).unwrap_err();

        assert!(!error.contains("secret-token"));
        assert!(!error.contains(secret_body));
    }
}
