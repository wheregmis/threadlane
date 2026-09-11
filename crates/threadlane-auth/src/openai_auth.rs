use crate::store::CredentialStore;
use crate::traits::AuthProvider;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

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
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";

/// Provider-specific Codex OAuth configuration.
///
/// The client ID must stay synchronized with the current Codex login
/// implementation, and the browser/device redirect URIs with the loopback
/// listeners below; stale values fail after browser authorization. Hosts that
/// embed this flow under their own registration pass their own values instead
/// of editing the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexOAuthConfig {
    pub client_id: String,
    pub token_url: String,
    pub authorize_url: String,
    pub browser_redirect_uri: String,
    pub device_usercode_url: String,
    pub device_token_url: String,
}

impl Default for CodexOAuthConfig {
    fn default() -> Self {
        Self {
            client_id: CLIENT_ID.to_string(),
            token_url: TOKEN_URL.to_string(),
            authorize_url: AUTHORIZE_URL.to_string(),
            browser_redirect_uri: BROWSER_REDIRECT_URI.to_string(),
            device_usercode_url: DEVICE_USERCODE_URL.to_string(),
            device_token_url: DEVICE_TOKEN_URL.to_string(),
        }
    }
}

fn account_store_guard() -> std::sync::MutexGuard<'static, ()> {
    // ponytail: in-process transactions around atomic file replacement; use
    // an advisory file lock if multiple Threadlane processes share credentials.
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    device_auth_id: String,
    user_code: String,
    #[serde(default = "default_verification_uri")]
    verification_uri: String,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(
        deserialize_with = "deserialize_string_or_number",
        default = "default_interval"
    )]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
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
    refresh_token: Option<String>,
    expires_at: Option<u64>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CodexAccountsStore {
    active_account_id: Option<String>,
    accounts: Vec<CodexAccount>,
}

impl CodexAccountsStore {
    fn active_account(&self) -> Option<&CodexAccount> {
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
    refresh_token: Option<String>,
    pub account_id: Option<String>,
    pub source: String,
}

fn jwt_claims(jwt: &str) -> Option<Value> {
    use base64::Engine;
    let payload = jwt.split('.').nth(1)?;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload))
        .ok()?;
    serde_json::from_slice(&payload_bytes).ok()
}

fn extract_jwt_claim(jwt: &str, claim_key: &str) -> Option<String> {
    if let Some(json) = jwt_claims(jwt) {
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

/// Test helper: the default store (tests point `HOME` at a temp dir).
#[cfg(test)]
fn get_credentials_path() -> PathBuf {
    CredentialStore::default().credentials_path()
}

/// Test helper: the default store (tests point `HOME` at a temp dir).
#[cfg(test)]
fn save_credentials_store(store: &CodexAccountsStore) -> Result<(), String> {
    save_credentials_store_in(store, &CredentialStore::default())
}

fn save_credentials_store_in(
    store: &CodexAccountsStore,
    locations: &CredentialStore,
) -> Result<(), String> {
    let path = locations.credentials_path();
    let json = serde_json::to_string_pretty(store)
        .map_err(|_| "Failed to serialize credentials".to_string())?;
    write_secure_text_file(&path, &json)
}

/// Test helper: the default store (tests point `HOME` at a temp dir).
#[cfg(test)]
fn add_or_update_account(tokens: &OAuthTokens) -> Result<CodexAccount, String> {
    add_or_update_account_in(tokens, &CredentialStore::default())
}

fn add_or_update_account_in(
    tokens: &OAuthTokens,
    locations: &CredentialStore,
) -> Result<CodexAccount, String> {
    let _guard = account_store_guard();
    let mut store = load_credentials_store_unlocked_in(locations);
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

    save_credentials_store_in(&store, locations)?;
    Ok(account)
}

/// Test helper: the default store (tests point `HOME` at a temp dir).
#[cfg(test)]
fn save_credentials(tokens: &OAuthTokens) -> Result<(), String> {
    save_credentials_in(tokens, &CredentialStore::default())
}

fn save_credentials_in(
    tokens: &OAuthTokens,
    locations: &CredentialStore,
) -> Result<(), String> {
    add_or_update_account_in(tokens, locations).map(|_| ())
}

pub fn is_own_source(source: &str) -> bool {
    source == "~/.threadlane/credentials.json"
}

pub fn remove_credentials() -> Result<(), String> {
    remove_credentials_in(&CredentialStore::default())
}

/// Removes the credentials file at the injected store's location.
pub fn remove_credentials_in(locations: &CredentialStore) -> Result<(), String> {
    let _guard = account_store_guard();
    remove_credentials_file_in(locations)
}

fn remove_credentials_file_in(locations: &CredentialStore) -> Result<(), String> {
    let path = locations.credentials_path();
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
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
    save_openai_api_key_in(key, &CredentialStore::default())
}

/// Saves the API key file at the injected store's location.
pub fn save_openai_api_key_in(key: &str, locations: &CredentialStore) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("OpenAI API key cannot be empty".to_string());
    }

    write_secure_text_file(&locations.openai_api_key_path(), key)
}

pub fn load_openai_api_key() -> Option<String> {
    load_openai_api_key_in(&CredentialStore::default())
}

/// Loads the API key file from the injected store's location.
pub fn load_openai_api_key_in(locations: &CredentialStore) -> Option<String> {
    let path = locations.openai_api_key_path();
    let key = fs::read_to_string(path).ok()?;
    let key = key.trim().to_string();
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

/// Test helper: the default store (tests point `HOME` at a temp dir).
#[cfg(test)]
fn load_credentials_store() -> CodexAccountsStore {
    load_credentials_store_in(&CredentialStore::default())
}

fn load_credentials_store_in(locations: &CredentialStore) -> CodexAccountsStore {
    let _guard = account_store_guard();
    load_credentials_store_unlocked_in(locations)
}

fn load_credentials_store_unlocked_in(locations: &CredentialStore) -> CodexAccountsStore {
    let threadlane_path = locations.credentials_path();

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
                    let _ = save_credentials_store_in(&store, locations);
                    return store;
                }
            }
        }
    }

    let codex_path = locations.codex_cli_auth_path();
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
    load_credentials_in(&CredentialStore::default())
}

/// Loads the active account from the injected store's location.
pub fn load_credentials_in(locations: &CredentialStore) -> Option<StoredCredentials> {
    let store = load_credentials_store_in(locations);
    let account = store.active_account()?;
    Some(StoredCredentials {
        access_token: account.access_token.clone(),
        refresh_token: account.refresh_token.clone(),
        account_id: account.account_id.clone(),
        source: account.source.clone(),
    })
}

pub fn load_all_codex_accounts() -> Vec<CodexAccount> {
    load_all_codex_accounts_in(&CredentialStore::default())
}

/// Lists all accounts in the injected store's location.
pub fn load_all_codex_accounts_in(locations: &CredentialStore) -> Vec<CodexAccount> {
    load_credentials_store_in(locations).accounts
}

pub fn get_active_codex_account() -> Option<CodexAccount> {
    get_active_codex_account_in(&CredentialStore::default())
}

/// Returns the active account from the injected store's location.
pub fn get_active_codex_account_in(locations: &CredentialStore) -> Option<CodexAccount> {
    load_credentials_store_in(locations).active_account().cloned()
}

/// Find the Threadlane account behind a token, including a token another
/// running provider has already refreshed. Never borrow another app's login.
pub fn codex_account_id_for_token(token: &str) -> Option<String> {
    codex_account_id_for_token_in(token, &CredentialStore::default())
}

/// Finds the owning account for `token` in the injected store's location.
pub fn codex_account_id_for_token_in(token: &str, locations: &CredentialStore) -> Option<String> {
    let identity = codex_token_identity(token);
    load_all_codex_accounts_in(locations)
        .into_iter()
        .filter(|account| is_own_source(&account.source))
        .find(|account| {
            account.access_token == token
                || (identity.is_some() && identity == codex_token_identity(&account.access_token))
        })
        .map(|account| account.id)
}

fn codex_token_identity(token: &str) -> Option<(String, String)> {
    let claims = jwt_claims(token)?;
    Some((
        claims.get("sub")?.as_str()?.to_string(),
        claims
            .get("https://api.openai.com/auth")?
            .get("chatgpt_account_id")?
            .as_str()?
            .to_string(),
    ))
}

pub fn get_backup_codex_accounts() -> Vec<CodexAccount> {
    get_backup_codex_accounts_in(&CredentialStore::default())
}

/// Lists non-active accounts from the injected store's location.
pub fn get_backup_codex_accounts_in(locations: &CredentialStore) -> Vec<CodexAccount> {
    let store = load_credentials_store_in(locations);
    let active_id = store.active_account().map(|a| a.id.clone());
    store
        .accounts
        .into_iter()
        .filter(|a| Some(&a.id) != active_id.as_ref())
        .collect()
}

pub fn set_active_codex_account(id: &str) -> Result<(), String> {
    set_active_codex_account_in(id, &CredentialStore::default())
}

/// Marks `id` active in the injected store's location.
pub fn set_active_codex_account_in(id: &str, locations: &CredentialStore) -> Result<(), String> {
    let _guard = account_store_guard();
    let mut store = load_credentials_store_unlocked_in(locations);
    if !store.accounts.iter().any(|a| a.id == id) {
        return Err(format!("Account '{id}' not found"));
    }
    store.active_account_id = Some(id.to_string());
    save_credentials_store_in(&store, locations)
}

pub fn remove_codex_account(id: &str) -> Result<(), String> {
    remove_codex_account_in(id, &CredentialStore::default())
}

/// Removes `id` from the injected store's location.
pub fn remove_codex_account_in(id: &str, locations: &CredentialStore) -> Result<(), String> {
    let _guard = account_store_guard();
    let mut store = load_credentials_store_unlocked_in(locations);
    let initial_len = store.accounts.len();
    store.accounts.retain(|a| a.id != id);
    if store.accounts.len() == initial_len {
        return Err(format!("Account '{id}' not found"));
    }
    if store.active_account_id.as_deref() == Some(id) {
        store.active_account_id = store.accounts.first().map(|a| a.id.clone());
    }
    if store.accounts.is_empty() {
        remove_credentials_file_in(locations)
    } else {
        save_credentials_store_in(&store, locations)
    }
}

fn codex_token_needs_refresh(account: &CodexAccount, now: u64) -> bool {
    let jwt_expiry = jwt_claims(&account.access_token)
        .and_then(|claims| claims.get("exp").and_then(Value::as_u64));
    account
        .expires_at
        .into_iter()
        .chain(jwt_expiry)
        .min()
        .is_some_and(|expiry| expiry <= now.saturating_add(60))
}

/// Resolve the account captured when a provider was created, even if the user
/// switches the active account while that provider is running.
pub async fn get_valid_codex_account_token(id: &str) -> Result<String, String> {
    get_valid_codex_account_token_in(id, &CredentialStore::default()).await
}

/// Resolves a usable token for `id` from the injected store's location,
/// refreshing with the default [`CodexOAuthConfig`] when near expiry.
pub async fn get_valid_codex_account_token_in(
    id: &str,
    locations: &CredentialStore,
) -> Result<String, String> {
    let config = CodexOAuthConfig::default();
    get_valid_codex_account_token_at(id, &config, locations.clone()).await
}

async fn get_valid_codex_account_token_at(
    id: &str,
    config: &CodexOAuthConfig,
    locations: CredentialStore,
) -> Result<String, String> {
    let id = id.to_string();
    let config = config.clone();
    let locations = locations;
    // Dropping a Tokio JoinHandle detaches it: cancelling a turn must not
    // abandon a rotated refresh token between the response and secure save.
    tokio::spawn(async move { resolve_codex_account_token(&id, &config, &locations).await })
        .await
        .map_err(|_| "Couldn't refresh ChatGPT sign-in. Try again.".to_string())?
}

async fn resolve_codex_account_token(
    id: &str,
    config: &CodexOAuthConfig,
    locations: &CredentialStore,
) -> Result<String, String> {
    // ponytail: one refresh gate for all accounts; use per-account gates if
    // concurrent refreshes become common. Reload inside it for rotating tokens.
    static REFRESH_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REFRESH_GATE.lock().await;
    let account = load_all_codex_accounts_in(locations)
        .into_iter()
        .find(|account| account.id == id && is_own_source(&account.source))
        .ok_or_else(|| {
            "ChatGPT account is no longer connected. Sign in again in Settings → Providers."
                .to_string()
        })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if !codex_token_needs_refresh(&account, now) {
        return Ok(account.access_token);
    }
    refresh_codex_account_token_at(&account, config, locations)
        .await
        .map(|account| account.access_token)
}

pub async fn refresh_codex_account_token(account: &CodexAccount) -> Result<CodexAccount, String> {
    refresh_codex_account_token_in(account, &CredentialStore::default()).await
}

/// Refreshes `account` with the default [`CodexOAuthConfig`], committing the
/// rotated tokens to the injected store's location.
pub async fn refresh_codex_account_token_in(
    account: &CodexAccount,
    locations: &CredentialStore,
) -> Result<CodexAccount, String> {
    let config = CodexOAuthConfig::default();
    refresh_codex_account_token_at(account, &config, locations).await
}

async fn refresh_codex_account_token_at(
    account: &CodexAccount,
    config: &CodexOAuthConfig,
    locations: &CredentialStore,
) -> Result<CodexAccount, String> {
    let refresh_token = account.refresh_token.as_ref().ok_or_else(|| {
        "ChatGPT sign-in expired. Sign in again in Settings → Providers.".to_string()
    })?;

    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh_token)
        .append_pair("client_id", &config.client_id)
        .finish();

    let client = reqwest::Client::new();
    let res = client
        .post(&config.token_url)
        .timeout(std::time::Duration::from_secs(30))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|_| {
            "Couldn't refresh ChatGPT sign-in. Check your connection and try again.".to_string()
        })?;

    let status = res.status();
    if !status.is_success() {
        return Err(
            if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
                "ChatGPT sign-in is temporarily unavailable. Try again shortly."
            } else {
                "ChatGPT sign-in expired. Sign in again in Settings → Providers."
            }
            .to_string(),
        );
    }
    let body = res
        .text()
        .await
        .map_err(|_| "Couldn't read the ChatGPT sign-in response. Try again.".to_string())?;
    let val: Value = crate::parse_oauth_response(&body)
        .map_err(|_| "ChatGPT returned an invalid sign-in response. Try again.".to_string())?;

    let access_token = val
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| "ChatGPT returned an invalid sign-in response. Try again.".to_string())?
        .to_string();

    let new_refresh = val
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|token| !token.trim().is_empty())
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
    let expires_at = Some(now.saturating_add(expires_in));

    let mut updated_account = account.clone();
    updated_account.access_token = access_token;
    updated_account.refresh_token = new_refresh;
    updated_account.expires_at = expires_at;

    commit_codex_account_refresh(account, updated_account, locations)
}

fn commit_codex_account_refresh(
    original: &CodexAccount,
    refreshed: CodexAccount,
    locations: &CredentialStore,
) -> Result<CodexAccount, String> {
    let _guard = account_store_guard();
    let mut store = load_credentials_store_unlocked_in(locations);
    let existing = store.accounts.iter_mut()
        .find(|account| account.id == original.id && is_own_source(&account.source))
        .ok_or_else(|| "ChatGPT account was disconnected during refresh. Sign in again in Settings → Providers.".to_string())?;
    if existing.access_token != original.access_token
        || existing.refresh_token != original.refresh_token
    {
        // A newer sign-in or refresh wins over this request's stale snapshot.
        return Ok(existing.clone());
    }
    existing.access_token = refreshed.access_token;
    existing.refresh_token = refreshed.refresh_token;
    existing.expires_at = refreshed.expires_at;
    let updated = existing.clone();
    save_credentials_store_in(&store, locations)
        .map_err(|_| "Couldn't save the refreshed ChatGPT sign-in. Check that your Threadlane settings folder is writable, then sign in again.".to_string())?;
    Ok(updated)
}

const BROWSER_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

pub fn build_browser_oauth_url(challenge: &str, state: &str) -> String {
    build_browser_oauth_url_with(challenge, state, &CodexOAuthConfig::default())
}

/// Builds the browser OAuth URL from an explicit provider configuration.
pub fn build_browser_oauth_url_with(
    challenge: &str,
    state: &str,
    config: &CodexOAuthConfig,
) -> String {
    let mut url = url::Url::parse(&config.authorize_url).unwrap();
    url.query_pairs_mut()
        .append_pair("client_id", &config.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &config.browser_redirect_uri)
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
    exchange_browser_code_for_tokens_in(
        code,
        code_verifier,
        &CodexOAuthConfig::default(),
        &CredentialStore::default(),
    )
    .await
}

/// Exchanges a browser OAuth code with an explicit provider configuration,
/// persisting the account to the injected store's location.
pub async fn exchange_browser_code_for_tokens_in(
    code: &str,
    code_verifier: &str,
    config: &CodexOAuthConfig,
    locations: &CredentialStore,
) -> Result<CodexAccount, String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", &config.browser_redirect_uri)
        .append_pair("client_id", &config.client_id)
        .append_pair("code_verifier", code_verifier)
        .finish();

    let client = reqwest::Client::new();
    let res = client
        .post(&config.token_url)
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
        return add_or_update_account_in(&tokens, locations);
    }

    Err("Code exchange returned no access token".into())
}

pub async fn start_device_login() -> Result<DeviceCodeResponse, String> {
    start_device_login_with(&CodexOAuthConfig::default()).await
}

/// Starts device login against an explicit provider configuration.
pub async fn start_device_login_with(
    config: &CodexOAuthConfig,
) -> Result<DeviceCodeResponse, String> {
    let client = reqwest::Client::new();
    let res = client
        .post(&config.device_usercode_url)
        .json(&serde_json::json!({
            "client_id": config.client_id,
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
    poll_device_token_in(
        device_auth_id,
        user_code,
        &CodexOAuthConfig::default(),
        &CredentialStore::default(),
    )
    .await
}

/// Polls the device endpoint from an explicit provider configuration,
/// persisting the tokens to the injected store's location.
pub async fn poll_device_token_in(
    device_auth_id: &str,
    user_code: &str,
    config: &CodexOAuthConfig,
    locations: &CredentialStore,
) -> Result<OAuthTokens, String> {
    let tokens =
        poll_device_token_without_saving(device_auth_id, user_code, config).await?;
    save_credentials_in(&tokens, locations)?;
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
    config: &CodexOAuthConfig,
) -> Result<OAuthTokens, String> {
    let client = reqwest::Client::new();
    let res = client
        .post(&config.device_token_url)
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
            config,
        )
        .await;
    }

    Err("Unexpected OAuth token response".into())
}

fn token_exchange_body(code: &str, code_verifier: &str, config: &CodexOAuthConfig) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair(
            "redirect_uri",
            "https://auth.openai.com/deviceauth/callback",
        )
        .append_pair("client_id", &config.client_id)
        .append_pair("code_verifier", code_verifier)
        .finish()
}

async fn exchange_authorization_code_without_saving(
    code: &str,
    code_verifier: &str,
    config: &CodexOAuthConfig,
) -> Result<OAuthTokens, String> {
    let client = reqwest::Client::new();
    let res = client
        .post(&config.token_url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(token_exchange_body(code, code_verifier, config))
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
        let account = get_active_codex_account()
            .filter(|account| is_own_source(&account.source))
            .ok_or_else(|| {
                "No stored OpenAI credentials found. Please run /login openai".to_string()
            })?;
        get_valid_codex_account_token(&account.id).await
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
        let config = CodexOAuthConfig::default();
        let body = token_exchange_body("code with spaces", "verifier+/=", &config);
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

    fn test_jwt(subject: &str, account: &str, expiry: u64) -> String {
        use base64::Engine;
        let claims = serde_json::json!({
            "sub": subject,
            "exp": expiry,
            "https://api.openai.com/auth": { "chatgpt_account_id": account },
        });
        format!(
            "eyJhbGciOiJub25lIn0.{}.test",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string())
        )
    }

    #[test]
    fn codex_refresh_uses_legacy_jwt_expiry_and_refreshes_before_expiration() {
        let mut account = CodexAccount {
            id: "work".into(),
            label: "Work".into(),
            account_id: None,
            access_token: test_jwt("user", "work", 1_000),
            refresh_token: Some("test-refresh".into()),
            expires_at: None,
            source: "~/.threadlane/credentials.json".into(),
        };
        assert!(!codex_token_needs_refresh(&account, 939));
        assert!(codex_token_needs_refresh(&account, 940));
        assert!(codex_token_needs_refresh(&account, 1_100));
        account.expires_at = Some(2_000);
        assert!(codex_token_needs_refresh(&account, 1_100));
        account.access_token = "opaque-token".into();
        assert!(!codex_token_needs_refresh(&account, 1_100));
        assert!(codex_token_needs_refresh(&account, 1_940));
    }

    #[tokio::test]
    async fn codex_token_resolution_preserves_account_after_rotation_and_active_switch() {
        let _env = TestHomeGuard::new("token-resolution");
        let original = test_jwt("user", "work", 1);
        let fresh = test_jwt("user", "work", u64::MAX);
        let work = CodexAccount {
            id: "work-login".into(),
            label: "Work".into(),
            account_id: None,
            access_token: fresh.clone(),
            refresh_token: None,
            expires_at: None,
            source: "~/.threadlane/credentials.json".into(),
        };
        let mut store = CodexAccountsStore {
            active_account_id: Some("personal-login".into()),
            accounts: vec![
                work.clone(),
                CodexAccount {
                    id: "personal-login".into(),
                    access_token: test_jwt("user", "personal", u64::MAX),
                    ..work
                },
            ],
        };
        save_credentials_store(&store).unwrap();
        let id = codex_account_id_for_token(&original).unwrap();
        assert_eq!(id, "work-login");
        assert_eq!(get_valid_codex_account_token(&id).await.unwrap(), fresh);
        assert_eq!(get_active_codex_account().unwrap().id, "personal-login");
        assert!(codex_account_id_for_token(&test_jwt("another-user", "work", 1)).is_none());

        store.accounts[0].access_token = original.clone();
        save_credentials_store(&store).unwrap();
        let error = get_valid_codex_account_token(&id).await.unwrap_err();
        assert!(error.contains("Sign in again in Settings"));
        assert!(!error.contains(&original));
        assert_eq!(load_all_codex_accounts()[0].access_token, original);

        store.accounts[0].source = "~/.codex/auth.json".into();
        save_credentials_store(&store).unwrap();
        assert!(codex_account_id_for_token(&original).is_none());
        assert!(get_valid_codex_account_token(&id).await.is_err());
    }

    #[test]
    fn refresh_commit_preserves_new_sign_in_and_does_not_restore_removed_accounts() {
        let _env = TestHomeGuard::new("refresh-commit");
        let original = add_or_update_account(&OAuthTokens {
            access_token: "original-access".into(),
            refresh_token: Some("original-refresh".into()),
            expires_in: Some(1),
            id_token: None,
            account_id: Some("work".into()),
        })
        .unwrap();
        let refreshed = CodexAccount {
            access_token: "refreshed-access".into(),
            refresh_token: Some("rotated-refresh".into()),
            expires_at: Some(u64::MAX),
            ..original.clone()
        };
        let refreshed = commit_codex_account_refresh(&original, refreshed).unwrap();
        assert_eq!(get_active_codex_account().unwrap(), refreshed);

        let new_sign_in = add_or_update_account(&OAuthTokens {
            access_token: "new-sign-in-access".into(),
            refresh_token: Some("new-sign-in-refresh".into()),
            expires_in: Some(3_600),
            id_token: None,
            account_id: Some("work".into()),
        })
        .unwrap();
        assert_eq!(
            commit_codex_account_refresh(&original, refreshed.clone()).unwrap(),
            new_sign_in
        );
        assert_eq!(get_active_codex_account().unwrap(), new_sign_in);

        remove_codex_account("work").unwrap();
        assert!(commit_codex_account_refresh(&original, refreshed).is_err());
        assert!(!get_credentials_path().exists());
    }

    #[tokio::test]
    async fn cancelling_a_token_caller_does_not_abandon_rotated_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _env = TestHomeGuard::new("cancel-refresh");
        let account = add_or_update_account(&OAuthTokens {
            access_token: "old-access".into(),
            refresh_token: Some("old-refresh".into()),
            expires_in: Some(0),
            id_token: None,
            account_id: Some("work".into()),
        })
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (respond_tx, respond_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            started_tx.send(()).unwrap();
            respond_rx.await.unwrap();
            let body = r#"{"access_token":"fresh-access","refresh_token":"rotated-refresh","expires_in":3600}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let caller_id = account.id.clone();
        let caller_endpoint = endpoint.clone();
        let caller = tokio::spawn(async move {
            get_valid_codex_account_token_at(&caller_id, &caller_endpoint).await
        });
        started_rx.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        respond_tx.send(()).unwrap();
        server.await.unwrap();

        // The next caller waits for the existing refresh, then reads its saved
        // token. The mock listener is gone, so a duplicate request would fail.
        assert_eq!(
            get_valid_codex_account_token_at(&account.id, &endpoint)
                .await
                .unwrap(),
            "fresh-access"
        );
        let saved = get_active_codex_account().unwrap();
        assert_eq!(saved.access_token, "fresh-access");
        assert_eq!(saved.refresh_token.as_deref(), Some("rotated-refresh"));
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
