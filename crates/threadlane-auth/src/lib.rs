pub mod antigravity_auth;
pub mod auth;
pub mod codex_auth;
pub mod github_auth;
pub mod openai_auth;
pub mod opencode_auth;
pub mod store;
pub mod traits;

pub use antigravity_auth::*;
pub use codex_auth::*;
pub use github_auth::*;
pub use openai_auth::*;
pub use opencode_auth::*;
pub use store::CredentialStore;

use serde::de::DeserializeOwned;

fn parse_oauth_response<T: DeserializeOwned>(body: &str) -> Result<T, String> {
    serde_json::from_str(body).map_err(|_| "OAuth provider returned an invalid response".into())
}

#[cfg(test)]
fn test_env_guard_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};

    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_parse_errors_do_not_include_response_bodies() {
        let secret_body = r#"{"access_token":"secret-token""#;

        let error = parse_oauth_response::<serde_json::Value>(secret_body).unwrap_err();

        assert!(!error.contains("secret-token"));
        assert!(!error.contains(secret_body));
    }
}
