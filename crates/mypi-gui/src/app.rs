use makepad_widgets::*;
use mypi_agent::{get_runtime, AgentEvent, CodingAgent, CodingAgentOptions};
use mypi_provider::auth;
use mypi_provider::openai::fetch_available_models;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

script_mod! {
    use mod.prelude.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root {
            main_window := Window {
                window.inner_size: vec2(1024, 768)
                body +: {
                    View {
                        width: Fill
                        height: Fill
                        flow: Down
                        spacing: 10
                        padding: 15

                        // Header Bar
                        header := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            align: Center
                            spacing: 10

                            title_label := Label {
                                text: "⚡ mypi coding agent"
                                draw_text.text_style.font_size: 16.0
                            }

                            api_key_label := Label {
                                text: "Key:"
                            }

                            api_key_input := TextInput {
                                width: 220
                                height: 32
                                empty_text: "sk-... (or click Login ChatGPT)"
                            }

                            model_label := Label {
                                text: "Model:"
                            }

                            model_drop := DropDown {
                                width: 170
                                height: 32
                                labels: [
                                    "gpt-5.4",
                                    "gpt-5.4-mini",
                                    "gpt-5.5",
                                    "gpt-5.6-luna",
                                    "gpt-5.6-sol",
                                    "gpt-5.6-terra",
                                    "gpt-5.3-codex-spark",
                                    "gpt-4o",
                                    "gpt-4o-mini"
                                ]
                            }

                            login_btn := Button {
                                width: 120
                                height: 32
                                text: "Login ChatGPT"
                            }

                            status_label := Label {
                                text: "Status: Ready"
                            }
                        }

                        // Chat Log View
                        log_view := View {
                            width: Fill
                            height: Fill
                            padding: 10

                            chat_text := TextInput {
                                width: Fill
                                height: Fill
                                is_read_only: true
                                is_multiline: true
                                empty_text: "Welcome to mypi! Enter a prompt or slash command (/plan, /todos, /model, /compact, /session, /tree, /fork, /clone)..."
                            }
                        }

                        // Control Input Bar
                        input_bar := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 10
                            align: Center

                            prompt_input := TextInput {
                                width: Fill
                                height: 44
                                empty_text: "Ask mypi to inspect files, edit code, or run commands (or type /command)..."
                            }

                            send_btn := Button {
                                width: 90
                                height: 44
                                text: "Send"
                            }
                        }
                    }
                }
            }
        }
    }
}

pub enum GuiAgentEvent {
    Agent(AgentEvent),
    DeviceCodePrompt { user_code: String, url: String },
    DeviceLoginSuccess,
    AvailableModelsLoaded(Vec<String>),
    CommandOutput(String),
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    pub ui: WidgetRef,
    #[rust]
    pub chat_history: String,
    #[rust]
    pub rx: Option<Arc<Mutex<Receiver<GuiAgentEvent>>>>,
    #[rust]
    pub agent: Option<Arc<tokio::sync::Mutex<CodingAgent>>>,
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let (std_tx, std_rx) = channel::<GuiAgentEvent>();
        self.rx = Some(Arc::new(Mutex::new(std_rx)));

        let mut key_opt = None;
        let mut account_id_opt = None;

        if let Some(creds) = auth::load_credentials() {
            self.ui
                .text_input(cx, ids!(api_key_input))
                .set_text(cx, &creds.access_token);
            self.ui
                .label(cx, ids!(status_label))
                .set_text(cx, "Status: Logged in via ChatGPT");
            self.append_chat(
                cx,
                &format!("ℹ️ Loaded saved credentials from {}\n", creds.source),
            );
            key_opt = Some(creds.access_token.clone());
            account_id_opt = creds.account_id.clone();
        }

        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let context = mypi_agent::ProjectContext::discover(&work_dir);

        if !context.context_files.is_empty() {
            self.append_chat(
                cx,
                &format!(
                    "📄 Discovered {} context file(s): {:?}\n",
                    context.context_files.len(),
                    context.context_files
                ),
            );
        }

        let api_key = key_opt
            .clone()
            .unwrap_or_else(|| std::env::var("OPENAI_API_KEY").unwrap_or_default());
        let agent_opts = CodingAgentOptions {
            api_key: api_key.clone(),
            account_id: account_id_opt.clone(),
            model: "gpt-5.4".to_string(),
            work_dir: work_dir.clone(),
            session_file: None,
            enable_plan_mode: false,
        };

        let coding_agent = CodingAgent::new(agent_opts);
        let ext_count = coding_agent.wasi_extensions.extensions.len();

        if ext_count > 0 {
            for (ext_name, ext) in &coding_agent.wasi_extensions.extensions {
                let cmd_names: Vec<String> = ext
                    .manifest
                    .commands
                    .iter()
                    .map(|c| format!("/{}", c.name))
                    .collect();
                self.append_chat(
                    cx,
                    &format!(
                        "🧩 Loaded WASI Extension `{}` ({})\n   Commands: {}\n",
                        ext_name,
                        ext.manifest.description,
                        cmd_names.join(", ")
                    ),
                );
            }
        } else {
            self.append_chat(
                cx,
                "🧩 No WASI extensions loaded (place packages in ./.mypi/extensions/<id>/)\n",
            );
        }

        self.agent = Some(Arc::new(tokio::sync::Mutex::new(coding_agent)));

        self.append_chat(
            cx,
            "💡 Built-in Slash Commands: /model, /compact, /session, /tree, /fork, /clone, /quit\n",
        );

        // Fetch models in background
        let std_tx_clone = std_tx.clone();
        get_runtime().spawn(async move {
            if !api_key.is_empty() {
                let models = fetch_available_models(&api_key, account_id_opt.as_deref()).await;
                let _ = std_tx_clone.send(GuiAgentEvent::AvailableModelsLoaded(models));
            }
        });
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.ui.button(cx, ids!(login_btn)).clicked(actions) {
            self.append_chat(cx, "\n🔑 Initiating ChatGPT Device Code Login...\n");
            self.ui
                .label(cx, ids!(status_label))
                .set_text(cx, "Status: Connecting to ChatGPT...");

            let (std_tx, std_rx) = channel::<GuiAgentEvent>();
            self.rx = Some(Arc::new(Mutex::new(std_rx)));

            let std_tx_clone = std_tx.clone();
            get_runtime().spawn(async move {
                match auth::start_device_login().await {
                    Ok(resp) => {
                        let _ = std_tx_clone.send(GuiAgentEvent::DeviceCodePrompt {
                            user_code: resp.user_code.clone(),
                            url: resp.verification_uri.clone(),
                        });

                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_secs(
                                resp.interval.max(3),
                            ))
                            .await;
                            match auth::poll_device_token(&resp.device_auth_id, &resp.user_code)
                                .await
                            {
                                Ok(_tokens) => {
                                    let _ = std_tx_clone.send(GuiAgentEvent::DeviceLoginSuccess);
                                    break;
                                }
                                Err(e) if e == "authorization_pending" || e.contains("pending") => {
                                    continue
                                }
                                Err(e) => {
                                    let _ = std_tx_clone.send(GuiAgentEvent::Agent(
                                        AgentEvent::AgentError { error: e },
                                    ));
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = std_tx_clone
                            .send(GuiAgentEvent::Agent(AgentEvent::AgentError { error: e }));
                    }
                }
            });
        }

        if self.ui.button(cx, ids!(send_btn)).clicked(actions) {
            let prompt_widget = self.ui.text_input(cx, ids!(prompt_input));
            let input_text = prompt_widget.text();

            if !input_text.trim().is_empty() {
                let api_key_widget = self.ui.text_input(cx, ids!(api_key_input));
                let mut api_key = api_key_widget.text();
                let mut account_id = None;

                if let Some(creds) = auth::load_credentials() {
                    if api_key.trim().is_empty() || api_key.trim() == creds.access_token {
                        api_key = creds.access_token;
                        account_id = creds.account_id;
                    }
                }

                if api_key.trim().is_empty() {
                    api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
                }

                if api_key.is_empty() {
                    self.append_chat(cx, "\n⚠️ Error: Please provide an OpenAI API key or click 'Login ChatGPT' to authenticate.\n");
                    return;
                }

                let selected_model = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
                let model_name = if selected_model.is_empty() {
                    "gpt-5.4".to_string()
                } else {
                    selected_model
                };

                let (std_tx, std_rx) = channel::<GuiAgentEvent>();
                self.rx = Some(Arc::new(Mutex::new(std_rx)));

                let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

                if self.agent.is_none() {
                    let agent_opts = CodingAgentOptions {
                        api_key,
                        account_id,
                        model: model_name,
                        work_dir,
                        session_file: None,
                        enable_plan_mode: false,
                    };
                    self.agent = Some(Arc::new(tokio::sync::Mutex::new(CodingAgent::new(
                        agent_opts,
                    ))));
                }

                let agent_arc = self.agent.as_ref().unwrap().clone();
                let input_str = input_text.to_string();
                prompt_widget.set_text(cx, "");

                if input_str.trim().starts_with('/') {
                    self.append_chat(cx, &format!("\n⌨️ Command: {}\n", input_str));
                } else {
                    self.append_chat(cx, &format!("\n👤 User: {}\n🤖 mypi: ", input_str));
                }
                self.ui
                    .label(cx, ids!(status_label))
                    .set_text(cx, "Status: Working...");

                let std_tx_clone = std_tx.clone();

                get_runtime().spawn(async move {
                    let mut agent_lock = agent_arc.lock().await;
                    let mut event_rx = agent_lock.subscribe();

                    let std_tx_event = std_tx_clone.clone();
                    tokio::spawn(async move {
                        while let Ok(evt) = event_rx.recv().await {
                            let _ = std_tx_event.send(GuiAgentEvent::Agent(evt));
                        }
                    });

                    if let Some(out) = agent_lock.handle_input(&input_str).await {
                        let _ = std_tx_clone.send(GuiAgentEvent::CommandOutput(out));
                    }
                });
            }
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.poll_agent_events(cx);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}

impl App {
    pub fn poll_agent_events(&mut self, cx: &mut Cx) {
        let mut events = Vec::new();
        if let Some(rx_arc) = &self.rx {
            if let Ok(rx) = rx_arc.lock() {
                while let Ok(evt) = rx.try_recv() {
                    events.push(evt);
                }
            }
        }

        for evt in events {
            match evt {
                GuiAgentEvent::CommandOutput(output) => {
                    self.append_chat(cx, &format!("💻 Output: {}\n", output));
                }
                GuiAgentEvent::AvailableModelsLoaded(models) => {
                    self.ui
                        .drop_down(cx, ids!(model_drop))
                        .set_labels(cx, models);
                }
                GuiAgentEvent::DeviceCodePrompt { user_code, url } => {
                    self.append_chat(cx, &format!("\n👉 Please open {url} in your browser and enter code: {}\n[Waiting for user authorization...]\n", user_code));
                    self.ui
                        .label(cx, ids!(status_label))
                        .set_text(cx, &format!("Enter code: {user_code}"));
                }
                GuiAgentEvent::DeviceLoginSuccess => {
                    let mut key_opt = None;
                    let mut acc_opt = None;
                    if let Some(creds) = auth::load_credentials() {
                        self.ui
                            .text_input(cx, ids!(api_key_input))
                            .set_text(cx, &creds.access_token);
                        key_opt = Some(creds.access_token.clone());
                        acc_opt = creds.account_id;
                    }
                    self.append_chat(cx, "\n✅ Successfully authenticated with ChatGPT!\n");
                    self.ui
                        .label(cx, ids!(status_label))
                        .set_text(cx, "Status: Logged in via ChatGPT");

                    if let Some(key) = key_opt {
                        let (std_tx, std_rx) = channel::<GuiAgentEvent>();
                        self.rx = Some(Arc::new(Mutex::new(std_rx)));
                        let std_tx_clone = std_tx.clone();

                        get_runtime().spawn(async move {
                            let models = fetch_available_models(&key, acc_opt.as_deref()).await;
                            let _ = std_tx_clone.send(GuiAgentEvent::AvailableModelsLoaded(models));
                        });
                    }
                }
                GuiAgentEvent::Agent(agent_event) => match agent_event {
                    AgentEvent::AgentStart => {
                        self.ui
                            .label(cx, ids!(status_label))
                            .set_text(cx, "Status: Agent Started");
                    }
                    AgentEvent::TurnStart { turn_number } => {
                        self.append_chat(cx, &format!("\n--- Turn {} ---\n", turn_number));
                    }
                    AgentEvent::MessageUpdate {
                        text_delta,
                        tool_call_name,
                        ..
                    } => {
                        if let Some(delta) = text_delta {
                            self.append_chat(cx, &delta);
                        }
                        if let Some(tool_name) = tool_call_name {
                            self.append_chat(
                                cx,
                                &format!("\n⚙️ [Requesting Tool: `{tool_name}`]\n"),
                            );
                        }
                    }
                    AgentEvent::MessageEnd { .. } => {}
                    AgentEvent::ToolExecutionStart {
                        name, arguments, ..
                    } => {
                        self.append_chat(
                            cx,
                            &format!("\n🛠️ [Executing Tool `{name}` args: {arguments}]\n"),
                        );
                    }
                    AgentEvent::ToolExecutionUpdate { partial_result, .. } => {
                        self.append_chat(cx, &partial_result);
                    }
                    AgentEvent::ToolExecutionEnd { name, result, .. } => {
                        self.append_chat(
                            cx,
                            &format!("✓ Tool `{name}` finished:\n```\n{}\n```\n", result.content),
                        );
                    }
                    AgentEvent::TurnEnd { .. } => {
                        self.ui
                            .label(cx, ids!(status_label))
                            .set_text(cx, "Status: Turn Completed");
                    }
                    AgentEvent::AgentEnd { .. } => {
                        self.ui
                            .label(cx, ids!(status_label))
                            .set_text(cx, "Status: Ready");
                    }
                    AgentEvent::AgentError { error } => {
                        self.append_chat(cx, &format!("\n❌ Agent Error: {error}\n"));
                        self.ui
                            .label(cx, ids!(status_label))
                            .set_text(cx, "Status: Error");
                    }
                    _ => {}
                },
            }
        }
    }

    pub fn append_chat(&mut self, cx: &mut Cx, text: &str) {
        self.chat_history.push_str(text);
        self.ui
            .text_input(cx, ids!(chat_text))
            .set_text(cx, &self.chat_history);
    }
}
