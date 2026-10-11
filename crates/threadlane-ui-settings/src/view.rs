use gpui::*;
use gpui_component::input::{InputEvent, InputState};

use threadlane_ui_kit::settings::{self as kit_settings, SettingsAction, SettingsGeneral, SettingsPage, SettingsUpdate};

use threadlane_acp::{AcpAgentRecord, AcpScope};
use threadlane_skills::SkillMetadata;
use threadlane_ui_state::next_event_batch;
use threadlane_ui_state::provider_auth::{self, ProviderAuthEvent};
use threadlane_ui_state::settings::SettingsEvent;
use threadlane_ui_state::AppState;
use threadlane_ui_state::{actions::AppAction, controller};
use threadlane_updater::{current_version, UpdateStatus};
use threadlane_wasi::packages::{ExtensionRecord, ExtensionScope};

// A project overlay and the global extension can share an ID. Key UI controls
// by the full durable inventory identity used by the settings service.
fn extension_row_id(record: &ExtensionRecord) -> String {
    format!("{:?}:{}:{:?}", record.scope(), record.id(), record.module_path())
}

fn update_controls(status: &UpdateStatus) -> (&'static str, &'static str, bool) {
    match status {
        UpdateStatus::Idle => ("Not checked yet", "Check for updates", false),
        UpdateStatus::UpToDate => ("Up to date", "Check for updates", false),
        UpdateStatus::Checking => ("Checking for updates…", "Checking…", true),
        UpdateStatus::Available(_) => ("Update available", "Download update", false),
        UpdateStatus::Downloading { .. } => ("Downloading update…", "Downloading…", true),
        UpdateStatus::ReadyToInstall { .. } => ("Ready to restart", "Restart to update", false),
        UpdateStatus::Installing => ("Installing update…", "Installing…", true),
        UpdateStatus::Error(_) => ("Update failed", "Retry update check", false),
    }
}

/// Provider auth state shown on the Providers page. Reading it hits disk
/// (`gh auth status` even spawns a subprocess), so it is snapshotted when
/// the page opens or an auth action runs instead of on every render frame.
#[derive(Clone, Debug, Default)]
struct ProvidersStatusSnapshot {
    github_status: Option<String>,
    gitlab_status: Option<String>,
    /// `(id, label)` pairs of connected Codex accounts; tokens are
    /// deliberately not retained here.
    codex_accounts: Vec<(String, String)>,
    active_codex_account_id: Option<String>,
    antigravity_connected: bool,
}

impl ProvidersStatusSnapshot {
    fn load() -> Self {
        Self {
            github_status: threadlane_auth::github_auth::get_github_auth_status(),
            gitlab_status: threadlane_auth::github_auth::get_gitlab_auth_status(),
            codex_accounts: threadlane_auth::openai_auth::load_all_codex_accounts()
                .into_iter()
                .filter(|account| threadlane_auth::openai_auth::is_own_source(&account.source))
                .map(|account| (account.id, account.label))
                .collect(),
            active_codex_account_id: threadlane_auth::openai_auth::get_active_codex_account()
                .map(|account| account.id),
            antigravity_connected: threadlane_auth::antigravity_auth::load_antigravity_credentials(
            )
            .is_some(),
        }
    }
}

pub struct SettingsView {
    model: Entity<AppState>,
    openai_input: Entity<InputState>,
    opencode_input: Entity<InputState>,
    github_input: Entity<InputState>,
    acp_name_input: Entity<InputState>,
    acp_command_input: Entity<InputState>,
    page: SettingsPage,
    install_globally: bool,
    extension_rows: Vec<ExtensionRecord>,
    skill_rows: Vec<SkillMetadata>,
    acp_rows: Vec<AcpAgentRecord>,
    acp_probe: threadlane_ui_state::settings::AcpProbeState,
    capability_status: Option<String>,
    auth_tx: tokio::sync::mpsc::UnboundedSender<ProviderAuthEvent>,
    settings_tx: tokio::sync::mpsc::UnboundedSender<SettingsEvent>,
    auth_message: Option<AuthStatusMessage>,
    providers_snapshot: Option<ProvidersStatusSnapshot>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, PartialEq)]
enum AuthStatusKind {
    Info,
    Success,
    Error,
}

#[derive(Clone)]
struct AuthStatusMessage {
    text: String,
    kind: AuthStatusKind,
}

impl AuthStatusMessage {
    fn new(text: impl Into<String>, kind: AuthStatusKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }

    /// Classifies a legacy free-form status string from `AppState` by content.
    fn from_legacy(text: String) -> Self {
        let lower = text.to_lowercase();
        let kind = if lower.contains("failed") || lower.contains("error") {
            AuthStatusKind::Error
        } else {
            AuthStatusKind::Info
        };
        Self { text, kind }
    }
}

impl SettingsView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (openai_key, opencode_key) = {
            let state = model.read(cx);
            (state.openai_key.clone(), state.opencode_key.clone())
        };

        let openai_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("sk-proj-...")
                .default_value(&openai_key)
                .masked(true)
        });
        let opencode_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("opencode-key-...")
                .default_value(&opencode_key)
                .masked(true)
        });
        let github_token = threadlane_auth::github_auth::load_github_credentials()
            .map(|c| c.token)
            .unwrap_or_default();
        let github_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("ghp_... / github_pat_...")
                .default_value(&github_token)
                .masked(true)
        });
        let acp_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Agent name"));
        let acp_command_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("npx -y @agentclientprotocol/claude-agent-acp")
        });

        let (auth_tx, mut auth_rx) = tokio::sync::mpsc::unbounded_channel();
        let auth_model = model.clone();
        cx.spawn(async move |this, cx| {
            while let Some(events) = next_event_batch(&mut auth_rx).await {
                let _ = this.update(cx, |this, cx| {
                    for event in events {
                        let credentials_changed = matches!(event, ProviderAuthEvent::Connected(_));
                        this.auth_message = Some(match event {
                            ProviderAuthEvent::Status(message) => {
                                AuthStatusMessage::new(message, AuthStatusKind::Info)
                            }
                            ProviderAuthEvent::Connected(message) => {
                                AuthStatusMessage::new(message, AuthStatusKind::Success)
                            }
                            ProviderAuthEvent::Error(message) => {
                                AuthStatusMessage::new(message, AuthStatusKind::Error)
                            }
                        });
                        if credentials_changed {
                            auth_model.update(cx, |state, cx| {
                                state.reconcile_selected_model();
                                cx.notify();
                            });
                            // OAuth/API credentials changed (e.g. Antigravity
                            // login): re-pull the live inventories whose
                            // contents depend on them. TTL-guarded, so unrelated
                            // connects are cheap.
                            let dynamic_model = auth_model.clone();
                            cx.spawn(async move |_this, cx| {
                                crate::refresh_antigravity_models_and_update(
                                    dynamic_model.clone(),
                                    cx,
                                )
                                .await;
                                crate::refresh_openai_models_and_update(dynamic_model, cx).await;
                            })
                            .detach();
                        }
                    }
                    if this.page == SettingsPage::Providers {
                        // Auth flows report completion through this pump; keep the
                        // Providers page snapshot current without re-reading
                        // credentials on unrelated frames.
                        this.refresh_providers_snapshot();
                    }
                    cx.notify();
                });
            }
        })
        .detach();

        let (settings_tx, mut settings_rx) = tokio::sync::mpsc::unbounded_channel();
        let observe_model = cx.observe(&model, |_this, _model, cx| cx.notify());
        let openai_model = model.clone();
        let save_openai = cx.subscribe_in(
            &openai_input,
            window,
            move |_this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let key = input.read(cx).value().to_string();
                    openai_model.update(cx, |state, cx| {
                        controller::dispatch(state, AppAction::SaveOpenAiKey(key));
                        cx.notify();
                    });
                    // The key just changed, so re-pull the live OpenAI list.
                    let openai_refresh = openai_model.clone();
                    cx.spawn(async move |_this, cx| {
                        crate::refresh_openai_models_and_update(openai_refresh, cx).await;
                    })
                    .detach();
                }
            },
        );
        let opencode_model = model.clone();
        let save_opencode = cx.subscribe_in(
            &opencode_input,
            window,
            move |_this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let key = input.read(cx).value().to_string();
                    opencode_model.update(cx, |state, cx| {
                        controller::dispatch(state, AppAction::SaveOpenCodeKey(key));
                        cx.notify();
                    });
                    // The key just changed, so re-pull the live Zen model list.
                    let discovery_model = opencode_model.clone();
                    cx.spawn(async move |_this, cx| {
                        crate::refresh_discovered_models_and_update(discovery_model, cx).await;
                    })
                    .detach();
                }
            },
        );
        let github_tx = auth_tx.clone();
        let save_github = cx.subscribe_in(
            &github_input,
            window,
            move |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let key = input.read(cx).value().to_string();
                    let tx = github_tx.clone();
                    if key.trim().is_empty() {
                        let _ = provider_auth::disconnect_github();
                    } else {
                        let _ = provider_auth::save_github_pat(&key, tx);
                    }
                    this.refresh_providers_snapshot();
                }
            },
        );
        cx.spawn(async move |this, cx| {
            while let Some(events) = next_event_batch(&mut settings_rx).await {
                let _ = this.update(cx, |this, cx| {
                    for event in events {
                        match event {
                            SettingsEvent::AcpRefreshed {
                                request_id,
                                records,
                            } => {
                                if this.acp_probe.is_current(request_id) {
                                    this.acp_rows = records;
                                }
                            }
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();

        Self {
            model,
            openai_input,
            opencode_input,
            github_input,
            acp_name_input,
            acp_command_input,
            page: SettingsPage::default(),
            install_globally: false,
            extension_rows: Vec::new(),
            skill_rows: Vec::new(),
            acp_rows: Vec::new(),
            acp_probe: Default::default(),
            capability_status: None,
            auth_tx,
            settings_tx,
            auth_message: None,
            providers_snapshot: None,
            _subscriptions: vec![observe_model, save_openai, save_opencode, save_github],
        }
    }

    /// Open the existing settings page for a static command-palette destination.
    pub fn open_search_destination(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(page) = threadlane_ui_kit::settings_search_page(id) else { return; };
        self.page = page;
        match self.page {
            SettingsPage::Providers => self.enter_providers_page(cx),
            SettingsPage::Skills => { self.capability_status = None; self.refresh_skills(cx); }
            SettingsPage::Extensions => { self.capability_status = None; self.refresh_extensions(cx); }
            SettingsPage::AcpAgents => { self.capability_status = None; self.refresh_acp(cx); }
            _ => {}
        }
        cx.notify();
    }

    fn active_project(&self, cx: &App) -> Option<std::path::PathBuf> {
        self.model.read(cx).active_work_dir.clone()
    }

    fn refresh_extensions(&mut self, cx: &mut Context<Self>) {
        self.extension_rows =
            threadlane_wasi::settings::discover_extensions(self.active_project(cx));
    }

    fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        let project = self.active_project(cx);
        self.skill_rows = threadlane_skills::settings::discover_skills(project.as_deref());
    }

    fn refresh_providers_snapshot(&mut self) {
        self.providers_snapshot = Some(ProvidersStatusSnapshot::load());
    }

    /// Entering Providers drops finished auth results, which go stale beside the
    /// freshly loaded status (a failed `gh` connect kept reporting failure after
    /// GitHub connected), but keeps an in-progress flow's instructions such as a
    /// device code.
    fn enter_providers_page(&mut self, cx: &mut Context<Self>) {
        if self
            .auth_message
            .as_ref()
            .is_some_and(|message| message.kind != AuthStatusKind::Info)
        {
            self.auth_message = None;
        }
        self.model.update(cx, |state, cx| {
            if state.auth_status_msg.take().is_some() {
                cx.notify();
            }
        });
        self.refresh_providers_snapshot();
    }

    fn refresh_acp(&mut self, cx: &mut Context<Self>) {
        self.load_acp(false, cx);
    }

    fn load_acp(&mut self, force: bool, cx: &mut Context<Self>) {
        let project = self.active_project(cx);
        if let Err(error) = threadlane_acp_engine::upgrade_acp_presets(project.as_deref()) {
            self.capability_status = Some(error);
        }
        let rows = threadlane_acp_engine::configured_acp_agents(project.clone());
        let configs = rows.iter().map(|row| row.config.clone()).collect();
        let Some(request_id) = self.acp_probe.begin(project.clone(), configs, force) else {
            return;
        };
        self.acp_rows = rows;
        self.model.update(cx, |state, cx| {
            state.reconcile_selected_model();
            cx.notify();
        });
        if let Err(error) = threadlane_ui_state::settings::probe_acp_agents(
            project,
            request_id,
            self.settings_tx.clone(),
        ) {
            self.capability_status = Some(error);
        }
        // Keep the shared model cache warm while the status probe runs: the
        // picker in any session serves these without spawning its own agent.
        // The cache TTL makes repeat visits cheap.
        let cache_model = self.model.clone();
        let cache_project = self.active_project(cx);
        cx.spawn(async move |_view, cx| {
            crate::refresh_acp_models_and_update(cache_model, cx, cache_project).await;
        })
        .detach();
    }

    fn render_navigation(&self, cx: &mut Context<Self>) -> Div {
        let owner = cx.entity().downgrade();
        kit_settings::settings_navigation(self.page, &SettingsPage::ALL, move |action, _, cx| {
            let _ = owner.update(cx, |this, cx| {
                match action {
                    SettingsAction::Page(page) => {
                        this.page = page;
                        match page {
                            SettingsPage::Providers => this.enter_providers_page(cx),
                            SettingsPage::Subagents => this.capability_status = None,
                            SettingsPage::Skills => { this.capability_status = None; this.refresh_skills(cx); }
                            SettingsPage::Extensions => { this.capability_status = None; this.refresh_extensions(cx); }
                            SettingsPage::AcpAgents => { this.capability_status = None; this.refresh_acp(cx); }
                            _ => {}
                        }
                    }
                    SettingsAction::Back => this.model.update(cx, |state, cx| { controller::dispatch(state, AppAction::CloseSettings); cx.notify(); }),
                    _ => {}
                }
                cx.notify();
            });
        }, cx)
    }

    fn render_subagents(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(project) = self.model.read(cx).active_work_dir.clone() else {
            return kit_settings::settings_agent(None, |_, _, _| {}, window, cx);
        };
        let preferences = threadlane_project::subagent_settings::load(&project);
        let available = threadlane_daemon::catalog::available_models_for_project(Some(&project));
        let model_id = preferences.fast_model.as_deref().unwrap_or_default();
        let presentation = kit_settings::SettingsAgent {
            model_label: preferences.fast_model.as_deref()
                .map(|id| threadlane_daemon::catalog::selection_label(id, &available))
                .unwrap_or_else(|| "Same as parent".into()),
            models: available.into_iter().map(|model| kit_settings::SettingsAgentModel {
                id: model.id,
                label: model.label,
                icon_path: Some(model.provider.icon_path().into()),
            }).collect(),
            effort: preferences.fast_reasoning_effort.map(|effort| {
                threadlane_provider::model_registry::effective_effort(model_id, effort, Some(&project))
            }),
            efforts: threadlane_daemon::catalog::supports_reasoning(model_id, Some(&project))
                .then(|| threadlane_daemon::catalog::efforts_for_model(model_id, Some(&project))),
            model: preferences.fast_model.clone(),
            mode: preferences.orchestrator_mode,
            error: self.capability_status.clone(),
        };
        let owner = cx.entity().downgrade();
        kit_settings::settings_agent(Some(&presentation), move |action, _, cx| {
            let _ = owner.update(cx, |this, cx| {
                if let kit_settings::SettingsAgentAction::Mode(mode) = action {
                    // Keep the composer's canonical no-op and in-flight-turn guards.
                    this.model.update(cx, |state, cx| {
                        controller::dispatch(state, AppAction::SelectOrchestratorMode(mode));
                        cx.notify();
                    });
                } else {
                    let mut settings = threadlane_project::subagent_settings::load(&project);
                    match action {
                        kit_settings::SettingsAgentAction::Model(model) => settings.fast_model = model,
                        kit_settings::SettingsAgentAction::Effort(effort) => settings.fast_reasoning_effort = effort,
                        kit_settings::SettingsAgentAction::Mode(_) => unreachable!(),
                    }
                    match threadlane_project::subagent_settings::save(&project, &settings) {
                        Ok(()) => {
                            this.capability_status = None;
                            this.model.update(cx, |state, cx| {
                                state.invalidate_capability_runtimes();
                                cx.notify();
                            });
                        }
                        Err(error) => this.capability_status = Some(format!("Couldn't save Agent & Fusion settings: {error}")),
                    }
                }
                cx.notify();
            });
        }, window, cx)
    }

    fn render_general(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.model.read(cx);
        let (status, action, busy) = update_controls(&state.update_status);
        let general = SettingsGeneral {
            version: current_version().to_string(),
            active_project: state.active_work_dir.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "No active project".into()),
            project_count: state.projects.len(),
            auto_address_reviews: state.auto_address_pr_reviews_enabled,
            update: cfg!(target_os = "macos").then(|| SettingsUpdate { status: status.into(), action: action.into(), busy }),
        };
        let owner = cx.entity().downgrade();
        let projects = state
            .projects
            .iter()
            .map(|project| kit_settings::SettingsProject {
                path: project.work_dir.clone(),
                name: project.name.clone(),
                active: state.active_work_dir.as_ref() == Some(&project.work_dir)
                    || project
                        .sessions
                        .iter()
                        .any(|session| state.active_session_id.as_ref() == Some(&session.id)),
                disabled_reason: state.project_removal_disabled_reason(&project.work_dir),
            })
            .collect();
        let general = kit_settings::settings_general(
            &general,
            move |action, window, cx| match action {
                SettingsAction::Update => {
                    window.dispatch_action(Box::new(crate::ActivateUpdate), cx)
                }
                SettingsAction::AutoAddressReviews(enabled) => {
                    let _ = owner.update(cx, |this, cx| {
                        let result = this.model.update(cx, |state, _| {
                            state.set_auto_address_pr_reviews_enabled(enabled)
                        });
                        if let Err(error) = result {
                            this.capability_status = Some(error);
                        }
                        cx.notify();
                    });
                }
                _ => {}
            },
            cx,
        );
        let owner = cx.entity().downgrade();
        let projects = kit_settings::settings_projects(
            projects,
            self.capability_status.clone(),
            move |path, window, cx| {
                use gpui_component::WindowExt;
                let owner = owner.clone();
                let name = owner
                    .upgrade()
                    .and_then(|view| {
                        view.read(cx)
                            .model
                            .read(cx)
                            .projects
                            .iter()
                            .find(|project| project.work_dir == path)
                            .map(|project| project.name.clone())
                    })
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let owner = owner.clone();
                    let path_to_remove = path.clone();
                    kit_settings::project_removal_dialog(alert, &name, &threadlane_ui_kit::display_path(&path))
                        .on_ok(move |_, _, cx| {
                            let _ = owner.update(cx, |this, cx| {
                                let result = this.model.update(cx, |state, cx| {
                                    let result = state.remove_project(&path_to_remove);
                                    cx.notify();
                                    result
                                });
                                this.capability_status = result
                                    .err()
                                    .map(|error| format!("Could not remove project: {error}"));
                                cx.notify();
                            });
                            true
                        },
                    )
                });
            },
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(general)
            .child(projects)
            .into_any_element()
    }

    fn render_appearance(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        kit_settings::settings_appearance(&threadlane_ui_theme::active_theme_name(cx), |action, _, cx| {
            if let SettingsAction::Theme(name) = action { threadlane_ui_theme::apply_theme(name, cx); }
        }, window, cx)
    }

    fn render_keybindings(&self, cx: &mut Context<Self>) -> AnyElement {
        kit_settings::settings_shortcuts(cx)
    }

    fn apply_provider(&mut self, action: kit_settings::SettingsProviderAction, cx: &mut Context<Self>) {
        use kit_settings::{SettingsProvider as Provider, SettingsProviderAction as Action};
        let activating_account = matches!(&action, Action::SetActiveAccount(_));
        match action {
            Action::Connect(provider) => {
                let result = match provider {
                    Provider::ChatGPT => provider_auth::start_chatgpt_login(self.auth_tx.clone()),
                    Provider::Antigravity => provider_auth::start_antigravity_login(self.auth_tx.clone()),
                    Provider::GitHub => provider_auth::connect_github_cli(self.auth_tx.clone()),
                    _ => return,
                };
                self.auth_message = Some(match result {
                    Ok(()) => AuthStatusMessage::new(match provider {
                        Provider::ChatGPT => "Starting ChatGPT sign-in…",
                        Provider::Antigravity => "Opening Google Antigravity sign-in…",
                        _ => "Connecting via GitHub CLI…",
                    }, AuthStatusKind::Info),
                    Err(error) => AuthStatusMessage::new(error, AuthStatusKind::Error),
                });
                self.refresh_providers_snapshot();
            }
            Action::Disconnect(provider) => {
                let result = match provider {
                    Provider::ChatGPT => threadlane_auth::openai_auth::remove_credentials(),
                    Provider::Antigravity => threadlane_auth::antigravity_auth::clear_antigravity_credentials(),
                    Provider::GitHub => provider_auth::disconnect_github(),
                    Provider::GitLab => provider_auth::disconnect_gitlab(),
                    _ => return,
                };
                let disconnected = result.is_ok();
                self.auth_message = Some(match result {
                    Ok(()) => AuthStatusMessage::new(match provider {
                        Provider::ChatGPT => "Disconnected ChatGPT.",
                        Provider::Antigravity => "Disconnected Google Antigravity.",
                        Provider::GitHub => "Disconnected GitHub.",
                        _ => "Disconnected GitLab.",
                    }, AuthStatusKind::Success),
                    Err(error) => AuthStatusMessage::new(format!("Failed to disconnect: {error}"), AuthStatusKind::Error),
                });
                if disconnected && matches!(provider, Provider::ChatGPT | Provider::Antigravity) {
                    self.model.update(cx, |state, cx| { state.reconcile_selected_model(); cx.notify(); });
                }
                self.refresh_providers_snapshot();
            }
            Action::TestConnection(provider) => match provider {
                Provider::ChatGPT => { let _ = provider_auth::test_openai_connection(None, self.auth_tx.clone()); }
                Provider::Antigravity => { let _ = provider_auth::test_antigravity_connection(self.auth_tx.clone()); }
                _ => return,
            },
            Action::SaveKey(provider) => match provider {
                Provider::GitHub => {
                    let value = self.github_input.read(cx).value().to_string();
                    if value.trim().is_empty() {
                        self.auth_message = Some(match provider_auth::disconnect_github() {
                            Ok(()) => AuthStatusMessage::new("Cleared GitHub token.", AuthStatusKind::Success),
                            Err(error) => AuthStatusMessage::new(format!("Failed to clear GitHub token: {error}"), AuthStatusKind::Error),
                        });
                    } else {
                        let _ = provider_auth::save_github_pat(&value, self.auth_tx.clone());
                    }
                    self.refresh_providers_snapshot();
                }
                Provider::OpenAI | Provider::OpenCode => {
                    let action = if provider == Provider::OpenAI {
                        AppAction::SaveOpenAiKey(self.openai_input.read(cx).value().to_string())
                    } else { AppAction::SaveOpenCodeKey(self.opencode_input.read(cx).value().to_string()) };
                    self.model.update(cx, |state, cx| { controller::dispatch(state, action); cx.notify(); });
                }
                _ => return,
            },
            Action::TestKey(provider) => match provider {
                Provider::OpenAI => { let _ = provider_auth::test_openai_connection(Some(self.openai_input.read(cx).value().to_string()), self.auth_tx.clone()); }
                Provider::OpenCode => { let _ = provider_auth::test_opencode_connection(&self.opencode_input.read(cx).value(), self.auth_tx.clone()); }
                _ => return,
            },
            Action::RefreshModels => {
                self.auth_message = Some(AuthStatusMessage::new(
                    "Refreshing provider model lists…",
                    AuthStatusKind::Info,
                ));
                let tx = self.auth_tx.clone();
                let model = self.model.clone();
                let project = self.active_project(cx);
                cx.spawn(async move |_, cx| {
                    crate::refresh_all_models_and_update(model, cx, project).await;
                    let _ = tx.send(provider_auth::ProviderAuthEvent::Status(
                        "Model lists refreshed.".into(),
                    ));
                })
                .detach();
            }
            Action::SetActiveAccount(id) | Action::RemoveAccount(id) => {
                // Both commands refresh the same inventory and live model catalog.
                let command = if activating_account {
                    AppAction::SetActiveCodexAccount(id)
                } else { AppAction::RemoveCodexAccount(id) };
                self.model.update(cx, |state, cx| { controller::dispatch(state, command); cx.notify(); });
                self.refresh_providers_snapshot();
                let model = self.model.clone();
                cx.spawn(async move |_, cx| { crate::refresh_openai_models_and_update(model, cx).await; }).detach();
            }
        }
        cx.notify();
    }

    fn render_providers(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        use kit_settings::{SettingsProviderAccount, SettingsProviders, SettingsProviderStatus, SettingsProviderStatusKind};
        let snapshot = self.providers_snapshot.as_ref();
        let active_id = snapshot.and_then(|snapshot| snapshot.active_codex_account_id.as_deref());
        let accounts = snapshot.map(|snapshot| snapshot.codex_accounts.iter().enumerate().map(|(ix, (id, label))| SettingsProviderAccount {
            id: id.clone(), label: label.clone(), active: active_id == Some(id.as_str()) || (active_id.is_none() && ix == 0),
        }).collect()).unwrap_or_default();
        let status = self.auth_message.clone().or_else(|| self.model.read(cx).auth_status_msg.clone().map(AuthStatusMessage::from_legacy))
            .map(|status| SettingsProviderStatus { text: status.text, kind: match status.kind {
                AuthStatusKind::Info => SettingsProviderStatusKind::Info,
                AuthStatusKind::Success => SettingsProviderStatusKind::Success,
                AuthStatusKind::Error => SettingsProviderStatusKind::Error,
            }});
        let providers = SettingsProviders { accounts, status,
            antigravity_connected: snapshot.is_some_and(|snapshot| snapshot.antigravity_connected),
            github_status: snapshot.and_then(|snapshot| snapshot.github_status.clone()),
            gitlab_status: snapshot.and_then(|snapshot| snapshot.gitlab_status.clone()),
        };
        let owner = cx.entity().downgrade();
        kit_settings::settings_providers(&providers, &self.github_input, &self.openai_input, &self.opencode_input,
            move |action, _, cx| { let _ = owner.update(cx, |this, cx| this.apply_provider(action, cx)); }, window, cx)
    }

    fn render_extensions(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_catalog(kit_settings::SettingsCatalogKind::Extensions, window, cx)
    }

    fn render_skills(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_catalog(kit_settings::SettingsCatalogKind::Skills, window, cx)
    }

    fn render_catalog(
        &self,
        kind: kit_settings::SettingsCatalogKind,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use kit_settings::{SettingsCatalogKind as Kind, SettingsCatalogStatus as Status};
        let has_project = self.active_project(cx).is_some();
        let rows = match kind {
            Kind::Skills => self
                .skill_rows
                .iter()
                .map(|skill| kit_settings::SettingsCatalogRow {
                    id: skill.id.clone(),
                    title: skill.name.clone(),
                    description: skill.description.clone(),
                    scope: skill.scope.display_name().into(),
                    enabled: skill.enabled,
                    status: if !skill.is_valid {
                        Status::Invalid
                    } else if skill.enabled {
                        Status::Enabled
                    } else {
                        Status::Disabled
                    },
                    disabled_reason: if !has_project {
                        Some("Attach a project to manage skills".into())
                    } else if !skill.is_valid {
                        Some("This skill is invalid".into())
                    } else {
                        None
                    },
                })
                .collect(),
            Kind::Extensions => self
                .extension_rows
                .iter()
                .map(|record| kit_settings::SettingsCatalogRow {
                    id: extension_row_id(record),
                    title: format!("{} · v{}", record.name(), record.version()),
                    description: record.module_path().display().to_string(),
                    scope: match record.scope() {
                        ExtensionScope::Global => "Global",
                        ExtensionScope::Project => "Project",
                    }
                    .into(),
                    enabled: record.is_enabled(),
                    status: if !record.is_enabled() {
                        Status::Disabled
                    } else if record.is_effective() {
                        Status::Active
                    } else {
                        Status::Overridden
                    },
                    disabled_reason: None,
                })
                .collect(),
        };
        let presentation = kit_settings::SettingsCatalog {
            kind,
            rows,
            has_project,
            install_globally: self.install_globally,
            status: self.capability_status.clone(),
        };
        let owner = cx.entity().downgrade();
        kit_settings::settings_catalog(
            &presentation,
            move |action, _, cx| {
                // File selection stays outside the entity update, as in the native host.
                let source = if action == kit_settings::SettingsCatalogAction::Install {
                    let Some(path) = rfd::FileDialog::new()
                        .set_title("Install a compiled WASI extension")
                        .add_filter("WebAssembly", &["wasm"])
                        .pick_file()
                    else {
                        return;
                    };
                    Some(path)
                } else {
                    None
                };
                let _ = owner.update(cx, |this, cx| this.apply_catalog(kind, action, source, cx));
            },
            window,
            cx,
        )
    }

    fn apply_catalog(
        &mut self,
        kind: kit_settings::SettingsCatalogKind,
        action: kit_settings::SettingsCatalogAction,
        source: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        use kit_settings::{SettingsCatalogAction as Action, SettingsCatalogKind as Kind};
        let mut changed = false;
        match action {
            Action::Scope(global) => {
                self.install_globally = global;
                cx.notify();
                return;
            }
            Action::Refresh => {}
            Action::Install if kind == Kind::Extensions => {
                let Some(source) = source else {
                    return;
                };
                let scope = if self.install_globally {
                    ExtensionScope::Global
                } else {
                    ExtensionScope::Project
                };
                self.capability_status = Some(
                    threadlane_wasi::settings::install_extension(
                        self.active_project(cx),
                        &source,
                        scope,
                    )
                    .unwrap_or_else(|error| error),
                );
                changed = true;
            }
            Action::DisableAll if kind == Kind::Skills => {
                let Some(project) = self.active_project(cx) else {
                    self.capability_status = Some("Attach a project to manage skills.".into());
                    cx.notify();
                    return;
                };
                self.capability_status = threadlane_skills::settings::disable_all_skills(
                    &project,
                    self.skill_rows.iter().map(|skill| skill.id.clone()),
                )
                .err();
                changed = true;
            }
            Action::Toggle { id, enabled } if kind == Kind::Skills => {
                let Some(project) = self.active_project(cx) else {
                    self.capability_status = Some("Attach a project to manage skills.".into());
                    cx.notify();
                    return;
                };
                if !self
                    .skill_rows
                    .iter()
                    .any(|skill| skill.id == id && skill.is_valid)
                {
                    return;
                }
                self.capability_status =
                    threadlane_skills::settings::set_skill_enabled(&project, &id, enabled).err();
                changed = true;
            }
            Action::Toggle { id, enabled } => {
                if let Some(record) = self
                    .extension_rows
                    .iter()
                    .find(|record| extension_row_id(record) == id)
                {
                    self.capability_status = threadlane_wasi::settings::set_extension_enabled(
                        self.active_project(cx),
                        record,
                        enabled,
                    )
                    .err();
                    changed = true;
                }
            }
            Action::Remove { id } if kind == Kind::Extensions => {
                if let Some(record) = self
                    .extension_rows
                    .iter()
                    .find(|record| extension_row_id(record) == id)
                {
                    self.capability_status = threadlane_wasi::settings::remove_extension(
                        self.active_project(cx),
                        record,
                    )
                    .err();
                    changed = true;
                }
            }
            _ => return,
        }
        match kind {
            Kind::Skills => self.refresh_skills(cx),
            Kind::Extensions => self.refresh_extensions(cx),
        }
        if changed {
            self.model.update(cx, |state, cx| {
                state.invalidate_capability_runtimes();
                cx.notify();
            });
        }
        cx.notify();
    }

    fn apply_external_agent(&mut self, action: kit_settings::SettingsExternalAgentAction, cx: &mut Context<Self>) {
        use kit_settings::SettingsExternalAgentAction as Action;
        let project = self.active_project(cx);
        let scope = |global| if global { AcpScope::Global } else { AcpScope::Project };
        let result = match action {
            Action::Scope(global) => { self.install_globally = global; cx.notify(); return; }
            Action::Refresh => { self.load_acp(true, cx); cx.notify(); return; }
            Action::Add { name, command } => threadlane_acp_engine::add_acp_agent(
                project.as_deref(), scope(self.install_globally), &name, &command),
            Action::Toggle { id, global, preset, enabled } => {
                if preset {
                    let Some(preset) = threadlane_acp_engine::ACP_PRESETS.iter().find(|preset| preset.id == id) else { return; };
                    threadlane_acp_engine::set_acp_preset_enabled(project.as_deref(), scope(global), preset, enabled)
                } else {
                    threadlane_acp_engine::set_acp_enabled(project.as_deref(), scope(global), &id, enabled)
                }
            }
            Action::Remove { id, global } => threadlane_acp_engine::remove_acp_agent(project.as_deref(), scope(global), &id),
        };
        self.capability_status = result.err();
        self.refresh_acp(cx);
        cx.notify();
    }

    fn render_acp_agents(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        use kit_settings::{SettingsExternalAgentRow, SettingsExternalAgents};
        let selected_scope = if self.install_globally { AcpScope::Global } else { AcpScope::Project };
        let mut rows: Vec<_> = threadlane_acp_engine::ACP_PRESETS.iter().map(|preset| {
            let configured = self.acp_rows.iter().find(|record| preset.matches_agent(&record.config) && record.config.scope == selected_scope);
            SettingsExternalAgentRow {
                id: preset.id.into(), name: preset.name.into(), description: preset.description.into(),
                status: configured.map(|record| record.status.display_status()).unwrap_or_else(|| "Not configured".into()),
                enabled: configured.is_some_and(|record| record.config.enabled),
                global: self.install_globally, preset: true,
                error: configured.is_some_and(|record| matches!(record.status, threadlane_acp::AcpAgentStatus::Error(_))),
            }
        }).collect();
        rows.extend(self.acp_rows.iter().filter(|record| !threadlane_acp_engine::ACP_PRESETS.iter().any(|preset| preset.matches_agent(&record.config))).map(|record| SettingsExternalAgentRow {
            id: record.config.id.clone(), name: record.config.name.clone(), description: record.config.command_line(),
            status: record.status.display_status(), enabled: record.config.enabled,
            global: record.config.scope == AcpScope::Global, preset: false,
            error: matches!(record.status, threadlane_acp::AcpAgentStatus::Error(_)),
        }));
        let agents = SettingsExternalAgents { rows, global: self.install_globally,
            has_project: self.active_project(cx).is_some(), status: self.capability_status.clone() };
        let owner = cx.entity().downgrade();
        kit_settings::settings_external_agents(&agents, &self.acp_name_input, &self.acp_command_input,
            move |action, _, cx| { let _ = owner.update(cx, |this, cx| this.apply_external_agent(action, cx)); }, window, cx)
    }

}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.page == SettingsPage::Providers && self.providers_snapshot.is_none() {
            self.refresh_providers_snapshot();
        }
        let content = match self.page {
            SettingsPage::General => self.render_general(cx),
            SettingsPage::Appearance => self.render_appearance(window, cx),
            SettingsPage::Keybindings => self.render_keybindings(cx),
            SettingsPage::Providers => self.render_providers(window, cx),
            SettingsPage::Subagents => self.render_subagents(window, cx),
            SettingsPage::Skills => self.render_skills(window, cx),
            SettingsPage::Extensions => self.render_extensions(window, cx),
            SettingsPage::AcpAgents => self.render_acp_agents(window, cx),
        };
        kit_settings::settings_screen(self.page, self.render_navigation(cx), content, cx)
    }
}

#[cfg(test)]
mod update_tests {
    use super::update_controls;
    use threadlane_updater::UpdateStatus;

    #[test]
    fn manual_update_check_is_available_before_and_after_a_check() {
        assert_eq!(
            update_controls(&UpdateStatus::Idle),
            ("Not checked yet", "Check for updates", false)
        );
        assert_eq!(
            update_controls(&UpdateStatus::UpToDate),
            ("Up to date", "Check for updates", false)
        );
        assert_eq!(
            update_controls(&UpdateStatus::Error("offline".into())),
            ("Update failed", "Retry update check", false)
        );
    }

    #[test]
    fn active_updates_cannot_be_interrupted_by_manual_checks() {
        for status in [
            UpdateStatus::Checking,
            UpdateStatus::Downloading {
                version: "1.2.3".into(),
                progress: 0.5,
            },
            UpdateStatus::Installing,
        ] {
            assert!(update_controls(&status).2);
        }
    }
}
