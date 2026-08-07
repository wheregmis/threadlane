use crate::traits::AuthProvider;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

const CLIENT_ID: &str = "app-8Nl2J3k7mP0xQ1vR";

fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringOrNumberVisitor;

    impl<'de> de::Visitor<'de> for StringOrNumberVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a number or string representing a number")
        }

        fn visit_u64<E>(self, value: u64) -> Result<u64, E>
        where
            E: de::Error,
        {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<u64, E>
        where
            E: de::Error,
        {
            if value >= 0 {
                Ok(value as u64)
            } else {
                Err(de::Error::custom("expected unsigned integer"))
            }
        }

        fn visit_str<E>(self, value: &str) -> Result<u64, E>
        where
            E: de::Error,
        {
            value.parse::<u64>().map_err(de::Error::custom)
        }
    }

    deserializer.deserialize_any(StringOrNumberVisitor)
}

fn default_verification_uri() -> String {
    "https://auth.openai.com/codex/device".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_auth_id: String,
    pub user_code: String,
    #[serde(default = "default_verification_uri")]
    pub verification_uri: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(
        deserialize_with = "deserialize_string_or_number",
        default = "default_interval"
    )]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
    pub source: String,
}

fn get_threadlane_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(home);
    path.push(".threadlane");
    let _ = fs::create_dir_all(&path);
    path
}

pub fn get_credentials_path() -> PathBuf {
    let mut path = get_threadlane_dir();
    path.push("credentials.json");
    path
}

pub fn save_credentials(tokens: &OAuthTokens) -> Result<(), String> {
    let path = get_credentials_path();
    let creds = StoredCredentials {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        account_id: tokens.account_id.clone(),
        source: "~/.threadlane/credentials.json".to_string(),
    };
    let json = serde_json::to_string_pretty(&creds)
        .map_err(|_| "Failed to serialize credentials".to_string())?;
    write_secure_text_file(&path, &json)
}

pub fn is_own_source(source: &str) -> bool {
    source == "~/.threadlane/credentials.json"
}

pub fn remove_credentials() -> Result<(), String> {
    let path = get_credentials_path();
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn get_openai_api_key_path() -> PathBuf {
    let mut path = get_threadlane_dir();
    path.push("openai_api_key");
    path
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, destination: &Path) -> std::io::Result<()> {
    let temp: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();

    if unsafe {
        MoveFileExW(
            temp.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temp_path, destination)
}

pub(crate) fn write_secure_text_file(path: &Path, contents: &str) -> Result<(), String> {
    write_secure_text_file_with_replacer(path, contents, replace_file)
}

fn write_secure_text_file_with_replacer(
    path: &Path,
    contents: &str,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Failed to store credentials".to_string())?;
    let tmp_path = parent.join(format!(
        ".credentials.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Failed to store credentials".to_string())?
            .as_nanos()
    ));

    let mut options = OpenOptions::new();
    options.create_new(true).truncate(true).write(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options
        .open(&tmp_path)
        .map_err(|e| format!("Failed to store credentials: {e}"))?;

    let write_result = file
        .write_all(contents.as_bytes())
        .and_then(|_| file.sync_all());
    drop(file);

    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("Failed to store credentials: {error}"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Err(error) = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("Failed to store credentials: {error}"));
        }
    }

    if let Err(error) = replace(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("Failed to store credentials: {error}"));
    }

    Ok(())
}

pub fn save_openai_api_key(key: &str) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("OpenAI API key cannot be empty".to_string());
    }

    write_secure_text_file(&get_openai_api_key_path(), key)
}

pub fn load_openai_api_key() -> Option<String> {
    let path = get_openai_api_key_path();
    let key = fs::read_to_string(path).ok()?;
    let key = key.trim().to_string();
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

pub fn load_credentials() -> Option<StoredCredentials> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());

    // 1. Try ~/.threadlane/credentials.json
    let threadlane_path = get_credentials_path();
    if threadlane_path.exists() {
        if let Ok(content) = fs::read_to_string(&threadlane_path) {
            if let Ok(creds) = serde_json::from_str::<StoredCredentials>(&content) {
                if !creds.access_token.is_empty() {
                    return Some(creds);
                }
            }
        }
    }

    // 2. Try ~/.codex/auth.json
    let codex_path = PathBuf::from(&home).join(".codex").join("auth.json");
    if codex_path.exists() {
        if let Ok(content) = fs::read_to_string(&codex_path) {
            if let Ok(val) = serde_json::from_str::<Value>(&content) {
                if let Some(tokens) = val.get("tokens") {
                    if let Some(token) = tokens.get("access_token").and_then(|v| v.as_str()) {
                        let account_id = tokens
                            .get("account_id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());

                        return Some(StoredCredentials {
                            access_token: token.to_string(),
                            refresh_token: tokens
                                .get("refresh_token")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            account_id,
                            source: "~/.codex/auth.json".to_string(),
                        });
                    }
                }
                if let Some(key) = val.get("OPENAI_API_KEY").and_then(|v| v.as_str()) {
                    if !key.is_empty() {
                        return Some(StoredCredentials {
                            access_token: key.to_string(),
                            refresh_token: None,
                            account_id: None,
                            source: "~/.codex/auth.json".to_string(),
                        });
                    }
                }
            }
        }
    }

    None
}

pub async fn start_device_login() -> Result<DeviceCodeResponse, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/api/accounts/deviceauth/usercode")
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|e| format!("Failed to initiate ChatGPT device login: {e}"))?;

    if !res.status().is_success() {
        let status = res.status();
        return Err(format!("Device login initiation failed ({status})"));
    }

    let text = res
        .text()
        .await
        .map_err(|e| format!("Failed to read device code body: {e}"))?;

    crate::parse_oauth_response(&text)
}

pub async fn poll_device_token(
    device_auth_id: &str,
    user_code: &str,
) -> Result<OAuthTokens, String> {
    let tokens = poll_device_token_without_saving(device_auth_id, user_code).await?;
    let _ = save_credentials(&tokens);
    Ok(tokens)
}

pub async fn poll_device_token_without_saving(
    device_auth_id: &str,
    user_code: &str,
) -> Result<OAuthTokens, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/api/accounts/deviceauth/token")
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
            "device_auth_id": device_auth_id,
            "user_code": user_code
        }))
        .send()
        .await
        .map_err(|e| format!("Error polling device token: {e}"))?;

    let body = res.text().await.unwrap_or_default();

    if body.contains("deviceauth_authorization_pending") || body.contains("authorization_pending") {
        return Err("authorization_pending".to_string());
    }

    let val: Value = crate::parse_oauth_response(&body)?;

    if let Some(access_token) = val.get("access_token").and_then(|v| v.as_str()) {
        let tokens = OAuthTokens {
            access_token: access_token.to_string(),
            refresh_token: val
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            expires_in: val.get("expires_in").and_then(|v| v.as_u64()),
            id_token: val
                .get("id_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            account_id: val
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        };
        return Ok(tokens);
    }

    let code_opt = val
        .get("authorization_code")
        .or_else(|| val.get("code"))
        .and_then(|v| v.as_str());

    if let Some(code) = code_opt {
        return exchange_authorization_code_without_saving(code).await;
    }

    Err("Unexpected OAuth token response".into())
}

async fn exchange_authorization_code_without_saving(code: &str) -> Result<OAuthTokens, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/oauth/token")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "redirect_uri": "https://auth.openai.com/device"
        }))
        .send()
        .await
        .map_err(|e| format!("Error exchanging code for OAuth token: {e}"))?;

    let body = res.text().await.unwrap_or_default();
    let val: Value = crate::parse_oauth_response(&body)?;

    if let Some(access_token) = val.get("access_token").and_then(|v| v.as_str()) {
        let tokens = OAuthTokens {
            access_token: access_token.to_string(),
            refresh_token: val
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            expires_in: val.get("expires_in").and_then(|v| v.as_u64()),
            id_token: val
                .get("id_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            account_id: val
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        };
        return Ok(tokens);
    }

    Err("Code exchange failed".into())
}

pub struct OpenAiAuthProvider;

#[async_trait::async_trait]
impl AuthProvider for OpenAiAuthProvider {
    fn provider_id(&self) -> &'static str {
        "openai"
    }

    fn has_credentials(&self) -> bool {
        load_credentials().is_some_and(|creds| is_own_source(&creds.source))
    }

    async fn get_token(&self) -> Result<String, String> {
        load_credentials()
            .filter(|creds| is_own_source(&creds.source))
            .map(|creds| creds.access_token)
            .ok_or_else(|| {
                "No stored OpenAI credentials found. Please run /login openai".to_string()
            })
    }

    fn clear_credentials(&self) -> Result<(), String> {
        remove_credentials()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    fn temp_home(name: &str) -> PathBuf {
        let mut home = std::env::temp_dir();
        home.push(format!(
            "threadlane-auth-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&home).unwrap();
        home
    }

    struct TestHomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous_home: Option<OsString>,
        home: PathBuf,
    }

    impl TestHomeGuard {
        fn new(name: &str) -> Self {
            let lock = crate::test_env_guard_lock();
            let previous_home = std::env::var_os("HOME");
            let home = temp_home(name);
            std::env::set_var("HOME", &home);
            Self {
                _lock: lock,
                previous_home,
                home,
            }
        }

        fn home(&self) -> &PathBuf {
            &self.home
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
    fn test_parse_device_code_response() {
        let sample_json = r#"{
            "device_auth_id": "deviceauth_123",
            "user_code": "JLHW-OEIT1",
            "interval": "5",
            "expires_at": "2026-07-21T20:56:56+00:00"
        }"#;

        let resp: DeviceCodeResponse = serde_json::from_str(sample_json).unwrap();
        assert_eq!(resp.user_code, "JLHW-OEIT1");
        assert_eq!(resp.interval, 5);
        assert_eq!(
            resp.verification_uri,
            "https://auth.openai.com/codex/device"
        );
    }

    #[test]
    fn test_openai_provider_id() {
        let openai = OpenAiAuthProvider;
        assert_eq!(openai.provider_id(), "openai");
    }

    #[test]
    fn test_save_and_load_openai_api_key_round_trip() {
        let env = TestHomeGuard::new("round-trip");

        save_openai_api_key("sk-test-123").unwrap();

        assert_eq!(load_openai_api_key().as_deref(), Some("sk-test-123"));
        assert!(env
            .home()
            .join(".threadlane")
            .join("openai_api_key")
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_save_codex_credentials_sets_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let env = TestHomeGuard::new("codex-permissions");
        save_credentials(&OAuthTokens {
            access_token: "codex-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            expires_in: None,
            id_token: None,
            account_id: Some("account".into()),
        })
        .unwrap();

        let path = env.home().join(".threadlane").join("credentials.json");
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn test_save_openai_api_key_rejects_empty_key() {
        let _env = TestHomeGuard::new("empty");

        let err = save_openai_api_key("   ").unwrap_err();
        assert!(err.to_lowercase().contains("empty"));
        assert!(!err.contains("sk-test-123"));
        assert!(load_openai_api_key().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_save_openai_api_key_sets_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let env = TestHomeGuard::new("perms");

        save_openai_api_key("sk-permissions").unwrap();

        let path = env.home().join(".threadlane").join("openai_api_key");
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn test_save_openai_api_key_does_not_echo_secret_on_write_error() {
        use std::os::unix::fs::PermissionsExt;

        let env = TestHomeGuard::new("write-error");

        let threadlane = env.home().join(".threadlane");
        fs::create_dir_all(&threadlane).unwrap();
        fs::set_permissions(&threadlane, fs::Permissions::from_mode(0o555)).unwrap();

        let secret = "sk-super-secret";
        let err = save_openai_api_key(secret).unwrap_err();
        assert!(!err.contains(secret));
    }

    #[test]
    fn test_save_openai_api_key_overwrites_without_backup_path() {
        let env = TestHomeGuard::new("no-backup");

        save_openai_api_key("sk-first").unwrap();
        save_openai_api_key("sk-second").unwrap();

        let key_path = env.home().join(".threadlane").join("openai_api_key");
        let backup_path = env.home().join(".threadlane").join("openai_api_key.bak");

        assert_eq!(fs::read_to_string(&key_path).unwrap(), "sk-second");
        assert!(!backup_path.exists());
    }

    #[test]
    fn test_failed_api_key_replacement_preserves_existing_key() {
        let env = TestHomeGuard::new("failed-replacement");

        save_openai_api_key("sk-first").unwrap();

        let key_path = env.home().join(".threadlane").join("openai_api_key");
        let backup_path = key_path.with_extension("bak");
        let err = write_secure_text_file_with_replacer(&key_path, "sk-second", |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "forced failure",
            ))
        })
        .unwrap_err();

        assert!(err.contains("forced failure"));
        assert_eq!(fs::read_to_string(&key_path).unwrap(), "sk-first");
        assert!(!backup_path.exists());
    }

    #[test]
    fn test_load_credentials_still_reads_codex_openai_api_key() {
        let env = TestHomeGuard::new("codex");

        let codex_dir = env.home().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("auth.json"),
            r#"{"OPENAI_API_KEY":"codex-secret"}"#,
        )
        .unwrap();

        let creds = load_credentials().unwrap();
        assert_eq!(creds.access_token, "codex-secret");
        assert_eq!(creds.source, "~/.codex/auth.json");
    }
}
