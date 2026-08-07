mod commands;
mod input;
mod login;
mod runtime;
mod tui;
mod ui;

use clap::Parser;
#[cfg(test)]
use runtime::{dispatch_input, Action};
use std::env;
#[cfg(test)]
use std::ffi::OsString;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use threadlane_agent::AgentEvent;
use threadlane_auth::{load_credentials, load_openai_api_key};
use threadlane_coding_agent::{CodingAgent, CodingAgentOptions};
#[cfg(test)]
use ui::{AppState, RunStatus};

#[derive(Parser, Debug)]
#[command(author, version, about = "Threadlane Terminal CLI & Ratatui TUI Runner", long_about = None)]
struct CliArgs {
    /// Optional single prompt for one-shot execution (streams directly to stdout)
    #[arg(short, long)]
    prompt: Option<String>,

    /// Model to use for generation
    #[arg(short, long, default_value = "gpt-4o")]
    model: String,

    /// Working directory for the agent
    #[arg(short, long)]
    dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = CliArgs::parse();
    let work_dir = args
        .dir
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let canonical_work_dir = std::fs::canonicalize(&work_dir).unwrap_or(work_dir);

    // If one-shot prompt is provided, run in headless mode streaming to stdout
    if let Some(prompt) = args.prompt {
        run_headless(canonical_work_dir, args.model, prompt).await?;
        return Ok(());
    }

    // Otherwise launch full Ratatui TUI interactive mode
    runtime::run_tui(canonical_work_dir, args.model).await?;
    Ok(())
}

async fn run_headless(
    work_dir: PathBuf,
    model: String,
    prompt: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let (api_key, account_id) = resolve_credentials();
    let mut agent = CodingAgent::new(CodingAgentOptions {
        api_key,
        account_id,
        model,
        work_dir,
        session_file: None,
        system_prompt: Default::default(),
    });

    let mut event_rx = agent.subscribe();
    let prompt_clone = prompt.clone();

    tokio::spawn(async move {
        let _ = agent.handle_input_with_images(&prompt_clone, vec![]).await;
    });

    while let Ok(event) = event_rx.recv().await {
        match event {
            AgentEvent::MessageUpdate {
                text_delta: Some(delta),
                ..
            } => {
                print!("{delta}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
            AgentEvent::AgentEnd { .. } => {
                println!();
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

pub(crate) fn resolve_credentials() -> (String, Option<String>) {
    let account_id = env::var("CHATGPT_ACCOUNT_ID").ok();
    if let Ok(api_key) = env::var("OPENAI_API_KEY") {
        if !api_key.trim().is_empty() {
            return (api_key, account_id);
        }
    }
    if let Some(api_key) = load_openai_api_key() {
        return (api_key, None);
    }
    if let Some(credentials) = load_credentials() {
        return (
            credentials.access_token,
            credentials.account_id.or(account_id),
        );
    }
    (String::new(), account_id)
}

#[cfg(test)]
pub(crate) fn test_env_guard_lock() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(())).lock().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved_home: Option<OsString>,
        saved_openai_key: Option<OsString>,
        saved_account_id: Option<OsString>,
        home: PathBuf,
    }

    impl EnvGuard {
        fn new(name: &str) -> Self {
            let lock = crate::test_env_guard_lock();
            let saved_home = env::var_os("HOME");
            let saved_openai_key = env::var_os("OPENAI_API_KEY");
            let saved_account_id = env::var_os("CHATGPT_ACCOUNT_ID");
            env::remove_var("OPENAI_API_KEY");
            env::remove_var("CHATGPT_ACCOUNT_ID");

            let home = std::env::temp_dir().join(format!(
                "threadlane-cli-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&home).unwrap();
            env::set_var("HOME", &home);

            Self {
                _lock: lock,
                saved_home,
                saved_openai_key,
                saved_account_id,
                home,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = self.saved_home.take() {
                env::set_var("HOME", value);
            } else {
                env::remove_var("HOME");
            }
            if let Some(value) = self.saved_openai_key.take() {
                env::set_var("OPENAI_API_KEY", value);
            } else {
                env::remove_var("OPENAI_API_KEY");
            }
            if let Some(value) = self.saved_account_id.take() {
                env::set_var("CHATGPT_ACCOUNT_ID", value);
            } else {
                env::remove_var("CHATGPT_ACCOUNT_ID");
            }
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn enter_submits_only_when_idle_and_composer_is_nonempty() {
        let mut state = AppState::test_state();
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::Submit),
            Action::Submit("".into())
        );
        state.composer = "inspect the project".into();
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::Submit),
            Action::Submit("inspect the project".into())
        );
        state.begin_generation();
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::Submit),
            Action::None
        );
    }

    #[test]
    fn escape_cancels_generation_before_quitting() {
        let mut state = AppState::test_state_generating();
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::CancelOrQuit),
            Action::Cancel
        );
        state.status = RunStatus::Idle;
        assert_eq!(
            dispatch_input(&mut state, input::InputEvent::CancelOrQuit),
            Action::Quit
        );
    }

    #[test]
    fn resolve_credentials_prefers_saved_openai_key_over_codex_credentials() {
        let _env = EnvGuard::new("saved-openai-key");
        threadlane_auth::save_openai_api_key("sk-saved").unwrap();
        threadlane_auth::save_credentials(&threadlane_auth::OAuthTokens {
            access_token: "codex-token".into(),
            refresh_token: None,
            expires_in: None,
            id_token: None,
            account_id: Some("codex-account".into()),
        })
        .unwrap();
        env::set_var("CHATGPT_ACCOUNT_ID", "environment-account");

        let (api_key, account_id) = resolve_credentials();

        assert_eq!(api_key, "sk-saved");
        assert_eq!(account_id, None);
    }

    #[test]
    fn resolve_credentials_prefers_explicit_environment_credentials() {
        let _env = EnvGuard::new("explicit-openai-key");
        threadlane_auth::save_openai_api_key("sk-saved").unwrap();
        threadlane_auth::save_credentials(&threadlane_auth::OAuthTokens {
            access_token: "codex-token".into(),
            refresh_token: None,
            expires_in: None,
            id_token: None,
            account_id: Some("codex-account".into()),
        })
        .unwrap();
        env::set_var("OPENAI_API_KEY", "sk-explicit");
        env::set_var("CHATGPT_ACCOUNT_ID", "environment-account");

        assert_eq!(
            resolve_credentials(),
            ("sk-explicit".into(), Some("environment-account".into()))
        );
    }

    #[test]
    fn resolve_credentials_uses_codex_credentials_as_final_fallback() {
        let _env = EnvGuard::new("codex-fallback");
        threadlane_auth::save_credentials(&threadlane_auth::OAuthTokens {
            access_token: "codex-token".into(),
            refresh_token: None,
            expires_in: None,
            id_token: None,
            account_id: Some("codex-account".into()),
        })
        .unwrap();

        assert_eq!(
            resolve_credentials(),
            ("codex-token".into(), Some("codex-account".into()))
        );
    }
}
