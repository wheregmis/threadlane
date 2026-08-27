use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::alert::{Alert, AlertVariant};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::switch::Switch;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};

use crate::app::{actions::AppAction, controller};
use crate::screens::next_event_batch;
use crate::services::provider_auth::{self, ProviderAuthEvent};
use crate::services::settings::{self, SettingsEvent};
use crate::state::AppState;
use threadlane_session::{
    AcpAgentRecord, AcpScope, ExtensionRecord, ExtensionScope, SkillMetadata,
};
use threadlane_updater::UpdateStatus;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsPage {
    #[default]
    General,
    Appearance,
    Keybindings,
    Providers,
    Subagents,
    Skills,
    Extensions,
    AcpAgents,
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
            antigravity_connected:
                threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some(),
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
    pub(crate) fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        let acp_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Claude Code"));
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
                    openai_model.update(cx, |state, _cx| {
                        controller::dispatch(state, AppAction::SaveOpenAiKey(key));
                    });
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
                    opencode_model.update(cx, |state, _cx| {
                        controller::dispatch(state, AppAction::SaveOpenCodeKey(key));
                    });
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
                            SettingsEvent::AcpRefreshed(records) => this.acp_rows = records,
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
            capability_status: None,
            auth_tx,
            settings_tx,
            auth_message: None,
            providers_snapshot: None,
            _subscriptions: vec![observe_model, save_openai, save_opencode, save_github],
        }
    }

    fn active_project(&self, cx: &App) -> Option<std::path::PathBuf> {
        self.model.read(cx).active_work_dir.clone()
    }

    fn refresh_extensions(&mut self, cx: &mut Context<Self>) {
        self.extension_rows = settings::discover_extensions(self.active_project(cx));
    }

    fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        let project = self.active_project(cx);
        self.skill_rows = settings::discover_skills(project.as_deref());
    }

    fn refresh_providers_snapshot(&mut self) {
        self.providers_snapshot = Some(ProvidersStatusSnapshot::load());
    }

    fn refresh_acp(&mut self, cx: &mut Context<Self>) {
        let project = self.active_project(cx);
        if let Err(error) = settings::upgrade_acp_presets(project.as_deref()) {
            self.capability_status = Some(error);
        }
        self.acp_rows = settings::configured_acp_agents(project.clone());
        self.model.update(cx, |state, cx| {
            state.reconcile_selected_model();
            cx.notify();
        });
        if let Err(error) = settings::probe_acp_agents(project, self.settings_tx.clone()) {
            self.capability_status = Some(error);
        }
    }

    /// Renders the muted "no items" placeholder shared by the extension,
    /// skill, and ACP agent lists.
    fn empty_state(message: &str, colors: gpui_component::ThemeColor) -> AnyElement {
        div()
            .p_6()
            .text_sm()
            .text_color(colors.muted_foreground)
            .child(message.to_string())
            .into_any_element()
    }

    fn render_navigation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();

        div()
            .w(px(240.0))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(div().h(px(48.0)).flex_none())
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child("SETTINGS"),
            )
            .child(
                div()
                    .px_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Button::new("settings-general")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::Settings)
                                    .child("General"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::General)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::General;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-appearance")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::Palette)
                                    .child("Appearance"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Appearance)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Appearance;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-keybindings")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::SquareTerminal)
                                    .child("Keybindings"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Keybindings)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Keybindings;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-providers")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::Bot)
                                    .child("Providers"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Providers)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Providers;
                                this.refresh_providers_snapshot();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-subagents")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::Bot)
                                    .child("Subagents"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Subagents)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Subagents;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-skills")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::BookOpen)
                                    .child("Skills"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Skills)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Skills;
                                this.capability_status = None;
                                this.refresh_skills(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-extensions")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::HardDrive)
                                    .child("WASI Extensions"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::Extensions)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::Extensions;
                                this.capability_status = None;
                                this.refresh_extensions(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-acp")
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_start()
                                    .gap_2()
                                    .child(IconName::Network)
                                    .child("ACP Agents"),
                            )
                            .ghost()
                            .selected(self.page == SettingsPage::AcpAgents)
                            .w_full()
                            .justify_start()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.page = SettingsPage::AcpAgents;
                                this.capability_status = None;
                                this.refresh_acp(cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex_1())
            .child(
                div().flex_none().px_3().py_2().child(
                    Button::new("settings-back")
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_start()
                                .gap_2()
                                .child(IconName::ArrowLeft)
                                .child("Back"),
                        )
                        .ghost()
                        .w_full()
                        .justify_start()
                        .text_color(theme.muted_foreground)
                        .on_click(move |_event, _window, cx| {
                            model.update(cx, |state, _cx| {
                                controller::dispatch(state, AppAction::CloseSettings);
                            });
                        }),
                ),
            )
    }

    fn render_subagents(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let state = self.model.read(cx);
        let Some(project) = state.active_work_dir.clone() else {
            return Self::empty_state("Attach a project to configure subagents.", theme);
        };
        let preferences = crate::services::subagent_settings::load(&project);
        let selected_model = preferences.model.clone();
        let selected_reasoning = preferences.reasoning_effort;
        let model_label = selected_model
            .as_deref()
            .and_then(crate::model_catalog::label_for)
            .unwrap_or_else(|| "Same as parent".into());
        let reasoning_label = selected_reasoning
            .map(|effort| effort.label())
            .unwrap_or("Same as parent");
        let available = crate::model_catalog::available_models_for_project(Some(&project));
        let model_entity = self.model.clone();
        let project_for_models = project.clone();
        let model_picker = Button::new("subagent-model-picker")
            .label(model_label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let model_entity_for_parent = model_entity.clone();
                let project_for_parent = project_for_models.clone();
                available.iter().cloned().fold(
                    menu.item(
                        PopupMenuItem::new("Same as parent").on_click(move |_, _, cx| {
                            let mut settings =
                                crate::services::subagent_settings::load(&project_for_parent);
                            settings.model = None;
                            if crate::services::subagent_settings::save(
                                &project_for_parent,
                                &settings,
                            )
                            .is_ok()
                            {
                                model_entity_for_parent.update(cx, |state, cx| {
                                    state.invalidate_capability_runtimes();
                                    cx.notify();
                                });
                            }
                        }),
                    ),
                    |menu, option| {
                        let model_entity = model_entity.clone();
                        let project = project_for_models.clone();
                        menu.item(
                            PopupMenuItem::new(option.label)
                                .icon(Icon::default().path(option.provider.icon_path()))
                                .on_click(move |_, _, cx| {
                                    let mut settings =
                                        crate::services::subagent_settings::load(&project);
                                    settings.model = Some(option.id.clone());
                                    if crate::services::subagent_settings::save(&project, &settings)
                                        .is_ok()
                                    {
                                        model_entity.update(cx, |state, cx| {
                                            state.invalidate_capability_runtimes();
                                            cx.notify();
                                        });
                                    }
                                }),
                        )
                    },
                )
            });
        let reasoning_entity = self.model.clone();
        let project_for_reasoning = project.clone();
        let reasoning_picker = Button::new("subagent-reasoning-picker")
            .label(reasoning_label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let entity = reasoning_entity.clone();
                let project = project_for_reasoning.clone();
                [
                    None,
                    Some(threadlane_runtime::ReasoningEffort::Minimal),
                    Some(threadlane_runtime::ReasoningEffort::Low),
                    Some(threadlane_runtime::ReasoningEffort::Medium),
                    Some(threadlane_runtime::ReasoningEffort::High),
                ]
                .into_iter()
                .fold(menu, |menu, effort| {
                    let entity = entity.clone();
                    let project = project.clone();
                    menu.item(
                        PopupMenuItem::new(
                            effort
                                .map(|value| value.label())
                                .unwrap_or("Same as parent"),
                        )
                        .on_click(move |_, _, cx| {
                            let mut settings = crate::services::subagent_settings::load(&project);
                            settings.reasoning_effort = effort;
                            if crate::services::subagent_settings::save(&project, &settings).is_ok()
                            {
                                entity.update(cx, |state, cx| {
                                    state.invalidate_capability_runtimes();
                                    cx.notify();
                                });
                            }
                        }),
                    )
                })
            });
        let row = |title: &'static str, description: &'static str, control: AnyElement| {
            div()
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .p_4()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(description),
                        ),
                )
                .child(control)
        };
        div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(row(
                "Model",
                "Default model for every delegated child.",
                model_picker.into_any_element(),
            ))
            .child(row(
                "Reasoning effort",
                "Default reasoning effort for every delegated child.",
                reasoning_picker.into_any_element(),
            ))
            .into_any_element()
    }

    fn render_general(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let state = self.model.read(cx);
        let active_project = state
            .active_work_dir
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "No active project".to_string());
        let project_count = state.projects.len();
        let needle_enabled = state.needle_enabled;
        let toggle_view = cx.entity().downgrade();
        let update_status_label = match &state.update_status {
            UpdateStatus::Checking => "Checking for updates...",
            UpdateStatus::Available(_) => "Update available",
            UpdateStatus::Downloading { .. } => "Downloading update...",
            UpdateStatus::ReadyToInstall { .. } => "Ready to restart",
            UpdateStatus::Installing => "Installing update...",
            UpdateStatus::Error(_) => "Update check failed",
            _ => "Up to date",
        };

        div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .size(px(36.0))
                                            .rounded_lg()
                                            .bg(theme.muted)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(theme.foreground)
                                            .child(IconName::Settings),
                                    )
                                    .child(
                                        div()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme.foreground)
                                                    .child("Application Details"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child("Threadlane GPUI Desktop Native Engine"),
                                            ),
                                    ),
                            )
                            .child(
                                Tag::new()
                                    .child(format!("v{}", env!("CARGO_PKG_VERSION")))
                                    .with_variant(TagVariant::Primary)
                                    .small(),
                            ),
                    )
                    .child(
                        div()
                            .grid()
                            .grid_cols(2)
                            .gap_3()
                            .pt_2()
                            .border_t_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Active Workspace")
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(active_project),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Attached Projects")
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(format!("{project_count} projects")),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .p_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child("Application Updates"),
                                    )
                                    .child(
                                        Tag::new()
                                            .child(update_status_label)
                                            .with_variant(TagVariant::Secondary)
                                            .small(),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Signed native desktop application release channel."),
                            ),
                    ),
            )
            .child(
                div()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .p_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child("Local Needle Indexing"),
                                    )
                                    .child(
                                        Tag::new()
                                            .child(if needle_enabled { "Enabled" } else { "Disabled" })
                                            .with_variant(if needle_enabled {
                                                TagVariant::Success
                                            } else {
                                                TagVariant::Secondary
                                            })
                                            .small(),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Accelerate file search and symbol extraction using the native local index."),
                            ),
                    )
                    .child(
                        Switch::new("general-needle-switch")
                            .checked(needle_enabled)
                            .tooltip(if needle_enabled {
                                "Disable Needle routing"
                            } else {
                                "Enable Needle routing"
                            })
                            .on_click(move |checked, _window, cx| {
                                let _ = toggle_view.update(cx, |this, cx| {
                                    let result = this.model.update(cx, |state, _cx| {
                                        state.set_needle_enabled(*checked)
                                    });
                                    if let Err(error) = result {
                                        this.capability_status = Some(error);
                                    }
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let active_theme = crate::theme::active_theme_name(cx);
        let is_dark = active_theme == "Threadlane Dark";
        let is_light = active_theme == "Threadlane Light";

        div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap_4()
                    .child(
                        div()
                            .id("theme-card-dark")
                            .p_4()
                            .rounded_xl()
                            .border_2()
                            .border_color(if is_dark { theme.primary } else { theme.border })
                            .bg(theme.title_bar)
                            .cursor_pointer()
                            .on_click(|_event, _window, cx| {
                                crate::theme::apply_theme("Threadlane Dark", cx);
                            })
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .h(px(80.0))
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(hsla(0.0, 0.0, 0.07, 1.0))
                                    .p_3()
                                    .flex()
                                    .flex_col()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .gap_1_5()
                                            .child(
                                                div()
                                                    .size(px(8.0))
                                                    .rounded_full()
                                                    .bg(hsla(0.0, 0.7, 0.6, 1.0)),
                                            )
                                            .child(
                                                div()
                                                    .size(px(8.0))
                                                    .rounded_full()
                                                    .bg(hsla(0.12, 0.7, 0.6, 1.0)),
                                            )
                                            .child(
                                                div()
                                                    .size(px(8.0))
                                                    .rounded_full()
                                                    .bg(hsla(0.35, 0.7, 0.6, 1.0)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .h(px(16.0))
                                            .w_3_4()
                                            .rounded_md()
                                            .bg(hsla(0.0, 0.0, 0.16, 1.0)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme.foreground)
                                                    .child("Threadlane Dark"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child("High-contrast deep black interface"),
                                            ),
                                    )
                                    .children(is_dark.then(|| {
                                        Tag::new()
                                            .child("Active")
                                            .with_variant(TagVariant::Success)
                                            .small()
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id("theme-card-light")
                            .p_4()
                            .rounded_xl()
                            .border_2()
                            .border_color(if is_light {
                                theme.primary
                            } else {
                                theme.border
                            })
                            .bg(theme.title_bar)
                            .cursor_pointer()
                            .on_click(|_event, _window, cx| {
                                crate::theme::apply_theme("Threadlane Light", cx);
                            })
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .h(px(80.0))
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(hsla(0.0, 0.0, 0.98, 1.0))
                                    .p_3()
                                    .flex()
                                    .flex_col()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .gap_1_5()
                                            .child(
                                                div()
                                                    .size(px(8.0))
                                                    .rounded_full()
                                                    .bg(hsla(0.0, 0.7, 0.6, 1.0)),
                                            )
                                            .child(
                                                div()
                                                    .size(px(8.0))
                                                    .rounded_full()
                                                    .bg(hsla(0.12, 0.7, 0.6, 1.0)),
                                            )
                                            .child(
                                                div()
                                                    .size(px(8.0))
                                                    .rounded_full()
                                                    .bg(hsla(0.35, 0.7, 0.6, 1.0)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .h(px(16.0))
                                            .w_3_4()
                                            .rounded_md()
                                            .bg(hsla(0.0, 0.0, 0.88, 1.0)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme.foreground)
                                                    .child("Threadlane Light"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child("Clean and crisp light aesthetic"),
                                            ),
                                    )
                                    .children(is_light.then(|| {
                                        Tag::new()
                                            .child("Active")
                                            .with_variant(TagVariant::Success)
                                            .small()
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_keybindings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;

        let shortcuts = [
            (
                "Global",
                vec![
                    ("⌘ ,", "Open Settings"),
                    ("⌘ B", "Toggle Left Sidebar"),
                    ("⌘ R", "Toggle Right Panel"),
                    ("⌘ J", "Toggle Terminal Panel"),
                    ("⌘ E", "Toggle Code/Diff Editor"),
                    ("⌘ N", "New Session"),
                    ("⌘ P", "Open Project File Finder"),
                    ("⌘ ⇧ O", "Attach Local Project"),
                ],
            ),
            (
                "Composer & Chat",
                vec![
                    ("Enter", "Submit prompt to agent"),
                    ("⇧ Enter", "Insert newline in composer"),
                    ("Escape", "Cancel active agent turn"),
                    ("/ (in empty composer)", "Open Slash Commands palette"),
                    ("@ (in composer)", "Reference file or context in prompt"),
                ],
            ),
            (
                "Editor & Diff",
                vec![
                    ("⌘ S", "Save active file"),
                    ("⌘ Z", "Undo edit"),
                    ("⌘ ⇧ Z", "Redo edit"),
                    ("⌘ F", "Find in active editor buffer"),
                ],
            ),
        ];

        let mut list = div().mt_5().flex().flex_col().gap_6();

        for (section_title, items) in shortcuts {
            let mut section_div = div()
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .bg(theme.title_bar)
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .pb_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child(section_title.to_uppercase()),
                );

            for (keys, description) in items {
                section_div = section_div.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .py_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.foreground)
                                .child(description),
                        )
                        .child(
                            Tag::new()
                                .child(keys)
                                .with_variant(TagVariant::Secondary)
                                .small(),
                        ),
                );
            }

            list = list.child(section_div);
        }

        list.into_any_element()
    }

    fn render_provider_connection(
        &self,
        title: &'static str,
        description: &'static str,
        connected: bool,
        antigravity: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let view = cx.entity().downgrade();
        let model = self.model.clone();
        let button_label = if connected {
            "Disconnect"
        } else if antigravity {
            "Sign in with Google"
        } else {
            "Sign in with ChatGPT"
        };

        div()
            .py_4()
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .w(px(36.0))
                    .h(px(36.0))
                    .flex_none()
                    .rounded_lg()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(if connected {
                        theme.success
                    } else {
                        theme.muted_foreground
                    })
                    .child(IconName::Bot),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(
                                Tag::new()
                                    .child(if connected { "Connected" } else { "Not connected" })
                                    .with_variant(if connected {
                                        TagVariant::Success
                                    } else {
                                        TagVariant::Secondary
                                    })
                                    .small(),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(description),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(connected.then(|| {
                        let auth_tx = self.auth_tx.clone();
                        Button::new(if antigravity {
                            "test-antigravity-connection-btn"
                        } else {
                            "test-chatgpt-connection-btn"
                        })
                        .icon(IconName::Play)
                        .label("Test")
                        .outline()
                        .on_click(move |_event, _window, _cx| {
                            if antigravity {
                                let _ = provider_auth::test_antigravity_connection(auth_tx.clone());
                            } else {
                                let _ = provider_auth::test_openai_connection(None, auth_tx.clone());
                            }
                        })
                    }))
                    .child(
                        Button::new(if antigravity {
                            "antigravity-auth-button"
                        } else {
                            "chatgpt-auth-button"
                        })
                        .label(button_label)
                        .when(!connected, |button| button.primary())
                        .when(connected, |button| button.ghost())
                        .on_click(move |_event, _window, cx| {
                            let _ = view.update(cx, |this, cx| {
                                if connected {
                                    let result = if antigravity {
                                        threadlane_provider::antigravity_auth::clear_antigravity_credentials()
                                    } else {
                                        threadlane_auth::openai_auth::remove_credentials()
                                    };
                                    let disconnected = result.is_ok();
                                    this.auth_message = Some(match result {
                                        Ok(()) if antigravity => AuthStatusMessage::new(
                                            "Disconnected Google Antigravity.",
                                            AuthStatusKind::Success,
                                        ),
                                        Ok(()) => AuthStatusMessage::new(
                                            "Disconnected ChatGPT.",
                                            AuthStatusKind::Success,
                                        ),
                                        Err(error) => AuthStatusMessage::new(
                                            format!("Failed to disconnect: {error}"),
                                            AuthStatusKind::Error,
                                        ),
                                    });
                                    if disconnected {
                                        model.update(cx, |state, cx| {
                                            state.reconcile_selected_model();
                                            cx.notify();
                                        });
                                    }
                                } else {
                                    let result = if antigravity {
                                        provider_auth::start_antigravity_login(this.auth_tx.clone())
                                    } else {
                                        provider_auth::start_chatgpt_login(this.auth_tx.clone())
                                    };
                                    this.auth_message = Some(match result {
                                        Ok(()) if antigravity => AuthStatusMessage::new(
                                            "Opening Google Antigravity sign-in...",
                                            AuthStatusKind::Info,
                                        ),
                                        Ok(()) => AuthStatusMessage::new(
                                            "Starting ChatGPT sign-in...",
                                            AuthStatusKind::Info,
                                        ),
                                        Err(error) => {
                                            AuthStatusMessage::new(error, AuthStatusKind::Error)
                                        }
                                    });
                                }
                                cx.notify();
                            });
                        }),
                    ),
            )
    }

    fn render_github_connection(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let view = cx.entity().downgrade();
        let github_status = self
            .providers_snapshot
            .as_ref()
            .map(|snapshot| snapshot.github_status.clone())
            .unwrap_or_else(threadlane_auth::github_auth::get_github_auth_status);
        let connected = github_status.is_some();
        let status_label = github_status.unwrap_or_else(|| "Not connected".to_string());
        let auth_tx = self.auth_tx.clone();

        div()
            .py_4()
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .w(px(36.0))
                    .h(px(36.0))
                    .flex_none()
                    .rounded_lg()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(if connected {
                        theme.success
                    } else {
                        theme.muted_foreground
                    })
                    .child(IconName::Globe),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("GitHub"),
                            )
                            .child(
                                Tag::new()
                                    .child(if connected {
                                        status_label
                                    } else {
                                        "Not connected".to_string()
                                    })
                                    .with_variant(if connected {
                                        TagVariant::Success
                                    } else {
                                        TagVariant::Secondary
                                    })
                                    .small(),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                            "Connect GitHub to inspect pr:// and issue:// virtual file references.",
                        ),
                    ),
            )
            .child(
                Button::new("github-auth-button")
                    .label(if connected {
                        "Disconnect"
                    } else {
                        "Connect via gh CLI"
                    })
                    .when(!connected, |button| button.primary())
                    .when(connected, |button| button.ghost())
                    .on_click(move |_event, _window, cx| {
                        let tx = auth_tx.clone();
                        let _ = view.update(cx, |this, cx| {
                            if connected {
                                let result = provider_auth::disconnect_github();
                                this.auth_message = Some(match result {
                                    Ok(()) => AuthStatusMessage::new(
                                        "Disconnected GitHub.",
                                        AuthStatusKind::Success,
                                    ),
                                    Err(err) => AuthStatusMessage::new(
                                        format!("Failed to disconnect GitHub: {err}"),
                                        AuthStatusKind::Error,
                                    ),
                                });
                            } else {
                                let result = provider_auth::connect_github_cli(tx);
                                if let Err(err) = result {
                                    this.auth_message = Some(AuthStatusMessage::new(
                                        format!("GitHub CLI connection: {err}"),
                                        AuthStatusKind::Error,
                                    ));
                                }
                            }
                            this.refresh_providers_snapshot();
                            cx.notify();
                        });
                    }),
            )
    }

    fn render_github_pat_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let view = cx.entity().downgrade();
        let input = self.github_input.clone();
        let auth_tx = self.auth_tx.clone();

        div()
            .py_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("GitHub Personal Access Token (PAT)"),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&input).mask_toggle()))
                    .child(
                        Button::new("save-github-token")
                            .label("Save")
                            .primary()
                            .on_click(move |_event, _window, cx| {
                                let val = input.read(cx).value().to_string();
                                let tx = auth_tx.clone();
                                let _ = view.update(cx, |this, cx| {
                                    if val.trim().is_empty() {
                                        let _ = provider_auth::disconnect_github();
                                        this.auth_message = Some(AuthStatusMessage::new(
                                            "Cleared GitHub token.",
                                            AuthStatusKind::Success,
                                        ));
                                    } else {
                                        let _ = provider_auth::save_github_pat(&val, tx);
                                    }
                                    this.refresh_providers_snapshot();
                                    cx.notify();
                                });
                            }),
                    ),
            )
    }

    fn render_gitlab_connection(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let view = cx.entity().downgrade();
        let gitlab_status = self
            .providers_snapshot
            .as_ref()
            .map(|snapshot| snapshot.gitlab_status.clone())
            .unwrap_or_else(threadlane_auth::github_auth::get_gitlab_auth_status);
        let connected = gitlab_status.is_some();
        let status_label = gitlab_status.unwrap_or_else(|| "Not connected".to_string());

        div()
            .py_4()
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .w(px(36.0))
                    .h(px(36.0))
                    .flex_none()
                    .rounded_lg()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(if connected {
                        theme.success
                    } else {
                        theme.muted_foreground
                    })
                    .child(IconName::Globe),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child("GitLab"),
                            )
                            .child(
                                Tag::new()
                                    .child(if connected {
                                        status_label
                                    } else {
                                        "Not connected".to_string()
                                    })
                                    .with_variant(if connected {
                                        TagVariant::Success
                                    } else {
                                        TagVariant::Secondary
                                    })
                                    .small(),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                            "Connect GitLab to inspect mr:// and GitLab issue virtual references.",
                        ),
                    ),
            )
            .child(
                Button::new("gitlab-auth-button")
                    .label("Disconnect")
                    .disabled(!connected)
                    .ghost()
                    .on_click(move |_event, _window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            if connected {
                                let result = provider_auth::disconnect_gitlab();
                                this.auth_message = Some(match result {
                                    Ok(()) => AuthStatusMessage::new(
                                        "Disconnected GitLab.",
                                        AuthStatusKind::Success,
                                    ),
                                    Err(err) => AuthStatusMessage::new(
                                        format!("Failed to disconnect GitLab: {err}"),
                                        AuthStatusKind::Error,
                                    ),
                                });
                                this.refresh_providers_snapshot();
                            }
                            cx.notify();
                        });
                    }),
            )
    }

    fn render_chatgpt_connections(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let view = cx.entity().downgrade();
        let model = self.model.clone();
        let (own_accounts, active_account_id) = match self.providers_snapshot.as_ref() {
            Some(snapshot) => (
                snapshot.codex_accounts.clone(),
                snapshot.active_codex_account_id.clone(),
            ),
            None => {
                let accounts = threadlane_auth::openai_auth::load_all_codex_accounts()
                    .into_iter()
                    .filter(|a| threadlane_auth::openai_auth::is_own_source(&a.source))
                    .map(|a| (a.id, a.label))
                    .collect::<Vec<_>>();
                (
                    accounts,
                    threadlane_auth::openai_auth::get_active_codex_account().map(|a| a.id),
                )
            }
        };

        if own_accounts.is_empty() {
            return self
                .render_provider_connection(
                    "OpenAI / ChatGPT",
                    "GPT and Codex models via ChatGPT device login or an API key.",
                    false,
                    false,
                    cx,
                )
                .into_any_element();
        }

        let auth_tx = self.auth_tx.clone();
        let count = own_accounts.len();

        div()
            .py_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .child(
                                div()
                                    .w(px(36.0))
                                    .h(px(36.0))
                                    .flex_none()
                                    .rounded_lg()
                                    .bg(theme.muted)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(theme.success)
                                    .child(IconName::Bot),
                            )
                            .child(
                                div()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme.foreground)
                                                    .child("OpenAI / ChatGPT"),
                                            )
                                            .child(
                                                Tag::new()
                                                    .child(if count == 1 {
                                                        "1 Account".to_string()
                                                    } else {
                                                        format!("{count} Accounts Connected")
                                                    })
                                                    .with_variant(TagVariant::Success)
                                                    .small(),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(
                                                "Manage connected accounts with automatic rate-limit and quota failover.",
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child({
                                let auth_tx = auth_tx.clone();
                                Button::new("test-chatgpt-accounts-btn")
                                    .icon(IconName::Play)
                                    .label("Test")
                                    .outline()
                                    .on_click(move |_event, _window, _cx| {
                                        let _ = provider_auth::test_openai_connection(None, auth_tx.clone());
                                    })
                            })
                            .child(
                                Button::new("add-chatgpt-account-btn")
                                    .icon(IconName::Plus)
                                    .label("Add Account")
                                    .outline()
                                    .on_click({
                                        let view = view.clone();
                                        let auth_tx = auth_tx.clone();
                                        move |_event, _window, cx| {
                                            let _ = view.update(cx, |this, cx| {
                                                let result =
                                                    provider_auth::start_chatgpt_login(auth_tx.clone());
                                                this.auth_message = Some(match result {
                                                    Ok(()) => AuthStatusMessage::new(
                                                        "Starting sign-in for additional account...",
                                                        AuthStatusKind::Info,
                                                    ),
                                                    Err(error) => AuthStatusMessage::new(
                                                        error,
                                                        AuthStatusKind::Error,
                                                    ),
                                                });
                                                cx.notify();
                                            });
                                        }
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .mt_3()
                    .pl(px(52.0))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(own_accounts.into_iter().enumerate().map(|(idx, (acc_id, acc_label))| {
                        let is_active = active_account_id.as_deref() == Some(&acc_id)
                            || (active_account_id.is_none() && idx == 0);
                        let acc_id_make_active = acc_id.clone();
                        let acc_id_remove = acc_id.clone();
                        let model_active = model.clone();
                        let model_remove = model.clone();
                        let view_active = view.clone();
                        let view_remove = view.clone();

                        let initial = acc_label
                            .chars()
                            .next()
                            .map(|c| c.to_uppercase().to_string())
                            .unwrap_or_else(|| "U".to_string());

                        div()
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.muted)
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .w(px(28.0))
                                            .h(px(28.0))
                                            .rounded_full()
                                            .bg(theme.title_bar)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_xs()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(if is_active {
                                                theme.success
                                            } else {
                                                theme.muted_foreground
                                            })
                                            .child(initial),
                                    )
                                    .child(
                                        div()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_weight(FontWeight::MEDIUM)
                                                            .text_color(theme.foreground)
                                                            .child(acc_label.clone()),
                                                    )
                                                    .child(
                                                        Tag::new()
                                                            .child(if is_active {
                                                                "Active"
                                                            } else {
                                                                "Backup"
                                                            })
                                                            .with_variant(if is_active {
                                                                TagVariant::Success
                                                            } else {
                                                                TagVariant::Secondary
                                                            })
                                                            .small(),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .mt_1()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(if is_active {
                                                        "Primary account for coding and prompt turns"
                                                    } else {
                                                        "Standby account — auto-failover on rate limits"
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                     .children((!is_active).then(|| {
                                        Button::new(format!("make-active-{}", acc_id))
                                            .icon(IconName::Check)
                                            .label("Set Active")
                                            .outline()
                                             .on_click(move |_event, _window, cx| {
                                                 let acc_id = acc_id_make_active.clone();
                                                 let _ = model_active.update(cx, |state, cx| {
                                                     controller::dispatch(
                                                         state,
                                                         AppAction::SetActiveCodexAccount(acc_id),
                                                     );
                                                     cx.notify();
                                                 });
                                                 let _ = view_active.update(cx, |this, cx| {
                                                     this.refresh_providers_snapshot();
                                                     cx.notify();
                                                 });
                                             })
                                    }))
                                    .child(
                                        Button::new(format!("remove-acc-{}", acc_id))
                                            .icon(IconName::Delete)
                                            .label("Disconnect")
                                            .ghost()
                                             .on_click(move |_event, _window, cx| {
                                                 let acc_id = acc_id_remove.clone();
                                                 let _ = model_remove.update(cx, |state, cx| {
                                                     controller::dispatch(
                                                         state,
                                                         AppAction::RemoveCodexAccount(acc_id),
                                                     );
                                                     cx.notify();
                                                 });
                                                 let _ = view_remove.update(cx, |this, cx| {
                                                     this.refresh_providers_snapshot();
                                                     cx.notify();
                                                 });
                                             }),
                                    ),
                            )
                    })),
            )
            .when(count > 1, |el| {
                el.child(
                    div()
                        .mt_3()
                        .pl(px(52.0))
                        .child(
                            div()
                                .p_2()
                                .rounded_md()
                                .bg(theme.muted)
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(
                                            "⚡ Automatic Failover Active: If your active account hits rate limits or 5-hour quota (HTTP 429), requests seamlessly failover to your backup account.",
                                        ),
                                ),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_key_row(
        &self,
        label: &'static str,
        input: &Entity<InputState>,
        button_id: &'static str,
        action: fn(String) -> AppAction,
        is_openai: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();
        let input = input.clone();
        let auth_tx = self.auth_tx.clone();

        div()
            .py_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(label),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&input).mask_toggle()))
                    .child({
                        let input = input.clone();
                        let auth_tx = auth_tx.clone();
                        Button::new(format!("test-{button_id}"))
                            .icon(IconName::Play)
                            .label("Test")
                            .outline()
                            .on_click(move |_event, _window, cx| {
                                let key = input.read(cx).value().to_string();
                                if is_openai {
                                    let _ = provider_auth::test_openai_connection(
                                        Some(key),
                                        auth_tx.clone(),
                                    );
                                } else {
                                    let _ = provider_auth::test_opencode_connection(
                                        &key,
                                        auth_tx.clone(),
                                    );
                                }
                            })
                    })
                    .child(Button::new(button_id).label("Save").primary().on_click(
                        move |_event, _window, cx| {
                            let value = input.read(cx).value().to_string();
                            model.update(cx, |state, _cx| {
                                controller::dispatch(state, action(value));
                            });
                        },
                    )),
            )
    }

    fn render_providers(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let state_status = self
            .model
            .read(cx)
            .auth_status_msg
            .clone()
            .map(AuthStatusMessage::from_legacy);
        let status = self.auth_message.clone().or(state_status);
        let antigravity_connected = self
            .providers_snapshot
            .as_ref()
            .map(|snapshot| snapshot.antigravity_connected)
            .unwrap_or_else(|| {
                threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some()
            });

        div()
            .mt_5()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .px_4()
            .children(status.map(|status| {
                let (bg, border, fg) = match status.kind {
                    AuthStatusKind::Success => (
                        theme.success.opacity(0.12),
                        theme.success.opacity(0.4),
                        theme.success,
                    ),
                    AuthStatusKind::Error => (
                        theme.danger.opacity(0.12),
                        theme.danger.opacity(0.4),
                        theme.danger,
                    ),
                    AuthStatusKind::Info => (theme.muted, theme.border, theme.foreground),
                };
                div()
                    .mt_4()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(bg)
                    .p_3()
                    .text_xs()
                    .text_color(fg)
                    .child(TextView::markdown("provider-auth-status", status.text).selectable(true))
            }))
            .child(self.render_chatgpt_connections(cx))
            .child(self.render_provider_connection(
                "Google Antigravity",
                "Gemini and other models via Google OAuth PKCE.",
                antigravity_connected,
                true,
                cx,
            ))
            .child(self.render_github_connection(cx))
            .child(self.render_github_pat_row(cx))
            .child(self.render_gitlab_connection(cx))
            .child(self.render_key_row(
                "OpenAI API key",
                &self.openai_input,
                "save-openai-key",
                AppAction::SaveOpenAiKey,
                true,
                cx,
            ))
            .child(self.render_key_row(
                "OpenCode API key",
                &self.opencode_input,
                "save-opencode-key",
                AppAction::SaveOpenCodeKey,
                false,
                cx,
            ))
            .into_any_element()
    }

    fn render_scope_picker(&self, prefix: &'static str, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .gap_1()
            .child(
                Button::new(SharedString::from(format!("{prefix}-project")))
                    .icon(IconName::Folder)
                    .label("Project")
                    .ghost()
                    .selected(!self.install_globally)
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.install_globally = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("{prefix}-global")))
                    .icon(IconName::Globe)
                    .label("Global")
                    .ghost()
                    .selected(self.install_globally)
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.install_globally = true;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_capability_status(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        self.capability_status.clone().map(|status| {
            Alert::new("capability-status-alert", status)
                .title("Notice")
                .with_variant(AlertVariant::Info)
                .into_any_element()
        })
    }

    fn render_extensions(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let view = cx.entity().downgrade();
        let rows = self.extension_rows.clone();
        let project_available = self.active_project(cx).is_some();
        div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.render_capability_status(cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(self.render_scope_picker("extension-scope", cx))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("extension-refresh")
                                    .icon(IconName::Redo)
                                    .label("Refresh")
                                    .outline()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.refresh_extensions(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("extension-install")
                                    .icon(IconName::Plus)
                                    .label("Install .wasm")
                                    .primary()
                                    .disabled(!self.install_globally && !project_available)
                                    .on_click(move |_event, _window, cx| {
                                        let Some(path) = rfd::FileDialog::new()
                                            .set_title("Install a compiled WASI extension")
                                            .add_filter("WebAssembly", &["wasm"])
                                            .pick_file()
                                        else {
                                            return;
                                        };
                                        let _ = view.update(cx, |this, cx| {
                                            let scope = if this.install_globally {
                                                ExtensionScope::Global
                                            } else {
                                                ExtensionScope::Project
                                            };
                                            let project = this.active_project(cx);
                                            this.capability_status = Some(
                                                settings::install_extension(project, &path, scope)
                                                    .unwrap_or_else(|error| error),
                                            );
                                            this.refresh_extensions(cx);
                                            this.model.update(cx, |state, cx| {
                                                state.invalidate_capability_runtimes();
                                                cx.notify();
                                            });
                                            cx.notify();
                                        });
                                    }),
                            ),
                    ),
            )
            .children(rows.into_iter().map(|record| {
                let toggle_record = record.clone();
                let remove_record = record.clone();
                let toggle_view = cx.entity().downgrade();
                let remove_view = cx.entity().downgrade();
                let enabled = record.is_enabled();
                let scope = match record.scope() {
                    ExtensionScope::Global => "Global",
                    ExtensionScope::Project => "Project",
                };
                let (status, status_variant) = if !enabled {
                    ("Disabled", TagVariant::Secondary)
                } else if record.is_effective() {
                    ("Active", TagVariant::Success)
                } else {
                    ("Overridden", TagVariant::Warning)
                };
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.0))
                            .flex_none()
                            .rounded_md()
                            .bg(theme.muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(IconName::HardDrive),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!("{} · v{}", record.name(), record.version())),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Tag::new()
                                            .child(scope)
                                            .with_variant(TagVariant::Secondary)
                                            .small(),
                                    )
                                    .child(
                                        Tag::new()
                                            .child(status)
                                            .with_variant(status_variant)
                                            .small(),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(record.module_path().display().to_string()),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!(
                            "extension-toggle-{}",
                            record.id()
                        )))
                        .checked(enabled)
                        .tooltip(if enabled {
                            "Disable extension"
                        } else {
                            "Enable extension"
                        })
                        .on_click(move |checked, _window, cx| {
                            let checked = *checked;
                            let _ = toggle_view.update(cx, |this, cx| {
                                let result = settings::set_extension_enabled(
                                    this.active_project(cx),
                                    &toggle_record,
                                    checked,
                                );
                                this.capability_status = result.err();
                                this.refresh_extensions(cx);
                                this.model.update(cx, |state, cx| {
                                    state.invalidate_capability_runtimes();
                                    cx.notify();
                                });
                                cx.notify();
                            });
                        }),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "extension-remove-{}",
                            record.id()
                        )))
                        .icon(IconName::Delete)
                        .tooltip("Remove extension")
                        .ghost()
                        .w(px(32.0))
                        .h(px(32.0))
                        .on_click(move |_event, _window, cx| {
                            let _ = remove_view.update(cx, |this, cx| {
                                let result = settings::remove_extension(
                                    this.active_project(cx),
                                    &remove_record,
                                );
                                this.capability_status = result.err();
                                this.refresh_extensions(cx);
                                this.model.update(cx, |state, cx| {
                                    state.invalidate_capability_runtimes();
                                    cx.notify();
                                });
                                cx.notify();
                            });
                        }),
                    )
            }))
            .when(self.extension_rows.is_empty(), |view| {
                view.child(Self::empty_state("No WASI extensions found.", theme))
            })
            .into_any_element()
    }

    fn render_skills(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let rows = self.skill_rows.clone();
        let skill_ids: Vec<String> = rows.iter().map(|skill| skill.id.clone()).collect();
        let has_enabled_skills = rows.iter().any(|skill| skill.enabled);
        let has_project = self.active_project(cx).is_some();
        div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.render_capability_status(cx))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("skills-disable-all")
                            .label("Disable all")
                            .outline()
                            .disabled(!has_project || !has_enabled_skills)
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                let Some(project) = this.active_project(cx) else {
                                    this.capability_status =
                                        Some("Attach a project to manage skills.".into());
                                    cx.notify();
                                    return;
                                };
                                this.capability_status =
                                    settings::disable_all_skills(&project, skill_ids.clone()).err();
                                this.refresh_skills(cx);
                                this.model.update(cx, |state, cx| {
                                    state.invalidate_capability_runtimes();
                                    cx.notify();
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("skills-refresh")
                            .icon(IconName::Redo)
                            .label("Refresh")
                            .outline()
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.refresh_skills(cx);
                                cx.notify();
                            })),
                    ),
            )
            .children(rows.into_iter().map(|skill| {
                let view = cx.entity().downgrade();
                let skill_id = skill.id.clone();
                let enabled = skill.enabled;
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.0))
                            .flex_none()
                            .rounded_md()
                            .bg(theme.muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(IconName::BookOpen),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(skill.name),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(skill.description),
                            )
                            .child({
                                let status_label = if !skill.is_valid {
                                    "Invalid"
                                } else if enabled {
                                    "Enabled"
                                } else {
                                    "Disabled"
                                };
                                let status_variant = if !skill.is_valid {
                                    TagVariant::Danger
                                } else if enabled {
                                    TagVariant::Success
                                } else {
                                    TagVariant::Secondary
                                };
                                div()
                                    .mt_1()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Tag::new()
                                            .child(skill.scope.display_name().to_string())
                                            .with_variant(TagVariant::Secondary)
                                            .small(),
                                    )
                                    .child(
                                        Tag::new()
                                            .child(status_label)
                                            .with_variant(status_variant)
                                            .small(),
                                    )
                            }),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("skill-toggle-{skill_id}")))
                            .checked(enabled)
                            .disabled(!has_project || !skill.is_valid)
                            .tooltip(if enabled {
                                "Disable skill"
                            } else {
                                "Enable skill"
                            })
                            .on_click(move |checked, _window, cx| {
                                let checked = *checked;
                                let _ = view.update(cx, |this, cx| {
                                    let Some(project) = this.active_project(cx) else {
                                        this.capability_status =
                                            Some("Attach a project to manage skills.".into());
                                        cx.notify();
                                        return;
                                    };
                                    this.capability_status =
                                        settings::set_skill_enabled(&project, &skill_id, checked)
                                            .err();
                                    this.refresh_skills(cx);
                                    this.model.update(cx, |state, cx| {
                                        state.invalidate_capability_runtimes();
                                        cx.notify();
                                    });
                                    cx.notify();
                                });
                            }),
                    )
            }))
            .when(self.skill_rows.is_empty(), |view| {
                view.child(Self::empty_state("No skills found.", theme))
            })
            .into_any_element()
    }

    fn render_acp_agents(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let rows = self.acp_rows.clone();
        let has_project = self.active_project(cx).is_some();
        let selected_scope = if self.install_globally {
            AcpScope::Global
        } else {
            AcpScope::Project
        };
        let add_view = cx.entity().downgrade();
        let name_input = self.acp_name_input.clone();
        let command_input = self.acp_command_input.clone();
        div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.render_capability_status(cx))
            .child(self.render_scope_picker("acp-scope", cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Quick setup"),
                    )
                    .children(settings::ACP_PRESETS.iter().map(|preset| {
                        let preset_view = cx.entity().downgrade();
                        let configured = rows.iter().find(|record| {
                            preset.matches_agent(&record.config)
                                && record.config.scope == selected_scope
                        });
                        let enabled = configured.is_some_and(|record| record.config.enabled);
                        let status = configured
                            .map(|record| record.status.display_status())
                            .unwrap_or_else(|| "Not configured".to_string());
                        let preset_id = preset.id;
                        div()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.title_bar)
                            .px_4()
                            .py_3()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(32.0))
                                    .flex_none()
                                    .rounded_md()
                                    .bg(theme.muted)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(theme.muted_foreground)
                                    .child(Icon::default().path("icons/providers/acp.svg")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(preset.name),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(preset.description),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(status),
                                    ),
                            )
                            .child(
                                Switch::new(SharedString::from(format!(
                                    "acp-preset-{preset_id}-{:?}",
                                    selected_scope
                                )))
                                .checked(enabled)
                                .disabled(selected_scope == AcpScope::Project && !has_project)
                                .tooltip(if enabled {
                                    "Disable ACP agent"
                                } else {
                                    "Enable ACP agent"
                                })
                                .on_click(
                                    move |checked, _window, cx| {
                                        let checked = *checked;
                                        let _ = preset_view.update(cx, |this, cx| {
                                            let project = this.active_project(cx);
                                            this.capability_status =
                                                settings::set_acp_preset_enabled(
                                                    project.as_deref(),
                                                    selected_scope,
                                                    preset,
                                                    checked,
                                                )
                                                .err();
                                            this.refresh_acp(cx);
                                            cx.notify();
                                        });
                                    },
                                ),
                            )
                    })),
            )
            .child(
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Custom agent"),
                    )
                    .child(Input::new(&self.acp_name_input))
                    .child(Input::new(&self.acp_command_input))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("acp-refresh")
                                    .icon(IconName::Redo)
                                    .label("Refresh")
                                    .outline()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.refresh_acp(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("acp-add")
                                    .icon(IconName::Plus)
                                    .label("Add agent")
                                    .primary()
                                    .disabled(!self.install_globally && !has_project)
                                    .on_click(move |_event, _window, cx| {
                                        let name = name_input.read(cx).value().to_string();
                                        let command = command_input.read(cx).value().to_string();
                                        let _ = add_view.update(cx, |this, cx| {
                                            let scope = if this.install_globally {
                                                AcpScope::Global
                                            } else {
                                                AcpScope::Project
                                            };
                                            let project = this.active_project(cx);
                                            this.capability_status = settings::add_acp_agent(
                                                project.as_deref(),
                                                scope,
                                                &name,
                                                &command,
                                            )
                                            .err();
                                            this.refresh_acp(cx);
                                            cx.notify();
                                        });
                                    }),
                            ),
                    ),
            )
            .children(rows.into_iter().filter_map(|record| {
                if settings::ACP_PRESETS
                    .iter()
                    .any(|preset| preset.matches_agent(&record.config))
                {
                    return None;
                }
                let toggle_view = cx.entity().downgrade();
                let remove_view = cx.entity().downgrade();
                let config = record.config;
                let toggle_id = config.id.clone();
                let remove_id = config.id.clone();
                let enabled = config.enabled;
                let scope = config.scope;
                let command_line = config.command_line();
                div()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.0))
                            .flex_none()
                            .rounded_md()
                            .bg(theme.muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(Icon::default().path("icons/providers/acp.svg")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(config.name),
                            )
                            .child({
                                let scope_label = match scope {
                                    AcpScope::Global => "Global",
                                    AcpScope::Project => "Project",
                                };
                                let status_label = record.status.display_status();
                                let status_variant = if status_label.contains("Ready")
                                    || status_label.contains("Available")
                                {
                                    TagVariant::Success
                                } else if status_label.contains("Failed")
                                    || status_label.contains("Error")
                                {
                                    TagVariant::Danger
                                } else {
                                    TagVariant::Info
                                };
                                div()
                                    .mt_1()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Tag::new()
                                            .child(scope_label)
                                            .with_variant(TagVariant::Secondary)
                                            .small(),
                                    )
                                    .child(
                                        Tag::new()
                                            .child(status_label)
                                            .with_variant(status_variant)
                                            .small(),
                                    )
                            })
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(command_line),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("acp-toggle-{toggle_id}")))
                            .checked(enabled)
                            .tooltip(if enabled {
                                "Disable ACP agent"
                            } else {
                                "Enable ACP agent"
                            })
                            .on_click(move |checked, _window, cx| {
                                let checked = *checked;
                                let _ = toggle_view.update(cx, |this, cx| {
                                    let project = this.active_project(cx);
                                    this.capability_status = settings::set_acp_enabled(
                                        project.as_deref(),
                                        scope,
                                        &toggle_id,
                                        checked,
                                    )
                                    .err();
                                    this.refresh_acp(cx);
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("acp-remove-{remove_id}")))
                            .icon(IconName::Delete)
                            .tooltip("Remove ACP agent")
                            .ghost()
                            .w(px(32.0))
                            .h(px(32.0))
                            .on_click(move |_event, _window, cx| {
                                let _ = remove_view.update(cx, |this, cx| {
                                    let project = this.active_project(cx);
                                    this.capability_status = settings::remove_acp_agent(
                                        project.as_deref(),
                                        scope,
                                        &remove_id,
                                    )
                                    .err();
                                    this.refresh_acp(cx);
                                    cx.notify();
                                });
                            }),
                    )
                    .into()
            }))
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        if self.page == SettingsPage::Providers && self.providers_snapshot.is_none() {
            self.refresh_providers_snapshot();
        }
        let (title, description, content) = match self.page {
            SettingsPage::General => (
                "General",
                "Application details, release channels, and runtime options.",
                self.render_general(cx),
            ),
            SettingsPage::Appearance => (
                "Appearance & Themes",
                "Customize the editor theme and visual aesthetic.",
                self.render_appearance(cx),
            ),
            SettingsPage::Keybindings => (
                "Keybindings",
                "Keyboard shortcuts reference and operational controls.",
                self.render_keybindings(cx),
            ),
            SettingsPage::Providers => (
                "Models & Providers",
                "Configure model providers, cloud authentication, and API credentials.",
                self.render_providers(cx),
            ),
            SettingsPage::Subagents => (
                "Subagents",
                "Choose project defaults for delegated child model and reasoning.",
                self.render_subagents(cx),
            ),
            SettingsPage::Skills => (
                "Skills Catalog",
                "Contextual instructions and automation skills enabled for your active workspace.",
                self.render_skills(cx),
            ),
            SettingsPage::Extensions => (
                "WASI Extensions",
                "Install and manage compiled WebAssembly extensions (.wasm) for tools and language servers.",
                self.render_extensions(cx),
            ),
            SettingsPage::AcpAgents => (
                "ACP Agents",
                "Configure external coding agents communicating over stdio (e.g. Claude Code, Copilot).",
                self.render_acp_agents(cx),
            ),
        };

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .bg(theme.background)
            .child(self.render_navigation(cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .overflow_y_scrollbar()
                    .px_8()
                    .pb_8()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(760.0))
                            .mx_auto()
                            .child(div().h(px(48.0)).flex_none())
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(description),
                            )
                            .child(content),
                    ),
            )
    }
}
