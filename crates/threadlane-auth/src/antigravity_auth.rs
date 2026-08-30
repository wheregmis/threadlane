use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const DEFAULT_CLIENT_SECRET: &str = "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";
const DEFAULT_REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
const OAUTH_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64, // Unix timestamp in seconds
    pub account_email: Option<String>,
    pub project_id: Option<String>,
}

fn get_antigravity_credentials_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(home);
    path.push(".threadlane");
    let _ = fs::create_dir_all(&path);
    path.push("antigravity_credentials.json");
    path
}

pub fn load_antigravity_credentials() -> Option<AntigravityCredentials> {
    let path = get_antigravity_credentials_path();
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(creds) = serde_json::from_str::<AntigravityCredentials>(&content) {
                if !creds.access_token.is_empty() {
                    return Some(creds);
                }
            }
        }
    }
    None
}

fn save_antigravity_credentials(creds: &AntigravityCredentials) -> Result<(), String> {
    let path = get_antigravity_credentials_path();
    let json = serde_json::to_string_pretty(creds)
        .map_err(|_| "Failed to serialize credentials".to_string())?;
    crate::openai_auth::write_secure_text_file(&path, &json)
}

pub fn clear_antigravity_credentials() -> Result<(), String> {
    let path = get_antigravity_credentials_path();
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn generate_pkce_pair() -> (String, String) {
    let mut random_bytes = [0u8; 32];
    getrandom::fill(&mut random_bytes).expect("secure randomness should be available for PKCE");
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge_bytes = hasher.finalize();
    let challenge = URL_SAFE_NO_PAD.encode(challenge_bytes);

    (verifier, challenge)
}

/// Generates an unpredictable OAuth state value for CSRF protection.
pub fn generate_oauth_state() -> String {
    let mut random_bytes = [0u8; 32];
    getrandom::fill(&mut random_bytes)
        .expect("secure randomness should be available for OAuth state");
    URL_SAFE_NO_PAD.encode(random_bytes)
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn build_authorization_url(code_challenge: &str, state: &str) -> String {
    let scopes = [
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
        "https://www.googleapis.com/auth/cclog",
        "https://www.googleapis.com/auth/experimentsandconfigs",
    ]
    .join(" ");

    let client_id =
        std::env::var("ANTIGRAVITY_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string());

    let mut url = url::Url::parse(OAUTH_AUTH_URL).unwrap();
    url.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", DEFAULT_REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("scope", &scopes)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state);

    url.to_string()
}

pub async fn exchange_code_for_tokens(
    code: &str,
    code_verifier: &str,
) -> Result<AntigravityCredentials, String> {
    let creds = exchange_code_for_tokens_without_saving(code, code_verifier).await?;
    save_antigravity_credentials(&creds)?;
    Ok(creds)
}

async fn exchange_code_for_tokens_without_saving(
    code: &str,
    code_verifier: &str,
) -> Result<AntigravityCredentials, String> {
    let client_id =
        std::env::var("ANTIGRAVITY_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string());
    let client_secret = std::env::var("ANTIGRAVITY_CLIENT_SECRET")
        .unwrap_or_else(|_| DEFAULT_CLIENT_SECRET.to_string());

    let client = reqwest::Client::new();
    let mut params = vec![
        ("client_id", client_id.as_str()),
        ("code", code),
        ("code_verifier", code_verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", DEFAULT_REDIRECT_URI),
    ];
    if !client_secret.is_empty() {
        params.push(("client_secret", client_secret.as_str()));
    }

    let res = client
        .post(OAUTH_TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Failed to connect to Google OAuth token endpoint: {e}"))?;

    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| format!("Failed to read OAuth response body: {e}"))?;

    let val: serde_json::Value = match crate::parse_oauth_response(&body) {
        Ok(value) => value,
        Err(error) => return Err(format!("OAuth token exchange failed ({status}): {error}")),
    };

    if !status.is_success() {
        let reason = val.get("error").and_then(|value| value.as_str());
        return Err(match reason {
            Some(reason) => format!("OAuth token exchange failed ({status}): {reason}"),
            None => format!("OAuth token exchange failed ({status})"),
        });
    }

    let access_token = val
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing access_token in OAuth response".to_string())?
        .to_string();

    let refresh_token = val
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let expires_in = val
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);

    let expires_at = current_timestamp() + expires_in;

    // Fetch user info email
    let account_email = fetch_user_email(&client, &access_token).await.ok();

    let creds = AntigravityCredentials {
        access_token,
        refresh_token,
        expires_at,
        account_email,
        project_id: std::env::var("ANTIGRAVITY_PROJECT_ID").ok(),
    };

    Ok(creds)
}

async fn fetch_user_email(client: &reqwest::Client, access_token: &str) -> Result<String, String> {
    let res = client
        .get(USERINFO_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("Failed userinfo request: {e}"))?;

    if res.status().is_success() {
        let val: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        if let Some(email) = val.get("email").and_then(|v| v.as_str()) {
            return Ok(email.to_string());
        }
    }
    Err("Email not found".to_string())
}

async fn refresh_antigravity_token(
    creds: &AntigravityCredentials,
) -> Result<AntigravityCredentials, String> {
    let refresh_token = creds
        .refresh_token
        .as_ref()
        .ok_or_else(|| "No refresh token available".to_string())?;

    let client_id =
        std::env::var("ANTIGRAVITY_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string());
    let client_secret = std::env::var("ANTIGRAVITY_CLIENT_SECRET")
        .unwrap_or_else(|_| DEFAULT_CLIENT_SECRET.to_string());

    let client = reqwest::Client::new();
    let mut params = vec![
        ("client_id", client_id.as_str()),
        ("refresh_token", refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];
    if !client_secret.is_empty() {
        params.push(("client_secret", client_secret.as_str()));
    }

    let res = client
        .post(OAUTH_TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Failed token refresh request: {e}"))?;

    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| format!("Failed to read refresh response: {e}"))?;

    if !status.is_success() {
        return Err(format!("Token refresh failed ({status})"));
    }

    let val: serde_json::Value = crate::parse_oauth_response(&body)?;

    let new_access_token = val
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing access_token in refresh response".to_string())?
        .to_string();

    let expires_in = val
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);

    let expires_at = current_timestamp() + expires_in;

    let new_refresh = val
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| creds.refresh_token.clone());

    let updated_creds = AntigravityCredentials {
        access_token: new_access_token,
        refresh_token: new_refresh,
        expires_at,
        account_email: creds.account_email.clone(),
        project_id: creds.project_id.clone(),
    };

    save_antigravity_credentials(&updated_creds)?;
    Ok(updated_creds)
}

pub async fn get_valid_antigravity_token() -> Result<String, String> {
    let creds = load_antigravity_credentials().ok_or_else(|| {
        "No stored Google Antigravity credentials found. Please run /login antigravity".to_string()
    })?;

    let now = current_timestamp();
    // Refresh if within 5 minutes (300 seconds) of expiration
    if creds.expires_at <= now + 300 && creds.refresh_token.is_some() {
        let refreshed = refresh_antigravity_token(&creds).await?;
        return Ok(refreshed.access_token);
    }

    Ok(creds.access_token)
}

/// Helper function to listen locally for the OAuth callback code
pub async fn listen_for_oauth_callback(expected_state: String) -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:51121")
        .or_else(|_| TcpListener::bind("0.0.0.0:51121"))
        .map_err(|e| format!("Failed to bind loopback callback listener on port 51121: {e}"))?;

    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to set listener non-blocking: {e}"))?;

    let start_time = current_timestamp();
    loop {
        if current_timestamp() - start_time > 300 {
            return Err("OAuth callback timed out after 5 minutes".to_string());
        }

        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buffer = [0u8; 2048];
                if let Ok(bytes_read) = stream.read(&mut buffer) {
                    let request_str = String::from_utf8_lossy(&buffer[..bytes_read]);
                    if let Some(first_line) = request_str.lines().next() {
                        if first_line.starts_with("GET /oauth-callback") {
                            let path = first_line.split_whitespace().nth(1).unwrap_or("");
                            if let Ok(parsed_url) =
                                url::Url::parse(&format!("http://localhost:51121{path}"))
                            {
                                let mut code = None;
                                let mut state = None;
                                let mut oauth_error = None;
                                for (k, v) in parsed_url.query_pairs() {
                                    if k == "code" {
                                        code = Some(v.to_string());
                                    } else if k == "state" {
                                        state = Some(v.to_string());
                                    } else if k == "error" {
                                        oauth_error = Some(v.to_string());
                                    }
                                }

                                let (res_code, html_response) = match (state, code, oauth_error) {
                                    (Some(st), Some(code), None) if st == expected_state => (
                                        Ok(code),
                                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!DOCTYPE html><html><body style='font-family:sans-serif;background:#0d1117;color:#58a6ff;padding:40px;text-align:center;'><h2>Google Antigravity Authentication Successful!</h2><p>You may now close this tab and return to Threadlane.</p></body></html>",
                                    ),
                                    (Some(st), _, _) if st != expected_state => (
                                        Err("OAuth state mismatch".to_string()),
                                        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h2>Authentication Error</h2><p>State mismatch.</p></body></html>",
                                    ),
                                    (_, _, Some(error)) => (
                                        Err(format!("Google OAuth callback failed: {error}")),
                                        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h2>Authentication Error</h2><p>Google sign-in was cancelled or denied.</p></body></html>",
                                    ),
                                    _ => (
                                        Err("Missing code or state in OAuth callback".to_string()),
                                        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h2>Authentication Error</h2><p>Missing parameters.</p></body></html>",
                                    ),
                                };

                                let _ = stream.write_all(html_response.as_bytes());
                                let _ = stream.flush();
                                return res_code;
                            }
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            }
            Err(e) => {
                return Err(format!("Error accepting callback connection: {e}"));
            }
        }
    }
}

pub struct AntigravityAuthProvider;

#[async_trait::async_trait]
impl crate::traits::AuthProvider for AntigravityAuthProvider {
    fn provider_id(&self) -> &'static str {
        "antigravity"
    }

    fn has_credentials(&self) -> bool {
        load_antigravity_credentials().is_some()
    }

    async fn get_token(&self) -> Result<String, String> {
        get_valid_antigravity_token().await
    }

    fn clear_credentials(&self) -> Result<(), String> {
        clear_antigravity_credentials()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::AuthProvider;
    use std::ffi::OsString;

    struct TestHomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous_home: Option<OsString>,
        home: PathBuf,
    }

    impl TestHomeGuard {
        fn new() -> Self {
            let lock = crate::test_env_guard_lock();
            let previous_home = std::env::var_os("HOME");
            let home = std::env::temp_dir().join(format!(
                "threadlane-antigravity-auth-{}-{}",
                std::process::id(),
                current_timestamp()
            ));
            fs::create_dir_all(&home).unwrap();
            std::env::set_var("HOME", &home);
            Self {
                _lock: lock,
                previous_home,
                home,
            }
        }
    }

    impl Drop for TestHomeGuard {
        fn drop(&mut self) {
            match self.previous_home.take() {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn test_build_authorization_url() {
        let code_challenge = "test_challenge_123";
        let state = "test_state_456";
        let url_str = build_authorization_url(code_challenge, state);

        let url = url::Url::parse(&url_str).unwrap();

        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        assert_eq!(url.path(), "/o/oauth2/v2/auth");

        let query_params: std::collections::HashMap<_, _> =
            url.query_pairs().into_owned().collect();

        assert_eq!(query_params.get("response_type").unwrap(), "code");
        assert_eq!(query_params.get("code_challenge").unwrap(), code_challenge);
        assert_eq!(query_params.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(query_params.get("access_type").unwrap(), "offline");
        assert_eq!(query_params.get("prompt").unwrap(), "consent");
        assert_eq!(query_params.get("state").unwrap(), state);
        assert_eq!(
            query_params.get("redirect_uri").unwrap(),
            DEFAULT_REDIRECT_URI
        );

        let scopes = query_params.get("scope").unwrap();
        assert!(scopes.contains("https://www.googleapis.com/auth/cloud-platform"));
        assert!(scopes.contains("https://www.googleapis.com/auth/userinfo.email"));
        assert!(scopes.contains("https://www.googleapis.com/auth/userinfo.profile"));
        assert!(scopes.contains("https://www.googleapis.com/auth/cclog"));
        assert!(scopes.contains("https://www.googleapis.com/auth/experimentsandconfigs"));

        if let Ok(client_id) = std::env::var("ANTIGRAVITY_CLIENT_ID") {
            assert_eq!(query_params.get("client_id").unwrap(), &client_id);
        } else {
            assert_eq!(query_params.get("client_id").unwrap(), DEFAULT_CLIENT_ID);
        }
    }

    #[test]
    fn test_antigravity_provider_id() {
        let provider = AntigravityAuthProvider;
        assert_eq!(provider.provider_id(), "antigravity");
    }

    #[test]
    fn test_generate_oauth_state() {
        let state = generate_oauth_state();
        assert_eq!(
            state.len(),
            43,
            "OAuth state should contain 32 random bytes"
        );
        assert!(
            state
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "OAuth state should be URL-safe"
        );
        assert_ne!(
            state,
            generate_oauth_state(),
            "OAuth states should be unique"
        );
    }

    #[test]
    fn test_generate_pkce_pair() {
        let (verifier, challenge) = generate_pkce_pair();

        // 32 bytes encoded in unpadded base64url is exactly 43 characters
        assert_eq!(verifier.len(), 43, "Verifier should be 43 characters long");
        assert_eq!(
            challenge.len(),
            43,
            "Challenge should be 43 characters long"
        );

        // Check character set for URL-safe base64
        for c in verifier.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "Invalid char in verifier"
            );
        }

        // Verify the SHA256 relationship
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected_challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(
            challenge, expected_challenge,
            "Challenge must be SHA256 of verifier"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_save_antigravity_credentials_sets_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let env = TestHomeGuard::new();
        save_antigravity_credentials(&AntigravityCredentials {
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            expires_at: current_timestamp() + 3600,
            account_email: None,
            project_id: None,
        })
        .unwrap();

        let path = env
            .home
            .join(".threadlane")
            .join("antigravity_credentials.json");
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
