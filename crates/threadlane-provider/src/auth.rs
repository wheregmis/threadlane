use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::path::PathBuf;

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

pub fn get_credentials_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let mut path = PathBuf::from(home);
    path.push(".threadlane");
    let _ = fs::create_dir_all(&path);
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
    let json = serde_json::to_string_pretty(&creds).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
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
        let body = res.text().await.unwrap_or_default();
        return Err(format!("Device login initiation failed ({status}): {body}"));
    }

    let text = res
        .text()
        .await
        .map_err(|e| format!("Failed to read device code body: {e}"))?;

    serde_json::from_str::<DeviceCodeResponse>(&text)
        .map_err(|e| format!("Failed to parse device code response ({e}): {text}"))
}

pub async fn poll_device_token(
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

    let val: Value = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse OAuth response body ({e}): {body}"))?;

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
        let _ = save_credentials(&tokens);
        return Ok(tokens);
    }

    let code_opt = val
        .get("authorization_code")
        .or_else(|| val.get("code"))
        .and_then(|v| v.as_str());

    if let Some(code) = code_opt {
        return exchange_authorization_code(code).await;
    }

    Err(format!("Unexpected OAuth token response: {body}"))
}

pub async fn exchange_authorization_code(code: &str) -> Result<OAuthTokens, String> {
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
    let val: Value = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse token exchange response ({e}): {body}"))?;

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
        let _ = save_credentials(&tokens);
        return Ok(tokens);
    }

    Err(format!("Code exchange failed: {body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_credentials_path() {
        let path = get_credentials_path();
        assert!(path.to_string_lossy().contains(".threadlane"));
    }
}
