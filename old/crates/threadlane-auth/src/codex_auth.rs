use crate::openai_auth::{load_credentials, remove_credentials, StoredCredentials};
use crate::traits::AuthProvider;

pub struct CodexAuthProvider;

impl CodexAuthProvider {
    pub fn load_codex_credentials() -> Option<StoredCredentials> {
        load_credentials()
    }
}

#[async_trait::async_trait]
impl AuthProvider for CodexAuthProvider {
    fn provider_id(&self) -> &'static str {
        "codex"
    }

    fn has_credentials(&self) -> bool {
        load_credentials().is_some()
    }

    async fn get_token(&self) -> Result<String, String> {
        load_credentials()
            .map(|creds| creds.access_token)
            .ok_or_else(|| "No stored Codex or OpenAI credentials found.".to_string())
    }

    fn clear_credentials(&self) -> Result<(), String> {
        remove_credentials()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codex_provider_id() {
        let codex = CodexAuthProvider;
        assert_eq!(codex.provider_id(), "codex");
    }
}
