//! Provider authentication service client — all credential operations via daemon.

use std::sync::Arc;
use std::time::Duration;

use threadlane_protocol::{
    client::DaemonClient, ConnectProviderRequest, DisconnectProviderRequest,
    GetProviderAuthRequest, ListCodexAccountsResponse, ProviderKind, RemoveCodexAccountRequest,
    SetActiveCodexAccountRequest,
};
use tokio::sync::mpsc::UnboundedSender as Sender;

#[derive(Clone, Debug)]
pub enum ProviderAuthEvent {
    Status(String),
    Connected(String),
    Error(String),
    /// Daemon wants GPUI to open this URL in the browser.
    AuthUrl(String),
}

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    crate::services::chat::executor()
}

async fn wait_for_oauth_completion(
    client: Arc<DaemonClient>,
    provider: ProviderKind,
    tx: Sender<ProviderAuthEvent>,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    let provider_name = match provider {
        ProviderKind::Antigravity => "Google Antigravity",
        ProviderKind::OpenAi => "ChatGPT",
        _ => return,
    };

    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;

        match client
            .get_provider_auth(GetProviderAuthRequest { provider })
            .await
        {
            Ok(status) => {
                if let Some(error) = status.error {
                    let _ = tx.send(ProviderAuthEvent::Error(error));
                    return;
                }
                if status.connected && !status.pending {
                    let account = status
                        .account
                        .as_deref()
                        .map(|value| format!(" as {value}"))
                        .unwrap_or_default();
                    let _ = tx.send(ProviderAuthEvent::Connected(format!(
                        "Connected to {provider_name}{account}."
                    )));
                    return;
                }
            }
            Err(error) => {
                let _ = tx.send(ProviderAuthEvent::Error(format!(
                    "Unable to check {provider_name} authentication: {error}"
                )));
                return;
            }
        }

        if tokio::time::Instant::now() >= deadline {
            let _ = tx.send(ProviderAuthEvent::Error(format!(
                "{provider_name} sign-in timed out. Please try again."
            )));
            return;
        }
    }
}

fn connect(provider: ProviderKind, api_key: Option<String>, tx: Sender<ProviderAuthEvent>) {
    if let Ok(rt) = executor() {
        rt.spawn(async move {
            let client = match crate::services::daemon_client::get_daemon_client().await {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(ProviderAuthEvent::Error(e));
                    return;
                }
            };
            match client
                .connect_provider(ConnectProviderRequest { provider, api_key })
                .await
            {
                Ok(res) => {
                    let status = res.status;
                    if let Some(url) = res.auth_url {
                        let _ = tx.send(ProviderAuthEvent::AuthUrl(url));
                        if status != "pending" {
                            let _ = tx.send(ProviderAuthEvent::Status(status));
                        }
                        if matches!(provider, ProviderKind::Antigravity | ProviderKind::OpenAi) {
                            wait_for_oauth_completion(client, provider, tx.clone()).await;
                        }
                    } else {
                        let _ = tx.send(ProviderAuthEvent::Connected(status));
                    }
                }
                Err(e) => {
                    let _ = tx.send(ProviderAuthEvent::Error(e));
                }
            }
        });
    }
}

fn disconnect(provider: ProviderKind) -> Result<(), String> {
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        client
            .disconnect_provider(DisconnectProviderRequest { provider })
            .await
    })
}

fn has_credentials(provider: ProviderKind) -> bool {
    executor()
        .ok()
        .and_then(|rt| {
            rt.block_on(async {
                let client = crate::services::daemon_client::get_daemon_client().await?;
                client
                    .get_provider_auth(GetProviderAuthRequest { provider })
                    .await
                    .map(|r| r.connected)
            })
            .ok()
        })
        .unwrap_or(false)
}

// ── Public API (matches call-sites in app_state.rs) ──────────────────────────

pub(crate) fn start_chatgpt_login(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Signing in to ChatGPT via daemon...".to_string(),
    ));
    connect(ProviderKind::OpenAi, None, tx);
    Ok(())
}

pub(crate) fn start_antigravity_login(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Signing in to Google Antigravity via daemon...".to_string(),
    ));
    connect(ProviderKind::Antigravity, None, tx);
    Ok(())
}

pub(crate) fn connect_github_cli(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Connecting GitHub via daemon...".to_string(),
    ));
    connect(ProviderKind::GitHub, None, tx);
    Ok(())
}

pub(crate) fn save_github_pat(token: &str, tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Saving GitHub Personal Access Token...".to_string(),
    ));
    connect(ProviderKind::GitHub, Some(token.to_string()), tx);
    Ok(())
}

pub(crate) fn disconnect_github() -> Result<(), String> {
    disconnect(ProviderKind::GitHub)
}

pub(crate) fn disconnect_gitlab() -> Result<(), String> {
    disconnect(ProviderKind::GitLab)
}

pub(crate) fn test_openai_connection(
    key: Option<String>,
    tx: Sender<ProviderAuthEvent>,
) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Validating OpenAI connection...".to_string(),
    ));
    connect(ProviderKind::OpenAi, key, tx);
    Ok(())
}

pub(crate) fn test_antigravity_connection(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Validating Google Antigravity connection...".to_string(),
    ));
    connect(ProviderKind::Antigravity, None, tx);
    Ok(())
}

pub(crate) fn test_opencode_connection(
    key: &str,
    tx: Sender<ProviderAuthEvent>,
) -> Result<(), String> {
    let key = key.to_string();
    let _ = tx.send(ProviderAuthEvent::Status(
        "Validating OpenCode connection...".to_string(),
    ));
    if key.trim().is_empty() {
        let _ = tx.send(ProviderAuthEvent::Error(
            "No OpenCode API key configured. Enter an API key first.".to_string(),
        ));
        return Ok(());
    }
    connect(ProviderKind::OpenCode, Some(key), tx);
    Ok(())
}

pub(crate) fn clear_antigravity_credentials() -> Result<(), String> {
    disconnect(ProviderKind::Antigravity)
}

pub(crate) fn remove_openai_credentials() -> Result<(), String> {
    disconnect(ProviderKind::OpenAi)
}

pub(crate) fn get_github_auth_status() -> Option<String> {
    executor()
        .ok()
        .and_then(|rt| {
            rt.block_on(async {
                let client = crate::services::daemon_client::get_daemon_client().await?;
                client
                    .get_provider_auth(GetProviderAuthRequest {
                        provider: ProviderKind::GitHub,
                    })
                    .await
                    .map(|r| r.account)
            })
            .ok()
        })
        .flatten()
}

pub(crate) fn get_gitlab_auth_status() -> Option<String> {
    executor()
        .ok()
        .and_then(|rt| {
            rt.block_on(async {
                let client = crate::services::daemon_client::get_daemon_client().await?;
                client
                    .get_provider_auth(GetProviderAuthRequest {
                        provider: ProviderKind::GitLab,
                    })
                    .await
                    .map(|r| r.account)
            })
            .ok()
        })
        .flatten()
}

pub(crate) fn has_antigravity_credentials() -> bool {
    has_credentials(ProviderKind::Antigravity)
}

pub(crate) fn has_openai_credentials() -> bool {
    has_credentials(ProviderKind::OpenAi)
}

pub(crate) fn has_opencode_credentials() -> bool {
    has_credentials(ProviderKind::OpenCode)
}

// ── Codex account helpers (OpenAI multi-account) ──────────────────────────────

#[derive(Clone, Debug)]
pub struct CodexAccountInfo {
    pub id: String,
    pub name: String,
    pub source: String,
}

fn codex_accounts_snapshot() -> Option<ListCodexAccountsResponse> {
    executor().ok().and_then(|rt| {
        rt.block_on(async {
            let client = crate::services::daemon_client::get_daemon_client().await?;
            client.list_codex_accounts().await
        })
        .ok()
    })
}

pub(crate) fn load_all_codex_accounts() -> Vec<CodexAccountInfo> {
    codex_accounts_snapshot()
        .map(|snapshot| {
            snapshot
                .accounts
                .into_iter()
                .map(|account| CodexAccountInfo {
                    id: account.id,
                    name: account.name,
                    source: account.source,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn is_own_source(source: &str) -> bool {
    source == "~/.threadlane/credentials.json"
}

pub(crate) fn get_active_codex_account() -> Option<CodexAccountInfo> {
    let snapshot = codex_accounts_snapshot()?;
    let active_id = snapshot.active_account_id?;
    snapshot
        .accounts
        .into_iter()
        .find(|account| account.id == active_id)
        .map(|account| CodexAccountInfo {
            id: account.id,
            name: account.name,
            source: account.source,
        })
}

pub(crate) fn set_active_codex_account(id: &str) -> Result<(), String> {
    let id = id.to_string();
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        client
            .set_active_codex_account(SetActiveCodexAccountRequest { id })
            .await
    })
}

pub(crate) fn remove_codex_account(id: &str) -> Result<(), String> {
    let id = id.to_string();
    executor()?.block_on(async {
        let client = crate::services::daemon_client::get_daemon_client().await?;
        client
            .remove_codex_account(RemoveCodexAccountRequest { id })
            .await
    })
}

#[derive(Clone, Debug)]
pub struct GithubCredentials {
    pub token: String,
}

pub(crate) fn load_github_credentials() -> Option<GithubCredentials> {
    None
}
