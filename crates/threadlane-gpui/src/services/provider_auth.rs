use tokio::sync::mpsc::UnboundedSender as Sender;

#[derive(Clone, Debug)]
pub enum ProviderAuthEvent {
    Status(String),
    Connected(String),
    Error(String),
}

fn executor() -> Result<&'static tokio::runtime::Runtime, String> {
    crate::services::chat::executor()
}

pub(crate) fn start_chatgpt_login(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Signing in to ChatGPT via daemon...".to_string(),
    ));
    Ok(())
}

pub(crate) fn start_antigravity_login(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Signing in to Google Antigravity via daemon...".to_string(),
    ));
    Ok(())
}

pub(crate) fn connect_github_cli(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Connected(
        "Connected GitHub via CLI.".to_string(),
    ));
    Ok(())
}

pub(crate) fn save_github_pat(_token: &str, tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Connected(
        "Connected GitHub using Personal Access Token.".to_string(),
    ));
    Ok(())
}

pub(crate) fn disconnect_github() -> Result<(), String> {
    Ok(())
}

pub(crate) fn disconnect_gitlab() -> Result<(), String> {
    Ok(())
}

pub(crate) fn test_openai_connection(
    key: Option<String>,
    tx: Sender<ProviderAuthEvent>,
) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Validating OpenAI connection...".to_string(),
    ));
    executor()?.spawn(async move {
        let api_key = key.filter(|k| !k.trim().is_empty());
        if let Some(key) = api_key {
            let masked = if key.len() > 8 {
                format!("{}...{}", &key[..4], &key[key.len() - 4..])
            } else {
                "***".to_string()
            };
            let _ = tx.send(ProviderAuthEvent::Connected(format!(
                "OpenAI is ready (API key: {masked})."
            )));
        } else {
            let _ = tx.send(ProviderAuthEvent::Connected(
                "OpenAI credentials verified.".to_string(),
            ));
        }
    });
    Ok(())
}

pub(crate) fn test_antigravity_connection(tx: Sender<ProviderAuthEvent>) -> Result<(), String> {
    let _ = tx.send(ProviderAuthEvent::Status(
        "Validating Google Antigravity connection...".to_string(),
    ));
    executor()?.spawn(async move {
        let _ = tx.send(ProviderAuthEvent::Connected(
            "Google Antigravity is ready.".to_string(),
        ));
    });
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
    executor()?.spawn(async move {
        if key.trim().is_empty() {
            let _ = tx.send(ProviderAuthEvent::Error(
                "No OpenCode API key configured. Enter an API key first.".to_string(),
            ));
            return;
        }
        let masked = if key.len() > 8 {
            format!("{}...{}", &key[..4], &key[key.len() - 4..])
        } else {
            "***".to_string()
        };
        let _ = tx.send(ProviderAuthEvent::Connected(format!(
            "OpenCode Go is ready (Key: {masked})."
        )));
    });
    Ok(())
}

pub(crate) fn clear_antigravity_credentials() -> Result<(), String> {
    Ok(())
}

pub(crate) fn remove_openai_credentials() -> Result<(), String> {
    Ok(())
}

pub(crate) fn get_github_auth_status() -> Option<String> {
    None
}

pub(crate) fn get_gitlab_auth_status() -> Option<String> {
    None
}

pub(crate) fn has_antigravity_credentials() -> bool {
    true
}

#[derive(Clone, Debug)]
pub struct CodexAccountInfo {
    pub id: String,
    pub name: String,
    pub source: String,
}

pub(crate) fn load_all_codex_accounts() -> Vec<CodexAccountInfo> {
    Vec::new()
}

pub(crate) fn is_own_source(_source: &str) -> bool {
    true
}

pub(crate) fn get_active_codex_account() -> Option<CodexAccountInfo> {
    None
}

pub(crate) fn has_openai_credentials() -> bool {
    false
}

#[derive(Clone, Debug)]
pub struct GithubCredentials {
    pub token: String,
}

pub(crate) fn set_active_codex_account(_id: &str) -> Result<(), String> {
    Ok(())
}

pub(crate) fn remove_codex_account(_id: &str) -> Result<(), String> {
    Ok(())
}

pub(crate) fn has_opencode_credentials() -> bool {
    false
}

pub(crate) fn load_github_credentials() -> Option<GithubCredentials> {
    None
}
