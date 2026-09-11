use crate::store::CredentialStore;
use crate::traits::AuthProvider;
use std::fs;

pub fn save_opencode_api_key(key: &str) -> Result<(), String> {
    save_opencode_api_key_in(key, &CredentialStore::default())
}

/// Saves the API key file at the injected store's location.
pub fn save_opencode_api_key_in(key: &str, locations: &CredentialStore) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("OpenCode API key cannot be empty".to_string());
    }

    crate::openai_auth::write_secure_text_file(&locations.opencode_api_key_path(), key)
}

pub fn load_opencode_api_key() -> Option<String> {
    load_opencode_api_key_in(&CredentialStore::default())
}

/// Loads the API key file from the injected store's location (env fallback
/// preserved: `OPENCODE_API_KEY`, then `OPENCODE_GO_API_KEY`).
pub fn load_opencode_api_key_in(locations: &CredentialStore) -> Option<String> {
    let path = locations.opencode_api_key_path();
    if path.exists() {
        if let Ok(content) = fs::read_to_string(path) {
            let key = content.trim().to_string();
            if !key.is_empty() {
                return Some(key);
            }
        }
    }

    for variable in ["OPENCODE_API_KEY", "OPENCODE_GO_API_KEY"] {
        if let Ok(key) = std::env::var(variable) {
            let key = key.trim().to_string();
            if !key.is_empty() {
                return Some(key);
            }
        }
    }

    None
}

pub fn clear_opencode_api_key() -> Result<(), String> {
    clear_opencode_api_key_in(&CredentialStore::default())
}

/// Removes the API key file at the injected store's location.
pub fn clear_opencode_api_key_in(locations: &CredentialStore) -> Result<(), String> {
    let path = locations.opencode_api_key_path();
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub struct OpencodeAuthProvider;

#[async_trait::async_trait]
impl AuthProvider for OpencodeAuthProvider {
    fn provider_id(&self) -> &'static str {
        "opencode-go"
    }

    fn has_credentials(&self) -> bool {
        load_opencode_api_key().is_some()
    }

    async fn get_token(&self) -> Result<String, String> {
        load_opencode_api_key()
            .ok_or_else(|| "No stored OpenCode API key found. Please enter key in settings or set OPENCODE_API_KEY.".to_string())
    }

    fn clear_credentials(&self) -> Result<(), String> {
        clear_opencode_api_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opencode_auth_provider_id() {
        let provider = OpencodeAuthProvider;
        assert_eq!(provider.provider_id(), "opencode-go");
    }

    #[test]
    fn test_save_and_load_opencode_api_key_round_trip() {
        let _guard = crate::test_env_guard_lock();
        let temp_dir =
            std::env::temp_dir().join(format!("test-opencode-auth-{}", std::process::id()));
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &temp_dir);

        let result = save_opencode_api_key("opencode-key-123");
        assert!(result.is_ok());
        assert_eq!(
            load_opencode_api_key(),
            Some("opencode-key-123".to_string())
        );

        let clear_res = clear_opencode_api_key();
        assert!(clear_res.is_ok());
        assert_eq!(load_opencode_api_key(), None);

        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(temp_dir);
    }
}
