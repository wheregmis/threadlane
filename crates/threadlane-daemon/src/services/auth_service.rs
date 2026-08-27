//! Provider authentication service: checks credentials and initiates OAuth flows.
//! The actual credential storage is handled by threadlane-auth; this service
//! provides the protocol bridge so GPUI never touches auth state directly.

use std::sync::{Arc, Mutex};

use threadlane_auth::{antigravity_auth, github_auth, openai_auth, opencode_auth};
use threadlane_protocol::capabilities::*;

#[derive(Clone, Debug, Default)]
struct AuthFlowState {
    pending: bool,
    error: Option<String>,
}

type SharedAuthFlow = Arc<Mutex<AuthFlowState>>;

#[derive(Clone, Default)]
pub struct AuthService {
    antigravity_flow: SharedAuthFlow,
    openai_flow: SharedAuthFlow,
}

impl AuthService {
    pub fn new() -> Self {
        Self::default()
    }

    fn mark_pending(flow: &SharedAuthFlow) {
        if let Ok(mut state) = flow.lock() {
            state.pending = true;
            state.error = None;
        }
    }

    fn mark_completed(flow: &SharedAuthFlow) {
        if let Ok(mut state) = flow.lock() {
            state.pending = false;
            state.error = None;
        }
    }

    fn mark_failed(flow: &SharedAuthFlow, error: String) {
        if let Ok(mut state) = flow.lock() {
            state.pending = false;
            state.error = Some(error);
        }
    }

    fn flow_snapshot(flow: &SharedAuthFlow) -> AuthFlowState {
        flow.lock().map(|state| state.clone()).unwrap_or_default()
    }

    fn clear_flow(flow: &SharedAuthFlow) {
        if let Ok(mut state) = flow.lock() {
            *state = AuthFlowState::default();
        }
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
        let flow = match req.provider {
            ProviderKind::Antigravity => Self::flow_snapshot(&self.antigravity_flow),
            ProviderKind::OpenAi => Self::flow_snapshot(&self.openai_flow),
            ProviderKind::OpenCode | ProviderKind::GitHub | ProviderKind::GitLab => {
                AuthFlowState::default()
            }
        };
        Ok(ProviderAuthStatusResponse {
            provider: req.provider,
            connected,
            account,
            pending: flow.pending,
            error: flow.error,
        })
    }

    pub async fn connect(
        &self,
        req: ConnectProviderRequest,
    ) -> Result<ConnectProviderResponse, String> {
        match req.provider {
            ProviderKind::Antigravity => {
                // Build a PKCE challenge and return the browser URL.
                let (verifier, challenge) = antigravity_auth::generate_pkce_pair();
                let state = antigravity_auth::generate_oauth_state();
                let url = antigravity_auth::build_authorization_url(&challenge, &state);

                Self::mark_pending(&self.antigravity_flow);
                let expected_state = state.clone();
                let verifier_clone = verifier.clone();
                let flow = self.antigravity_flow.clone();
                tokio::spawn(async move {
                    tracing::info!("Antigravity OAuth listener starting on port 51121...");
                    let result = async {
                        let code =
                            antigravity_auth::listen_for_oauth_callback(expected_state).await?;
                        tracing::info!("Received Antigravity OAuth code, exchanging tokens...");
                        antigravity_auth::exchange_code_for_tokens(&code, &verifier_clone).await
                    }
                    .await;

                    match result {
                        Ok(creds) => {
                            Self::mark_completed(&flow);
                            tracing::info!(
                                "Antigravity authentication successful for {:?}",
                                creds.account_email
                            );
                        }
                        Err(error) => {
                            Self::mark_failed(&flow, error.clone());
                            tracing::error!("Antigravity authentication failed: {error}");
                        }
                    }
                });

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
                    let device = openai_auth::start_device_login().await?;
                    let auth_url = device.verification_uri;
                    let device_auth_id = device.device_auth_id;
                    let user_code = device.user_code;
                    let poll_interval = device.interval.max(1);

                    Self::mark_pending(&self.openai_flow);
                    let flow = self.openai_flow.clone();
                    let polling_code = user_code.clone();
                    tokio::spawn(async move {
                        tracing::info!("OpenAI device authorization polling started");
                        let deadline =
                            tokio::time::Instant::now() + std::time::Duration::from_secs(600);
                        let result = loop {
                            if tokio::time::Instant::now() >= deadline {
                                break Err("OpenAI device authorization timed out".to_string());
                            }

                            match openai_auth::poll_device_token(&device_auth_id, &polling_code)
                                .await
                            {
                                Ok(tokens) => break Ok(tokens),
                                Err(error) if error == "authorization_pending" => {
                                    tokio::time::sleep(std::time::Duration::from_secs(
                                        poll_interval,
                                    ))
                                    .await;
                                }
                                Err(error) => break Err(error),
                            }
                        };

                        match result {
                            Ok(_) => {
                                Self::mark_completed(&flow);
                                tracing::info!("OpenAI authentication successful");
                            }
                            Err(error) => {
                                Self::mark_failed(&flow, error.clone());
                                tracing::error!("OpenAI authentication failed: {error}");
                            }
                        }
                    });

                    Ok(ConnectProviderResponse {
                        status: format!(
                            "Enter code {user_code} in the ChatGPT authorization page."
                        ),
                        auth_url: Some(auth_url),
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
                auth_url: Some("https://gitlab.com/-/profile/personal_access_tokens".to_string()),
            }),
        }
    }

    pub fn list_codex_accounts(&self) -> Result<ListCodexAccountsResponse, String> {
        let store = openai_auth::load_credentials_store();
        let active_account_id = store.active_account().map(|account| account.id.clone());
        let accounts = store
            .accounts
            .into_iter()
            .map(|account| CodexAccountRecord {
                id: account.id,
                name: account.label,
                source: account.source,
            })
            .collect();
        Ok(ListCodexAccountsResponse {
            accounts,
            active_account_id,
        })
    }

    pub fn set_active_codex_account(
        &self,
        req: SetActiveCodexAccountRequest,
    ) -> Result<(), String> {
        openai_auth::set_active_codex_account(&req.id)
    }

    pub fn remove_codex_account(&self, req: RemoveCodexAccountRequest) -> Result<(), String> {
        openai_auth::remove_codex_account(&req.id)
    }

    pub fn disconnect(&self, req: DisconnectProviderRequest) -> Result<(), String> {
        match req.provider {
            ProviderKind::Antigravity => {
                Self::clear_flow(&self.antigravity_flow);
                antigravity_auth::clear_antigravity_credentials()
            }
            ProviderKind::OpenAi => {
                Self::clear_flow(&self.openai_flow);
                openai_auth::remove_credentials()
            }
            ProviderKind::OpenCode => opencode_auth::clear_opencode_api_key(),
            ProviderKind::GitHub => github_auth::remove_github_credentials(),
            ProviderKind::GitLab => github_auth::remove_gitlab_credentials(),
        }
    }
}
