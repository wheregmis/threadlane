use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginMode {
    ProviderPicker,
    OpenAiKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginProvider {
    Codex,
    OpenAi,
    Antigravity,
}

impl LoginProvider {
    pub const ALL: [Self; 3] = [Self::Codex, Self::OpenAi, Self::Antigravity];

    pub fn from_index(index: usize) -> Self {
        Self::ALL[index % Self::ALL.len()]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::OpenAi => "OpenAI",
            Self::Antigravity => "Antigravity",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LoginState {
    pub mode: LoginMode,
    pub status: Option<String>,
    pub pending: bool,
    pub attempt_id: u64,
    selected_provider: usize,
    masked_key: String,
    openai_key: String,
}

impl std::fmt::Debug for LoginState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoginState")
            .field("mode", &self.mode)
            .field("status", &self.status)
            .field("pending", &self.pending)
            .field("attempt_id", &self.attempt_id)
            .field("selected_provider", &self.selected_provider)
            .field("masked_key", &self.masked_key)
            .field("openai_key_len", &self.openai_key.len())
            .finish()
    }
}

impl Default for LoginState {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginState {
    pub fn new() -> Self {
        Self {
            mode: LoginMode::ProviderPicker,
            status: None,
            pending: false,
            attempt_id: 0,
            selected_provider: 0,
            masked_key: String::new(),
            openai_key: String::new(),
        }
    }

    pub fn selected_provider(&self) -> LoginProvider {
        LoginProvider::from_index(self.selected_provider)
    }

    pub fn select_next_provider(&mut self) {
        if !self.pending {
            self.selected_provider = (self.selected_provider + 1) % LoginProvider::ALL.len();
        }
    }

    pub fn select_previous_provider(&mut self) {
        if !self.pending {
            self.selected_provider =
                (self.selected_provider + LoginProvider::ALL.len() - 1) % LoginProvider::ALL.len();
        }
    }

    pub fn select_provider(&mut self, provider: LoginProvider) {
        self.selected_provider = LoginProvider::ALL
            .iter()
            .position(|candidate| *candidate == provider)
            .unwrap_or(0);
        self.pending = false;
        self.attempt_id = 0;
        self.status = None;
        self.clear_secret();
        self.mode = match provider {
            LoginProvider::OpenAi => LoginMode::OpenAiKey,
            LoginProvider::Codex | LoginProvider::Antigravity => LoginMode::ProviderPicker,
        };
    }

    pub fn begin_provider_flow(
        &mut self,
        provider: LoginProvider,
        attempt_id: u64,
        status: impl Into<String>,
    ) {
        self.selected_provider = LoginProvider::ALL
            .iter()
            .position(|candidate| *candidate == provider)
            .unwrap_or(0);
        self.mode = LoginMode::ProviderPicker;
        self.pending = true;
        self.attempt_id = attempt_id;
        self.status = Some(status.into());
        self.clear_secret();
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
    }

    pub fn push_char(&mut self, character: char) {
        if !self.pending && matches!(self.mode, LoginMode::OpenAiKey) {
            self.openai_key.push(character);
            self.masked_key.push('*');
        }
    }

    pub fn push_paste(&mut self, text: &str) {
        if !self.pending && matches!(self.mode, LoginMode::OpenAiKey) {
            self.openai_key.push_str(text);
            self.masked_key
                .extend(std::iter::repeat_n('*', text.chars().count()));
        }
    }

    pub fn backspace_key(&mut self) {
        if !self.pending && matches!(self.mode, LoginMode::OpenAiKey) {
            self.openai_key.pop();
            self.masked_key.pop();
        }
    }

    pub fn masked_key(&self) -> &str {
        &self.masked_key
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn save_openai_key(&mut self) -> Result<String, String> {
        let key = self.openai_key.trim().to_string();
        if key.is_empty() {
            let error = "OpenAI API key cannot be empty.".to_string();
            self.set_status(error.clone());
            return Err(error);
        }

        match threadlane_auth::save_openai_api_key(&key) {
            Ok(()) => {
                self.clear_secret();
                self.pending = false;
                self.attempt_id = 0;
                self.status = Some("Saved OpenAI API key.".into());
                Ok(key)
            }
            Err(error) => {
                self.clear_secret();
                self.pending = false;
                self.attempt_id = 0;
                self.status = Some(error.clone());
                Err(error)
            }
        }
    }

    pub fn clear_secret(&mut self) {
        self.masked_key.clear();
        self.openai_key.clear();
    }
}

#[derive(Debug)]
pub enum LoginEvent {
    DeviceCodePrompt {
        attempt_id: u64,
        user_code: String,
        url: String,
    },
    BrowserPrompt {
        attempt_id: u64,
        url: String,
    },
    CodexTokens {
        attempt_id: u64,
        tokens: Box<threadlane_auth::OAuthTokens>,
    },
    AntigravityCredentials {
        attempt_id: u64,
        credentials: Box<threadlane_auth::AntigravityCredentials>,
    },
    Failed {
        attempt_id: u64,
        message: String,
    },
}

pub fn spawn_provider_login(
    provider: LoginProvider,
    attempt_id: u64,
    tx: UnboundedSender<LoginEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        match provider {
            LoginProvider::Codex => run_codex_login(attempt_id, tx).await,
            LoginProvider::Antigravity => run_antigravity_login(attempt_id, tx).await,
            LoginProvider::OpenAi => {}
        }
    })
}

async fn run_codex_login(attempt_id: u64, tx: UnboundedSender<LoginEvent>) {
    match threadlane_auth::start_device_login().await {
        Ok(response) => {
            let _ = tx.send(LoginEvent::DeviceCodePrompt {
                attempt_id,
                user_code: response.user_code.clone(),
                url: response.verification_uri.clone(),
            });
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(response.interval.max(3)))
                    .await;
                match threadlane_auth::poll_device_token_without_saving(
                    &response.device_auth_id,
                    &response.user_code,
                )
                .await
                {
                    Ok(tokens) => {
                        let _ = tx.send(LoginEvent::CodexTokens {
                            attempt_id,
                            tokens: Box::new(tokens),
                        });
                        break;
                    }
                    Err(error) if error == "authorization_pending" || error.contains("pending") => {
                        continue;
                    }
                    Err(error) => {
                        let _ = tx.send(LoginEvent::Failed {
                            attempt_id,
                            message: error,
                        });
                        break;
                    }
                }
            }
        }
        Err(error) => {
            let _ = tx.send(LoginEvent::Failed {
                attempt_id,
                message: error,
            });
        }
    }
}

async fn run_antigravity_login(attempt_id: u64, tx: UnboundedSender<LoginEvent>) {
    let (verifier, challenge) = threadlane_auth::generate_pkce_pair();
    let (state, _) = threadlane_auth::generate_pkce_pair();
    let url = threadlane_auth::build_authorization_url(&challenge, &state);
    let _ = tx.send(LoginEvent::BrowserPrompt {
        attempt_id,
        url: url.clone(),
    });

    match threadlane_auth::listen_for_oauth_callback(state).await {
        Ok(code) => {
            match threadlane_auth::exchange_code_for_tokens_without_saving(&code, &verifier).await {
                Ok(credentials) => {
                    let _ = tx.send(LoginEvent::AntigravityCredentials {
                        attempt_id,
                        credentials: Box::new(credentials),
                    });
                }
                Err(error) => {
                    let _ = tx.send(LoginEvent::Failed {
                        attempt_id,
                        message: error,
                    });
                }
            }
        }
        Err(error) => {
            let _ = tx.send(LoginEvent::Failed {
                attempt_id,
                message: error,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LoginMode, LoginProvider, LoginState};

    #[test]
    fn openai_key_mode_masks_typed_and_pasted_input() {
        let mut state = LoginState::new();
        state.select_provider(LoginProvider::OpenAi);

        assert_eq!(state.mode, LoginMode::OpenAiKey);

        state.push_char('s');
        state.push_char('k');
        state.push_paste("-test-123");

        assert_eq!(state.masked_key(), "***********");
        assert!(state.openai_key.starts_with("sk-"));
    }
}
