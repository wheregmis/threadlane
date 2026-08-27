//! Provider authentication service: checks credentials and initiates OAuth flows.
//! The actual credential storage is handled by threadlane-auth; this service
//! provides the protocol bridge so GPUI never touches auth state directly.

use threadlane_auth::{
    antigravity_auth, github_auth, openai_auth, opencode_auth,
};
use threadlane_protocol::capabilities::*;

#[derive(Clone, Default)]
pub struct AuthService;

impl AuthService {
    pub fn new() -> Self {
        Self
    }

    pub fn get_status(
        &self,
        req: GetProviderAuthRequest,
    ) -> Result<ProviderAuthStatusResponse, String> {
        let (connected, account) = match req.provider {
            ProviderKind::Antigravity => {
                let creds = antigravity_auth::load_antigravity_credentials();
                let account = creds.as_ref().and_then(|c| c.account_email.clone());
                (creds.is_some(), account)
            }
            ProviderKind::OpenAi => {
                let key = openai_auth::load_openai_api_key();
                let store = openai_auth::load_credentials_store();
                let connected = key.is_some() || !store.accounts.is_empty();
                (connected, None)
            }
            ProviderKind::OpenCode => {
                let key = opencode_auth::load_opencode_api_key();
                (key.is_some(), None)
            }
            ProviderKind::GitHub => {
                let account = github_auth::get_github_auth_status();
                (account.is_some(), account)
            }
            ProviderKind::GitLab => {
                let account = github_auth::get_gitlab_auth_status();
                (account.is_some(), account)
            }
        };
        Ok(ProviderAuthStatusResponse {
            provider: req.provider,
            connected,
            account,
        })
    }

    pub fn connect(
        &self,
        req: ConnectProviderRequest,
    ) -> Result<ConnectProviderResponse, String> {
        match req.provider {
            ProviderKind::Antigravity => {
                // Build a PKCE challenge and return the browser URL.
                let (verifier, challenge) = antigravity_auth::generate_pkce_pair();
                let state = format!("{:x}", std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0));
                let url = antigravity_auth::build_authorization_url(&challenge, &state);
                // Store verifier for later exchange (GPUI will poll `auth/status`).
                let _ = verifier; // TODO: persist verifier for token exchange
                Ok(ConnectProviderResponse {
                    status: "pending".to_string(),
                    auth_url: Some(url),
                })
            }
            ProviderKind::OpenAi => {
                if let Some(key) = req.api_key {
                    openai_auth::save_openai_api_key(&key)
                        .map_err(|e| format!("Failed to save OpenAI key: {e}"))?;
                    Ok(ConnectProviderResponse {
                        status: "connected".to_string(),
                        auth_url: None,
                    })
                } else {
                    // Codex OAuth device-code flow: generate PKCE and return browser URL.
                    let (verifier, challenge) = antigravity_auth::generate_pkce_pair();
                    let state = format!("{:x}", std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0));
                    let url = openai_auth::build_browser_oauth_url(&challenge, &state);
                    let _ = verifier;
                    Ok(ConnectProviderResponse {
                        status: "pending".to_string(),
                        auth_url: Some(url),
                    })
                }
            }
            ProviderKind::OpenCode => {
                if let Some(key) = req.api_key {
                    opencode_auth::save_opencode_api_key(&key)
                        .map_err(|e| format!("Failed to save OpenCode key: {e}"))?;
                    Ok(ConnectProviderResponse {
                        status: "connected".to_string(),
                        auth_url: None,
                    })
                } else {
                    Err("OpenCode requires an API key".into())
                }
            }
            ProviderKind::GitHub => {
                // Trigger gh CLI sync (will return the login URL if CLI is available).
                match github_auth::sync_from_gh_cli() {
                    Ok(_) => Ok(ConnectProviderResponse {
                        status: "connected".to_string(),
                        auth_url: None,
                    }),
                    Err(_) => Ok(ConnectProviderResponse {
                        status: "pending".to_string(),
                        auth_url: Some("https://github.com/login/device".to_string()),
                    }),
                }
            }
            ProviderKind::GitLab => Ok(ConnectProviderResponse {
                status: "pending".to_string(),
                auth_url: Some(
                    "https://gitlab.com/-/profile/personal_access_tokens".to_string(),
                ),
            }),
        }
    }

    pub fn disconnect(&self, req: DisconnectProviderRequest) -> Result<(), String> {
        match req.provider {
            ProviderKind::Antigravity => {
                antigravity_auth::clear_antigravity_credentials()
            }
            ProviderKind::OpenAi => openai_auth::remove_credentials(),
            ProviderKind::OpenCode => opencode_auth::clear_opencode_api_key(),
            ProviderKind::GitHub => github_auth::remove_github_credentials(),
            ProviderKind::GitLab => github_auth::remove_gitlab_credentials(),
        }
    }
}
