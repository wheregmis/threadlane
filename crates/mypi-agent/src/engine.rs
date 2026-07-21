use crate::session::Session;
use mypi_provider::openai::{OpenAIClient, StreamEvent};
use mypi_tools::{execute_tool, get_available_tools, get_codex_tools};
use std::sync::{Arc, OnceLock};
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, Mutex};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime")
    })
}

#[derive(Debug, Clone)]
pub enum AgentUIEvent {
    TokenStream(String),
    ToolExecuting { name: String, args: String },
    ToolCompleted { name: String, result: String },
    AgentFinished,
    AgentError(String),
    DeviceCodePrompt { user_code: String, url: String },
    DeviceLoginSuccess,
    AvailableModelsLoaded(Vec<String>),
}

pub struct AgentEngine {
    pub session: Arc<Mutex<Session>>,
    api_key: String,
    account_id: Option<String>,
}

impl AgentEngine {
    pub fn new(api_key: String, account_id: Option<String>, model: &str) -> Self {
        Self {
            session: Arc::new(Mutex::new(Session::new(model))),
            api_key,
            account_id,
        }
    }

    pub async fn run_turn(&self, prompt: &str, ui_tx: mpsc::Sender<AgentUIEvent>) {
        {
            let mut sess = self.session.lock().await;
            sess.add_user_message(prompt);
        }

        loop {
            let (api_payload, codex_payload) = {
                let sess = self.session.lock().await;
                (
                    sess.to_api_payload(get_available_tools()),
                    sess.to_codex_payload(get_codex_tools()),
                )
            };

            let (event_tx, mut event_rx) = mpsc::channel(100);
            let api_key = self.api_key.clone();
            let account_id = self.account_id.clone();

            let handle = get_runtime().spawn(async move {
                let client = OpenAIClient::new(api_key, account_id);
                client
                    .stream_chat_completion(api_payload, codex_payload, event_tx)
                    .await;
            });

            let mut current_turn_text = String::new();

            while let Some(evt) = event_rx.recv().await {
                match evt {
                    StreamEvent::ContentToken(token) => {
                        current_turn_text.push_str(&token);
                        let _ = ui_tx.send(AgentUIEvent::TokenStream(token)).await;
                    }
                    StreamEvent::ToolCallStart { name, .. } => {
                        let _ = ui_tx
                            .send(AgentUIEvent::TokenStream(format!(
                                "\n\n⚙️ [Requesting Tool: {name}]\n"
                            )))
                            .await;
                    }
                    StreamEvent::ToolCallArgsDelta { .. } => {}
                    StreamEvent::Finished { tool_calls } => {
                        handle.await.ok();

                        let mut sess = self.session.lock().await;

                        let text_opt = if current_turn_text.is_empty() {
                            None
                        } else {
                            Some(current_turn_text.clone())
                        };

                        let tool_calls_opt = if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls.clone())
                        };

                        sess.add_assistant_message(text_opt, tool_calls_opt);

                        if tool_calls.is_empty() {
                            let _ = ui_tx.send(AgentUIEvent::AgentFinished).await;
                            return;
                        }

                        for tc in tool_calls {
                            let tool_id = tc.id.clone();
                            let tool_name = tc.function.name.clone();
                            let tool_args = tc.function.arguments.clone();

                            let _ = ui_tx
                                .send(AgentUIEvent::ToolExecuting {
                                    name: tool_name.clone(),
                                    args: tool_args.clone(),
                                })
                                .await;

                            let result = execute_tool(&tool_name, &tool_args);

                            let _ = ui_tx
                                .send(AgentUIEvent::ToolCompleted {
                                    name: tool_name.clone(),
                                    result: result.clone(),
                                })
                                .await;

                            sess.add_tool_result(&tool_id, &tool_name, &result);
                        }

                        break;
                    }
                    StreamEvent::Error(err) => {
                        let _ = ui_tx.send(AgentUIEvent::AgentError(err)).await;
                        return;
                    }
                }
            }
        }
    }
}
