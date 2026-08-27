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

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

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
    expires_at: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
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

#[derive(Debug, Deserialize)]
struct DeviceAuthorizationCode {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexAccount {
    pub id: String,
    pub label: String,
    pub account_id: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CodexAccountsStore {
    pub active_account_id: Option<String>,
    pub accounts: Vec<CodexAccount>,
}

impl CodexAccountsStore {
    pub fn active_account(&self) -> Option<&CodexAccount> {
        if let Some(active_id) = &self.active_account_id {
            if let Some(acc) = self.accounts.iter().find(|a| &a.id == active_id) {
                return Some(acc);
            }
        }
        self.accounts.first()
    }
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

fn get_credentials_path() -> PathBuf {
    let mut path = get_threadlane_dir();
    path.push("credentials.json");
    path
}

fn extract_jwt_claim(jwt: &str, claim_key: &str) -> Option<String> {
    use base64::Engine;
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() >= 2 {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let standard_engine = base64::engine::general_purpose::STANDARD_NO_PAD;
        let payload_bytes = engine
            .decode(parts[1])
            .or_else(|_| standard_engine.decode(parts[1]))
            .ok()?;
        let json: Value = serde_json::from_slice(&payload_bytes).ok()?;
        if let Some(val) = json.get(claim_key).and_then(Value::as_str) {
            return Some(val.to_string());
        }
        if claim_key == "email" {
            if let Some(email) = json
                .get("https://api.openai.com/profile")
                .and_then(|p| p.get("email"))
                .and_then(Value::as_str)
            {
                return Some(email.to_string());
            }
        }
    }
    None
}

pub fn save_credentials_store(store: &CodexAccountsStore) -> Result<(), String> {
    let path = get_credentials_path();
    let json = serde_json::to_string_pretty(store)
        .map_err(|_| "Failed to serialize credentials".to_string())?;
    write_secure_text_file(&path, &json)
}

pub fn add_or_update_account(tokens: &OAuthTokens) -> Result<CodexAccount, String> {
    let mut store = load_credentials_store();
    let email = tokens
        .id_token
        .as_deref()
        .and_then(|jwt| extract_jwt_claim(jwt, "email"))
        .or_else(|| extract_jwt_claim(&tokens.access_token, "email"));

    let id = email
        .clone()
        .or_else(|| tokens.account_id.clone())
        .unwrap_or_else(|| {
            let prefix = if tokens.access_token.len() >= 12 {
                &tokens.access_token[..12]
            } else {
                &tokens.access_token
            };
            format!("account_{prefix}")
        });

    let label = email.unwrap_or_else(|| {
        if let Some(account_id) = &tokens.account_id {
            format!("Account ({account_id})")
        } else {
            format!("Account {}", store.accounts.len() + 1)
        }
    });

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let expires_at = tokens.expires_in.map(|exp| now + exp);

    let account = CodexAccount {
        id: id.clone(),
        label,
        account_id: tokens.account_id.clone(),
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        expires_at,
        source: "~/.threadlane/credentials.json".to_string(),
    };

    if let Some(existing) = store
        .accounts
        .iter_mut()
        .find(|a| a.id == id || (a.account_id.is_some() && a.account_id == tokens.account_id))
    {
        existing.access_token = account.access_token.clone();
        existing.refresh_token = account
            .refresh_token
            .clone()
            .or_else(|| existing.refresh_token.clone());
        existing.expires_at = account.expires_at.or(existing.expires_at);
        existing.account_id = account
            .account_id
            .clone()
            .or_else(|| existing.account_id.clone());
    } else {
        store.accounts.push(account.clone());
    }

    if store.active_account_id.is_none() {
        store.active_account_id = Some(id);
    }

    save_credentials_store(&store)?;
    Ok(account)
}

fn save_credentials(tokens: &OAuthTokens) -> Result<(), String> {
    add_or_update_account(tokens).map(|_| ())
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

pub fn load_credentials_store() -> CodexAccountsStore {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let threadlane_path = get_credentials_path();

    if threadlane_path.exists() {
        if let Ok(content) = fs::read_to_string(&threadlane_path) {
            if let Ok(store) = serde_json::from_str::<CodexAccountsStore>(&content) {
                if !store.accounts.is_empty() {
                    return store;
                }
            }
            if let Ok(legacy) = serde_json::from_str::<StoredCredentials>(&content) {
                if !legacy.access_token.is_empty() {
                    let id = legacy
                        .account_id
                        .clone()
                        .unwrap_or_else(|| "account_1".to_string());
                    let label = legacy
                        .account_id
                        .as_deref()
                        .map(|id| format!("Account ({id})"))
                        .unwrap_or_else(|| "Account 1".to_string());
                    let store = CodexAccountsStore {
                        active_account_id: Some(id.clone()),
                        accounts: vec![CodexAccount {
                            id,
                            label,
                            account_id: legacy.account_id,
                            access_token: legacy.access_token,
                            refresh_token: legacy.refresh_token,
                            expires_at: None,
                            source: legacy.source,
                        }],
                    };
                    let _ = save_credentials_store(&store);
                    return store;
                }
            }
        }
    }

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

                        let id = account_id
                            .clone()
                            .unwrap_or_else(|| "codex_cli".to_string());
                        return CodexAccountsStore {
                            active_account_id: Some(id.clone()),
                            accounts: vec![CodexAccount {
                                id,
                                label: "Codex CLI".to_string(),
                                account_id,
                                access_token: token.to_string(),
                                refresh_token: tokens
                                    .get("refresh_token")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string()),
                                expires_at: None,
                                source: "~/.codex/auth.json".to_string(),
                            }],
                        };
                    }
                }
                if let Some(key) = val.get("OPENAI_API_KEY").and_then(|v| v.as_str()) {
                    if !key.is_empty() {
                        return CodexAccountsStore {
                            active_account_id: Some("codex_api_key".to_string()),
                            accounts: vec![CodexAccount {
                                id: "codex_api_key".to_string(),
                                label: "Codex API Key".to_string(),
                                account_id: None,
                                access_token: key.to_string(),
                                refresh_token: None,
                                expires_at: None,
                                source: "~/.codex/auth.json".to_string(),
                            }],
                        };
                    }
                }
            }
        }
    }

    CodexAccountsStore::default()
}

pub fn load_credentials() -> Option<StoredCredentials> {
    let store = load_credentials_store();
    let account = store.active_account()?;
    Some(StoredCredentials {
        access_token: account.access_token.clone(),
        refresh_token: account.refresh_token.clone(),
        account_id: account.account_id.clone(),
        source: account.source.clone(),
    })
}

pub fn load_all_codex_accounts() -> Vec<CodexAccount> {
    load_credentials_store().accounts
}

pub fn get_active_codex_account() -> Option<CodexAccount> {
    load_credentials_store().active_account().cloned()
}

pub fn get_backup_codex_accounts() -> Vec<CodexAccount> {
    let store = load_credentials_store();
    let active_id = store.active_account().map(|a| a.id.clone());
    store
        .accounts
        .into_iter()
        .filter(|a| Some(&a.id) != active_id.as_ref())
        .collect()
}

pub fn set_active_codex_account(id: &str) -> Result<(), String> {
    let mut store = load_credentials_store();
    if !store.accounts.iter().any(|a| a.id == id) {
        return Err(format!("Account '{id}' not found"));
    }
    store.active_account_id = Some(id.to_string());
    save_credentials_store(&store)
}

pub fn remove_codex_account(id: &str) -> Result<(), String> {
    let mut store = load_credentials_store();
    let initial_len = store.accounts.len();
    store.accounts.retain(|a| a.id != id);
    if store.accounts.len() == initial_len {
        return Err(format!("Account '{id}' not found"));
    }
    if store.active_account_id.as_deref() == Some(id) {
        store.active_account_id = store.accounts.first().map(|a| a.id.clone());
    }
    if store.accounts.is_empty() {
        remove_credentials()
    } else {
        save_credentials_store(&store)
    }
}

pub async fn refresh_codex_account_token(account: &CodexAccount) -> Result<CodexAccount, String> {
    let refresh_token = account
        .refresh_token
        .as_ref()
        .ok_or_else(|| "No refresh token available for account".to_string())?;

    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh_token)
        .append_pair("client_id", CLIENT_ID)
        .finish();

    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/oauth/token")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Failed to refresh Codex OAuth token: {e}"))?;

    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    let val: Value = crate::parse_oauth_response(&body)?;

    if !status.is_success() {
        let reason = val
            .get("error_description")
            .or_else(|| val.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("Token refresh failed ({status}): {reason}"));
    }

    let access_token = val
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing access_token in refresh response".to_string())?
        .to_string();

    let new_refresh = val
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| account.refresh_token.clone());

    let expires_in = val
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let expires_at = Some(now + expires_in);

    let mut store = load_credentials_store();
    let mut updated_account = account.clone();
    updated_account.access_token = access_token;
    updated_account.refresh_token = new_refresh;
    updated_account.expires_at = expires_at;

    if let Some(existing) = store.accounts.iter_mut().find(|a| a.id == account.id) {
        *existing = updated_account.clone();
        save_credentials_store(&store)?;
    }

    Ok(updated_account)
}

const BROWSER_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

pub fn build_browser_oauth_url(challenge: &str, state: &str) -> String {
    let mut url = url::Url::parse("https://auth.openai.com/oauth/authorize").unwrap();
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", BROWSER_REDIRECT_URI)
        .append_pair("scope", "openid profile email offline_access")
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    url.to_string()
}

pub async fn listen_for_browser_oauth_callback(expected_state: String) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:1455")
        .or_else(|_| TcpListener::bind("0.0.0.0:1455"))
        .map_err(|e| format!("Failed to bind loopback callback listener on port 1455: {e}"))?;

    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to set listener non-blocking: {e}"))?;

    let start_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.saturating_sub(start_time) > 300 {
            return Err("OAuth callback timed out after 5 minutes".to_string());
        }

        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buffer = [0u8; 4096];
                if let Ok(bytes_read) = stream.read(&mut buffer) {
                    let request_str = String::from_utf8_lossy(&buffer[..bytes_read]);
                    if let Some(first_line) = request_str.lines().next() {
                        if first_line.starts_with("GET /auth/callback")
                            || first_line.starts_with("GET /")
                        {
                            let path = first_line.split_whitespace().nth(1).unwrap_or("");
                            if let Ok(parsed_url) =
                                url::Url::parse(&format!("http://localhost:1455{path}"))
                            {
                                let mut code = None;
                                let mut state = None;
                                let mut error = None;
                                let mut error_desc = None;
                                for (k, v) in parsed_url.query_pairs() {
                                    if k == "code" {
                                        code = Some(v.to_string());
                                    } else if k == "state" {
                                        state = Some(v.to_string());
                                    } else if k == "error" {
                                        error = Some(v.to_string());
                                    } else if k == "error_description" {
                                        error_desc = Some(v.to_string());
                                    }
                                }

                                if let Some(err) = error {
                                    let desc = error_desc.unwrap_or_else(|| err.clone());
                                    let html = format!("HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!DOCTYPE html><html><body style='font-family:sans-serif;background:#0d1117;color:#f85149;padding:40px;text-align:center;'><h2>Authentication Error</h2><p>{desc}</p></body></html>");
                                    let _ = stream.write_all(html.as_bytes());
                                    let _ = stream.flush();
                                    return Err(format!("OAuth error: {desc}"));
                                }

                                let (res_code, html_response) = if let (Some(code), Some(st)) =
                                    (code, state)
                                {
                                    if st == expected_state {
                                        (
                                            Ok(code),
                                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!DOCTYPE html><html><body style='font-family:sans-serif;background:#0d1117;color:#10a37f;padding:40px;text-align:center;'><h2>ChatGPT Authentication Successful!</h2><p>You may now close this tab and return to Threadlane.</p></body></html>",
                                        )
                                    } else {
                                        (
                                            Err("OAuth state mismatch".to_string()),
                                            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h2>Authentication Error</h2><p>State mismatch.</p></body></html>",
                                        )
                                    }
                                } else {
                                    (
                                        Err("Missing code or state in OAuth callback".to_string()),
                                        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h2>Authentication Error</h2><p>Missing parameters.</p></body></html>",
                                    )
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
                return Err(format!("Failed to accept callback connection: {e}"));
            }
        }
    }
}

pub async fn exchange_browser_code_for_tokens(
    code: &str,
    code_verifier: &str,
) -> Result<CodexAccount, String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", BROWSER_REDIRECT_URI)
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code_verifier", code_verifier)
        .finish();

    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/oauth/token")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Error exchanging code for OAuth token: {e}"))?;

    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    let val: Value = crate::parse_oauth_response(&body)?;

    if !status.is_success() {
        let reason = val
            .get("error_description")
            .or_else(|| val.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("Code exchange failed ({status}): {reason}"));
    }

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
        return add_or_update_account(&tokens);
    }

    Err("Code exchange returned no access token".into())
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
    save_credentials(&tokens)?;
    Ok(tokens)
}

fn device_token_error(status: reqwest::StatusCode, body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = value.get("error");
    let code = error
        .and_then(Value::as_str)
        .or_else(|| {
            error
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
        })
        .or_else(|| value.get("code").and_then(Value::as_str));

    if matches!(
        code,
        Some("authorization_pending" | "deviceauth_authorization_pending")
    ) {
        return Some("authorization_pending".to_string());
    }
    if status.is_success() {
        return None;
    }

    let reason = value
        .get("error_description")
        .and_then(Value::as_str)
        .or_else(|| {
            error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .or(code)
        .unwrap_or("unknown error");
    Some(format!("Device login failed ({status}): {reason}"))
}

async fn poll_device_token_without_saving(
    device_auth_id: &str,
    user_code: &str,
) -> Result<OAuthTokens, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/api/accounts/deviceauth/token")
        .json(&serde_json::json!({
            "device_auth_id": device_auth_id,
            "user_code": user_code
        }))
        .send()
        .await
        .map_err(|e| format!("Error polling device token: {e}"))?;

    let status = res.status();
    let body = res.text().await.unwrap_or_default();

    if let Some(error) = device_token_error(status, &body) {
        return Err(error);
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

    if val.get("authorization_code").is_some() {
        let code: DeviceAuthorizationCode = serde_json::from_value(val).map_err(|_| {
            "OAuth provider returned an incomplete device code response".to_string()
        })?;
        return exchange_authorization_code_without_saving(
            &code.authorization_code,
            &code.code_verifier,
        )
        .await;
    }

    Err("Unexpected OAuth token response".into())
}

fn token_exchange_body(code: &str, code_verifier: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair(
            "redirect_uri",
            "https://auth.openai.com/deviceauth/callback",
        )
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code_verifier", code_verifier)
        .finish()
}

async fn exchange_authorization_code_without_saving(
    code: &str,
    code_verifier: &str,
) -> Result<OAuthTokens, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://auth.openai.com/oauth/token")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(token_exchange_body(code, code_verifier))
        .send()
        .await
        .map_err(|e| format!("Error exchanging code for OAuth token: {e}"))?;

    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    let val: Value = crate::parse_oauth_response(&body)?;

    if !status.is_success() {
        let reason = val
            .get("error_description")
            .or_else(|| val.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("Code exchange failed ({status}): {reason}"));
    }

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
    fn test_uses_current_codex_oauth_client() {
        assert_eq!(CLIENT_ID, "app_EMoamEEZ73f0CkXaXp7hrann");
    }

    #[test]
    fn test_device_authorization_code_includes_pkce_verifier() {
        let response: DeviceAuthorizationCode = serde_json::from_str(
            r#"{
                "authorization_code": "authorization-secret",
                "code_challenge": "challenge",
                "code_verifier": "verifier-secret"
            }"#,
        )
        .unwrap();

        assert_eq!(response.authorization_code, "authorization-secret");
        assert_eq!(response.code_verifier, "verifier-secret");
    }

    #[test]
    fn test_token_exchange_body_uses_device_callback_and_pkce() {
        let body = token_exchange_body("code with spaces", "verifier+/=");
        let params = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(params.get("grant_type").unwrap(), "authorization_code");
        assert_eq!(params.get("code").unwrap(), "code with spaces");
        assert_eq!(
            params.get("redirect_uri").unwrap(),
            "https://auth.openai.com/deviceauth/callback"
        );
        assert_eq!(params.get("client_id").unwrap(), CLIENT_ID);
        assert_eq!(params.get("code_verifier").unwrap(), "verifier+/=");
    }

    #[test]
    fn test_device_token_error_retries_only_explicit_pending_response() {
        assert_eq!(
            device_token_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"error":{"code":"deviceauth_authorization_pending"}}"#,
            ),
            Some("authorization_pending".to_string())
        );
        assert_eq!(
            device_token_error(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":"authorization_pending"}"#,
            ),
            Some("authorization_pending".to_string())
        );
    }

    #[test]
    fn test_device_token_error_stops_on_terminal_forbidden_or_not_found() {
        let forbidden = device_token_error(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"error":"authorization_expired"}"#,
        )
        .unwrap();
        assert!(forbidden.contains("403 Forbidden"));
        assert!(forbidden.contains("authorization_expired"));

        let not_found = device_token_error(
            reqwest::StatusCode::NOT_FOUND,
            r#"{"error":"unknown_device"}"#,
        )
        .unwrap();
        assert!(not_found.contains("404 Not Found"));
        assert!(not_found.contains("unknown_device"));
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

    #[test]
    fn test_multi_account_storage_and_switching() {
        let env = TestHomeGuard::new("multi-account");

        let acc1 = add_or_update_account(&OAuthTokens {
            access_token: "token-acc-1".into(),
            refresh_token: Some("refresh-1".into()),
            expires_in: Some(3600),
            id_token: None,
            account_id: Some("acc_work".into()),
        })
        .unwrap();

        assert_eq!(acc1.id, "acc_work");
        assert_eq!(get_active_codex_account().unwrap().id, "acc_work");

        let acc2 = add_or_update_account(&OAuthTokens {
            access_token: "token-acc-2".into(),
            refresh_token: Some("refresh-2".into()),
            expires_in: Some(3600),
            id_token: None,
            account_id: Some("acc_personal".into()),
        })
        .unwrap();

        assert_eq!(acc2.id, "acc_personal");
        let all = load_all_codex_accounts();
        assert_eq!(all.len(), 2);

        // Active account remains acc1 until changed
        assert_eq!(get_active_codex_account().unwrap().id, "acc_work");
        let backups = get_backup_codex_accounts();
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].id, "acc_personal");

        // Switch to personal
        set_active_codex_account("acc_personal").unwrap();
        assert_eq!(get_active_codex_account().unwrap().id, "acc_personal");

        let active_creds = load_credentials().unwrap();
        assert_eq!(active_creds.access_token, "token-acc-2");
        assert_eq!(active_creds.account_id.as_deref(), Some("acc_personal"));

        // Remove personal account -> work becomes active again
        remove_codex_account("acc_personal").unwrap();
        assert_eq!(load_all_codex_accounts().len(), 1);
        assert_eq!(get_active_codex_account().unwrap().id, "acc_work");

        let _ = env;
    }

    #[test]
    fn test_migration_from_legacy_stored_credentials() {
        let env = TestHomeGuard::new("legacy-migration");

        let threadlane_dir = env.home().join(".threadlane");
        fs::create_dir_all(&threadlane_dir).unwrap();
        let legacy_json = r#"{
            "access_token": "legacy-token-xyz",
            "refresh_token": "legacy-refresh-xyz",
            "account_id": "legacy_acc",
            "source": "~/.threadlane/credentials.json"
        }"#;
        fs::write(threadlane_dir.join("credentials.json"), legacy_json).unwrap();

        let store = load_credentials_store();
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.accounts[0].access_token, "legacy-token-xyz");
        assert_eq!(store.accounts[0].id, "legacy_acc");
        assert_eq!(store.active_account_id.as_deref(), Some("legacy_acc"));

        let creds = load_credentials().unwrap();
        assert_eq!(creds.access_token, "legacy-token-xyz");

        let _ = env;
    }
}
