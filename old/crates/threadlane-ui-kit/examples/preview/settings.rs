//! Local host for shared settings. No preference writes, updates, or authentication.
use gpui::{prelude::*, AppContext, Context, EventEmitter, IntoElement, Render, Window};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, Sizable};
use gpui_component::input::InputState;
use threadlane_protocol::{OrchestratorMode, ReasoningEffort};
use threadlane_ui_kit::settings::{
    self as kit, SettingsAction, SettingsAgent, SettingsAgentAction, SettingsAgentModel,
    SettingsCatalog, SettingsCatalogAction, SettingsCatalogKind, SettingsCatalogRow,
    SettingsCatalogStatus, SettingsGeneral, SettingsPage, SettingsUpdate,
};

pub enum SettingsPreviewEvent {
    Back,
}
pub struct SettingsPreview {
    page: SettingsPage,
    general: SettingsGeneral,
    agent: Option<SettingsAgent>,
    skills: SettingsCatalog,
    extensions: SettingsCatalog,
    installed_samples: usize,
    external_agents: kit::SettingsExternalAgents,
    acp_name: gpui::Entity<InputState>,
    acp_command: gpui::Entity<InputState>,
    providers: kit::SettingsProviders,
    github_key: gpui::Entity<InputState>,
    openai_key: gpui::Entity<InputState>,
    opencode_key: gpui::Entity<InputState>,
}
impl EventEmitter<SettingsPreviewEvent> for SettingsPreview {}
fn completion_alert_sample(sample: usize) -> kit::SettingsCompletionAlerts {
    kit::SettingsCompletionAlerts {
        supported: sample != 4,
        enabled: sample == 2,
        busy: sample == 1,
        loading: false,
        error: (sample == 3).then(|| "Sample save error. Try again.".into()),
        status: (sample == 2).then(|| "Preview test alert requested. No system alert was sent.".into()),
    }
}
impl SettingsPreview {
    pub(super) fn open_search_destination(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(page) = threadlane_ui_kit::settings_search_page(id) { self.page = page; cx.notify(); }
    }
    pub fn new(project: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            providers: kit::SettingsProviders {
                accounts: vec![
                    kit::SettingsProviderAccount { id: "sample-personal".into(), label: "Personal · sample".into(), active: true },
                    kit::SettingsProviderAccount { id: "sample-work".into(), label: "Work · sample".into(), active: false },
                ],
                antigravity_connected: false, github_status: None,
                gitlab_status: Some("Connected · preview sample".into()), status: None,
                models_open: true, hosting_open: true,
            },
            github_key: cx.new(|cx| InputState::new(window, cx).placeholder("Sample GitHub token").masked(true)),
            openai_key: cx.new(|cx| InputState::new(window, cx).placeholder("Sample OpenAI key").default_value("sample-openai-key").masked(true)),
            opencode_key: cx.new(|cx| InputState::new(window, cx).placeholder("Sample OpenCode key").masked(true)),
            external_agents: external_agent_samples(),
            acp_name: cx.new(|cx| InputState::new(window, cx).placeholder("Agent name")),
            acp_command: cx.new(|cx| InputState::new(window, cx).placeholder("agent --acp")),
            page: SettingsPage::General,
            installed_samples: 0,
            skills: SettingsCatalog {
                kind: SettingsCatalogKind::Skills,
                has_project: true,
                install_globally: false,
                status: None,
                rows: vec![
                    SettingsCatalogRow {
                        id: "sample-review".into(),
                        title: "Code review".into(),
                        description:
                            "Review changes for correctness, regressions and missing coverage."
                                .into(),
                        scope: "Project".into(),
                        status: SettingsCatalogStatus::Enabled,
                        enabled: true,
                        disabled_reason: None,
                    },
                    SettingsCatalogRow {
                        id: "sample-docs".into(),
                        title: "Documentation".into(),
                        description: "Keep project documentation aligned with the implementation."
                            .into(),
                        scope: "Global".into(),
                        status: SettingsCatalogStatus::Disabled,
                        enabled: false,
                        disabled_reason: None,
                    },
                    SettingsCatalogRow {
                        id: "sample-invalid".into(),
                        title: "Incomplete skill".into(),
                        description: "The sample skill is missing required metadata.".into(),
                        scope: "Project".into(),
                        status: SettingsCatalogStatus::Invalid,
                        enabled: false,
                        disabled_reason: Some("This skill is invalid".into()),
                    },
                ],
            },
            extensions: SettingsCatalog {
                kind: SettingsCatalogKind::Extensions,
                has_project: true,
                install_globally: false,
                status: None,
                rows: vec![
                    SettingsCatalogRow {
                        id: "sample-global-search".into(),
                        title: "Search tools · v1.0.0".into(),
                        description: "~/.threadlane/extensions/search-tools.wasm".into(),
                        scope: "Global".into(),
                        status: SettingsCatalogStatus::Overridden,
                        enabled: true,
                        disabled_reason: None,
                    },
                    SettingsCatalogRow {
                        id: "sample-project-search".into(),
                        title: "Search tools · v1.0.0".into(),
                        description: ".threadlane/extensions/search-tools.wasm".into(),
                        scope: "Project".into(),
                        status: SettingsCatalogStatus::Active,
                        enabled: true,
                        disabled_reason: None,
                    },
                    SettingsCatalogRow {
                        id: "sample-global-format".into(),
                        title: "Format tools · v1.2.0".into(),
                        description: "~/.threadlane/extensions/format-tools.wasm".into(),
                        scope: "Global".into(),
                        status: SettingsCatalogStatus::Disabled,
                        enabled: false,
                        disabled_reason: None,
                    },
                ],
            },
            agent: Some(SettingsAgent {
                models: vec![
                    SettingsAgentModel {
                        id: "sample-reasoning".into(),
                        label: "Sample reasoning model".into(),
                        icon_path: None,
                    },
                    SettingsAgentModel {
                        id: "sample-standard".into(),
                        label: "Sample model without reasoning".into(),
                        icon_path: None,
                    },
                ],
                model: None,
                model_label: "Same as parent".into(),
                effort: None,
                efforts: Some(vec![
                    ReasoningEffort::Low,
                    ReasoningEffort::Medium,
                    ReasoningEffort::High,
                ]),
                mode: OrchestratorMode::Normal,
                error: None,
            }),
            general: SettingsGeneral {
                completion_alerts: completion_alert_sample(if cfg!(target_family = "wasm") { 4 } else { 0 }),
                version: env!("CARGO_PKG_VERSION").into(),
                active_project: project,
                project_count: 1,
                auto_address_reviews: false,
                update: Some(SettingsUpdate {
                    status: "Not checked yet".into(),
                    action: "Check for updates".into(),
                    busy: false,
                }),
            },
        }
    }
    fn apply_catalog(
        &mut self,
        kind: SettingsCatalogKind,
        action: SettingsCatalogAction,
        cx: &mut Context<Self>,
    ) {
        let catalog = if kind == SettingsCatalogKind::Skills {
            &mut self.skills
        } else {
            &mut self.extensions
        };
        match action {
            SettingsCatalogAction::Scope(global) => catalog.install_globally = global,
            SettingsCatalogAction::Refresh => {
                catalog.status = Some("Local sample inventory refreshed".into())
            }
            SettingsCatalogAction::DisableAll
                if kind == SettingsCatalogKind::Skills && catalog.has_project =>
            {
                for row in &mut catalog.rows {
                    row.enabled = false;
                    if !matches!(row.status, SettingsCatalogStatus::Invalid) {
                        row.status = SettingsCatalogStatus::Disabled;
                    }
                }
            }
            SettingsCatalogAction::Toggle { id, enabled } => {
                if let Some(row) = catalog
                    .rows
                    .iter_mut()
                    .find(|row| row.id == id && row.disabled_reason.is_none())
                {
                    row.enabled = enabled;
                    row.status = if enabled {
                        SettingsCatalogStatus::Enabled
                    } else {
                        SettingsCatalogStatus::Disabled
                    };
                }
            }
            SettingsCatalogAction::Remove { id } if kind == SettingsCatalogKind::Extensions => {
                catalog.rows.retain(|row| row.id != id);
            }
            SettingsCatalogAction::Install
                if kind == SettingsCatalogKind::Extensions
                    && (catalog.has_project || catalog.install_globally) =>
            {
                self.installed_samples += 1;
                catalog.rows.push(SettingsCatalogRow {
                    id: format!("sample-installed-{}", self.installed_samples),
                    title: format!("Sample extension {} · v1.0.0", self.installed_samples),
                    description: if catalog.install_globally {
                        "~/.threadlane/extensions/sample.wasm"
                    } else {
                        ".threadlane/extensions/sample.wasm"
                    }
                    .into(),
                    scope: if catalog.install_globally {
                        "Global"
                    } else {
                        "Project"
                    }
                    .into(),
                    status: SettingsCatalogStatus::Active,
                    enabled: true,
                    disabled_reason: None,
                });
                catalog.status =
                    Some("Installed a local sample extension. No files were written.".into());
            }
            _ => {}
        }
        if kind == SettingsCatalogKind::Extensions {
            let overridden_titles: Vec<String> = catalog
                .rows
                .iter()
                .filter(|row| row.scope == "Project" && row.enabled)
                .map(|row| row.title.clone())
                .collect();
            for row in &mut catalog.rows {
                row.status = if !row.enabled {
                    SettingsCatalogStatus::Disabled
                } else if row.scope == "Global" && overridden_titles.contains(&row.title) {
                    SettingsCatalogStatus::Overridden
                } else {
                    SettingsCatalogStatus::Active
                };
            }
        }
        cx.notify();
    }
    fn apply_agent(&mut self, action: SettingsAgentAction, cx: &mut Context<Self>) {
        let Some(agent) = &mut self.agent else {
            return;
        };
        match action {
            SettingsAgentAction::Model(model) => {
                agent.model_label = model
                    .as_ref()
                    .and_then(|id| agent.models.iter().find(|option| &option.id == id))
                    .map(|option| option.label.clone())
                    .unwrap_or_else(|| "Same as parent".into());
                agent.efforts = (model.as_deref() != Some("sample-standard")).then(|| {
                    vec![
                        ReasoningEffort::Low,
                        ReasoningEffort::Medium,
                        ReasoningEffort::High,
                    ]
                });
                agent.model = model;
            }
            SettingsAgentAction::Effort(effort) => agent.effort = effort,
            SettingsAgentAction::Mode(mode) => agent.mode = mode,
        }
        cx.notify();
    }
    fn apply_external_agent(&mut self, action: kit::SettingsExternalAgentAction, cx: &mut Context<Self>) {
        use kit::SettingsExternalAgentAction as Action;
        let agents = &mut self.external_agents;
        match action {
            Action::Scope(global) => agents.global = global,
            Action::Refresh => agents.status = Some("Sample agents refreshed. No processes were started.".into()),
            Action::Add { name, command } if agents.global || agents.has_project => {
                if name.trim().is_empty() || command.trim().is_empty() {
                    agents.status = Some("Enter both an agent name and command.".into());
                } else if command.trim().starts_with("http://") || command.trim().starts_with("https://") {
                    agents.status = Some("ACP agents must be local stdio commands, not URLs.".into());
                } else {
                    self.installed_samples += 1;
                    agents.rows.push(kit::SettingsExternalAgentRow {
                        id: format!("sample-custom-{}", self.installed_samples), name: name.trim().into(),
                        description: command.trim().into(), status: "Not running · preview sample".into(),
                        enabled: true, global: agents.global, preset: false, error: false,
                    });
                    agents.status = Some("Added a local sample agent. No configuration was written.".into());
                }
            }
            Action::Toggle { id, global, preset, enabled } => {
                if global || agents.has_project {
                    if let Some(row) = agents.rows.iter_mut().find(|row| row.id == id && row.global == global && row.preset == preset) {
                        row.enabled = enabled;
                        row.error = false;
                        row.status = if enabled { "Not running · preview sample" } else { "Disabled" }.into();
                    }
                }
            }
            Action::Remove { id, global } => {
                if global || agents.has_project {
                    agents.rows.retain(|row| row.preset || row.id != id || row.global != global);
                }
            }
            Action::Add { .. } => {}
        }
        cx.notify();
    }
    fn apply_provider(&mut self, action: kit::SettingsProviderAction, cx: &mut Context<Self>) {
        use kit::{SettingsProvider as Provider, SettingsProviderAction as Action, SettingsProviderStatusKind as Kind};
        let mut kind = Kind::Success;
        let message = match action {
            Action::Connect(provider) => {
                match provider {
                    Provider::ChatGPT => {
                        self.installed_samples += 1;
                        self.providers.accounts.push(kit::SettingsProviderAccount {
                            id: format!("sample-account-{}", self.installed_samples), label: format!("Account {} · sample", self.installed_samples),
                            active: self.providers.accounts.is_empty(),
                        });
                    }
                    Provider::Antigravity => self.providers.antigravity_connected = true,
                    Provider::GitHub => self.providers.github_status = Some("Connected · preview sample".into()),
                    _ => return,
                }
                "Connected a local sample. No sign-in was started."
            }
            Action::Disconnect(provider) => {
                match provider {
                    Provider::ChatGPT => self.providers.accounts.clear(),
                    Provider::Antigravity => self.providers.antigravity_connected = false,
                    Provider::GitHub => self.providers.github_status = None,
                    Provider::GitLab => self.providers.gitlab_status = None,
                    _ => return,
                }
                "Disconnected the local sample. No credentials were changed."
            }
            Action::TestConnection(_) => "Sample connection test passed. No network request was sent.",
            Action::SetActiveAccount(id) => {
                if !self.providers.accounts.iter().any(|account| account.id == id) { return; }
                for account in &mut self.providers.accounts { account.active = account.id == id; }
                "Changed the active sample account. No preferences were saved."
            }
            Action::RemoveAccount(id) => {
                let removed_active = self.providers.accounts.iter().any(|account| account.id == id && account.active);
                self.providers.accounts.retain(|account| account.id != id);
                if removed_active { if let Some(account) = self.providers.accounts.first_mut() { account.active = true; } }
                "Disconnected the local sample account. No credentials were changed."
            }
            Action::SaveKey(provider) | Action::TestKey(provider) => {
                let input = match provider { Provider::GitHub => &self.github_key, Provider::OpenAI => &self.openai_key,
                    Provider::OpenCode => &self.opencode_key, _ => return };
                let empty = input.read(cx).value().trim().is_empty();
                if matches!(action, Action::SaveKey(_)) {
                    if provider == Provider::GitHub {
                        self.providers.github_status = (!empty).then(|| "Connected · preview sample".into());
                    }
                    if empty { "Cleared the local sample key. No credentials were changed." }
                    else { "Saved the local sample key. No credentials were written." }
                } else if empty {
                    kind = Kind::Error;
                    "Enter a sample key before testing. No network request was sent."
                } else { "Sample key test passed. No network request was sent." }
            }
            Action::RefreshModels => "Sample model refresh finished. No network request was sent.",
            // Presentation only: leave the sample status untouched.
            Action::ToggleSection(section) => {
                let open = match section {
                    kit::SettingsProviderSection::Models => &mut self.providers.models_open,
                    kit::SettingsProviderSection::Hosting => &mut self.providers.hosting_open,
                };
                *open = !*open;
                cx.notify();
                return;
            }
        };
        self.providers.status = Some(kit::SettingsProviderStatus { text: message.into(), kind });
        cx.notify();
    }
    fn apply(&mut self, action: SettingsAction, _: &mut Window, cx: &mut Context<Self>) {
        match action {
            SettingsAction::Page(page) => self.page = page,
            SettingsAction::Back => cx.emit(SettingsPreviewEvent::Back),
            SettingsAction::Theme(name) => {
                threadlane_ui_theme::preview_theme(name, cx);
            }
            SettingsAction::AutoAddressReviews(enabled) => {
                self.general.auto_address_reviews = enabled
            }
            SettingsAction::Update => {
                if let Some(update) = &mut self.general.update {
                    update.status = "Up to date".into();
                }
            }
            SettingsAction::CompletionAlerts(enabled) => {
                self.general.completion_alerts.enabled = enabled;
                self.general.completion_alerts.status = enabled.then(|| "Preview test alert requested. No system alert was sent.".into());
            }
            SettingsAction::TestCompletionAlert => {
                self.general.completion_alerts.status = Some("Preview test alert requested. No system alert was sent.".into());
            }
            SettingsAction::RetryCompletionAlerts => {
                self.general.completion_alerts.error = None;
            }
        }
        cx.notify();
    }
}
impl Render for SettingsPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let callback = move |action, window: &mut Window, cx: &mut gpui::App| {
            let _ = owner.update(cx, |this, cx| this.apply(action, window, cx));
        };
        let content = match self.page {
            SettingsPage::General => gpui::div().flex().flex_col().gap_3()
                .child(gpui::div().text_xs().text_color(cx.theme().muted_foreground).child("Completion alerts · local preview only"))
                .child(gpui::div().flex().flex_wrap().gap_2().children(["Off", "Saving", "Enabled", "Error", "Unsupported"].into_iter().enumerate().map(|(sample, label)| {
                    Button::new(("completion-alert-sample", sample)).label(label).outline().small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.general.completion_alerts = completion_alert_sample(sample);
                            cx.notify();
                        }))
                })))
                .child(kit::settings_general(&self.general, callback.clone(), cx)).into_any_element(),
            SettingsPage::Appearance => kit::settings_appearance(
                &threadlane_ui_theme::active_theme_name(cx),
                callback.clone(),
                window,
                cx,
            ),
            SettingsPage::Keybindings => kit::settings_shortcuts(cx),
            SettingsPage::Subagents => {
                let owner = cx.entity().downgrade();
                kit::settings_agent(
                    self.agent.as_ref(),
                    move |action, _, cx| {
                        let _ = owner.update(cx, |this, cx| this.apply_agent(action, cx));
                    },
                    window,
                    cx,
                )
            }
            SettingsPage::Skills | SettingsPage::Extensions => {
                let kind = if self.page == SettingsPage::Skills {
                    SettingsCatalogKind::Skills
                } else {
                    SettingsCatalogKind::Extensions
                };
                let catalog = if kind == SettingsCatalogKind::Skills {
                    &self.skills
                } else {
                    &self.extensions
                };
                let owner = cx.entity().downgrade();
                kit::settings_catalog(
                    catalog,
                    move |action, _, cx| {
                        let _ = owner.update(cx, |this, cx| this.apply_catalog(kind, action, cx));
                    },
                    window,
                    cx,
                )
            }
            SettingsPage::AcpAgents => {
                let owner = cx.entity().downgrade();
                let agents = kit::SettingsExternalAgents {
                    rows: self.external_agents.rows.iter().filter(|row| !row.preset || row.global == self.external_agents.global).cloned().collect(),
                    global: self.external_agents.global, has_project: self.external_agents.has_project,
                    status: self.external_agents.status.clone(),
                };
                kit::settings_external_agents(&agents, &self.acp_name, &self.acp_command,
                    move |action, _, cx| { let _ = owner.update(cx, |this, cx| this.apply_external_agent(action, cx)); }, window, cx)
            }
            SettingsPage::Providers => {
                let owner = cx.entity().downgrade();
                kit::settings_providers(&self.providers, &self.github_key, &self.openai_key, &self.opencode_key,
                    move |action, _, cx| { let _ = owner.update(cx, |this, cx| this.apply_provider(action, cx)); }, window, cx)
            }
        };
        let navigation = kit::settings_navigation(
            self.page,
            &SettingsPage::ALL,
            callback,
            cx,
        );
        kit::settings_screen(self.page, navigation, content, cx)
    }
}

fn external_agent_samples() -> kit::SettingsExternalAgents {
    let mut rows: Vec<_> = [false, true].into_iter().flat_map(|global| {
        [("claude_code", "Claude Code", "Use Anthropic's Claude Code agent through ACP."),
         ("codex", "Codex", "Use OpenAI Codex through ACP."),
         ("opencode2", "OpenCode (ACP)", "Use OpenCode as an external ACP agent."),
         ("antigravity", "Google Antigravity", "Use Google's Antigravity agent through ACP.")]
            .into_iter().map(move |(id, name, description)| kit::SettingsExternalAgentRow {
                id: id.into(), name: name.into(), description: description.into(),
                status: "Not configured · preview sample".into(), enabled: false, global, preset: true, error: false,
            })
    }).collect();
    rows.extend([false, true].into_iter().map(|global| kit::SettingsExternalAgentRow {
        id: "sample-review".into(), name: "Review agent".into(), description: "review-agent --stdio --workspace sample-project".into(),
        status: if global { "Disabled" } else { "Error: sample command unavailable" }.into(),
        enabled: !global, global, preset: false, error: !global,
    }));
    kit::SettingsExternalAgents { rows, global: false, has_project: true, status: None }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, TestAppContext};
    use std::{cell::Cell, rc::Rc};
    use threadlane_ui_kit::settings::SettingsPage;

    pub(super) fn activate_key(cx: &mut gpui::VisualTestContext, key: &str) {
        let keystroke = Keystroke::parse(key).unwrap();
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    #[gpui::test]
    fn completion_alert_preview_is_controlled_and_exposes_off_saving_enabled_error_unsupported(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(threadlane_ui_theme::init_bundled);
        let captured = Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let preview = cx.new(|cx| super::SettingsPreview::new("Sample workspace".into(), window, cx));
            *capture.borrow_mut() = Some(preview.clone());
            gpui_component::Root::new(preview, window, cx)
        });
        let preview = captured.borrow_mut().take().unwrap();
        cx.simulate_resize(gpui::size(gpui::px(960.0), gpui::px(1200.0)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(!preview.read_with(cx, |view, _| view.general.completion_alerts.enabled));
        let switch = cx.debug_bounds("general-completion-alerts-switch").unwrap();
        cx.simulate_click(switch.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(preview.read_with(cx, |view, _| view.general.completion_alerts.enabled));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let test = cx.debug_bounds("general-test-completion-alert").unwrap();
        cx.simulate_click(test.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(preview.read_with(cx, |view, _| view.general.completion_alerts.status.as_deref().unwrap().contains("No system alert was sent")));
        for sample in [1, 4] {
            preview.update(cx, |view, cx| { view.general.completion_alerts = super::completion_alert_sample(sample); cx.notify(); });
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let switch = cx.debug_bounds("general-completion-alerts-switch").unwrap();
            cx.simulate_click(switch.center(), Modifiers::default());
            cx.run_until_parked();
            assert!(!preview.read_with(cx, |view, _| view.general.completion_alerts.enabled));
        }
        preview.update(cx, |view, cx| { view.general.completion_alerts = super::completion_alert_sample(3); cx.notify(); });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let retry = cx.debug_bounds("general-retry-completion-alerts").unwrap();
        cx.simulate_click(retry.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(preview.read_with(cx, |view, _| view.general.completion_alerts.error.is_none()));
    }

    #[gpui::test]
    fn shared_settings_navigation_controls_themes_and_back(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(threadlane_ui_theme::init_bundled);
        cx.update(|cx| {
            assert!(threadlane_ui_theme::preview_theme("Threadlane Dark", cx));
        });
        let captured = Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let preview = cx.new(|cx| super::SettingsPreview::new("Sample workspace".into(), window, cx));
            *capture.borrow_mut() = Some(preview.clone());
            gpui_component::Root::new(preview, window, cx)
        });
        let preview = captured.borrow_mut().take().unwrap();
        cx.simulate_resize(gpui::size(gpui::px(960.0), gpui::px(900.0)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let switch = cx
            .debug_bounds("general-auto-address-pr-reviews-switch")
            .unwrap();
        cx.simulate_click(switch.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(preview.read_with(cx, |view, _| view.general.auto_address_reviews));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        activate_key(cx, "space");
        assert!(!preview.read_with(cx, |view, _| view.general.auto_address_reviews));

        let update = cx.debug_bounds("settings-update").unwrap();
        cx.simulate_click(update.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            preview.read_with(cx, |view, _| view
                .general
                .update
                .as_ref()
                .unwrap()
                .status
                .clone()),
            "Up to date"
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let providers = cx.debug_bounds("settings-providers").unwrap();
        cx.simulate_click(providers.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            preview.read_with(cx, |view, _| view.page),
            SettingsPage::Providers
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let appearance = cx.debug_bounds("settings-appearance").unwrap();
        cx.simulate_click(appearance.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        for (width, font) in [(480.0, 14.0), (800.0, 16.0), (960.0, 20.0)] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(900.0)));
            cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(font));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let heading = cx.debug_bounds("settings-page-heading").unwrap();
            let dark = cx.debug_bounds("theme-card-dark").unwrap();
            let light = cx.debug_bounds("theme-card-light").unwrap();
            assert_eq!(
                heading.left(),
                dark.left(),
                "theme card must share the content spine"
            );
            if width < font * 44.0 {
                assert_eq!(dark.left(), light.left());
                assert!(light.top() > dark.bottom());
            } else {
                assert_eq!(dark.top(), light.top());
            }
            assert!(
                light.right() <= gpui::px(width),
                "theme controls overflow {width}px at {font}px: {light:?}"
            );
        }
        let light = cx.debug_bounds("theme-card-light").unwrap();
        cx.simulate_click(light.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            cx.update(|_, cx| threadlane_ui_theme::active_theme_name(cx)),
            "Threadlane Light"
        );
        // Button pointer activation intentionally keeps the previous focus.
        // Start a real Tab traversal at the first enabled settings destination.
        cx.update(|window, cx| {
            window.blur(cx);
            for _ in 0..SettingsPage::ALL.len() + 2 {
                window.focus_next(cx);
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        activate_key(cx, "enter");
        assert_eq!(
            cx.update(|_, cx| threadlane_ui_theme::active_theme_name(cx)),
            "Threadlane Dark"
        );

        let shortcuts = cx.debug_bounds("settings-keybindings").unwrap();
        cx.simulate_click(shortcuts.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            preview.read_with(cx, |view, _| view.page),
            SettingsPage::Keybindings
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let back_count = Rc::new(Cell::new(0));
        let count = back_count.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&preview, move |_, _: &super::SettingsPreviewEvent, _| {
                count.set(count.get() + 1)
            })
        });
        let back = cx.debug_bounds("settings-back").unwrap();
        cx.simulate_click(back.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(back_count.get(), 1);
    }
    #[gpui::test]
    fn shared_agent_settings_menus_inheritance_and_layout(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(threadlane_ui_theme::init_bundled);
        let captured = Rc::new(std::cell::RefCell::new(None));
        let capture = captured.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let preview = cx.new(|cx| super::SettingsPreview::new("Sample workspace".into(), window, cx));
            *capture.borrow_mut() = Some(preview.clone());
            gpui_component::Root::new(preview, window, cx)
        });
        let preview = captured.borrow_mut().take().unwrap();
        cx.simulate_resize(gpui::size(gpui::px(960.0), gpui::px(900.0)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let page = cx.debug_bounds("settings-subagents").unwrap();
        cx.simulate_click(page.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            preview.read_with(cx, |view, _| view.page),
            SettingsPage::Subagents
        );

        // A pointer opens the canonical menu; arrow keys choose and Enter commits.
        let model = cx.debug_bounds("fast-model-picker").unwrap();
        cx.simulate_click(model.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            preview.read_with(cx, |view, _| view.agent.as_ref().unwrap().model.clone()),
            Some("sample-reasoning".into())
        );

        let effort = cx.debug_bounds("fast-reasoning-picker").unwrap();
        cx.simulate_click(effort.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down down down down enter");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            preview.read_with(cx, |view, _| view.agent.as_ref().unwrap().effort),
            Some(threadlane_protocol::ReasoningEffort::High)
        );

        let mode = cx.debug_bounds("orchestrator-picker").unwrap();
        cx.simulate_click(mode.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            preview.read_with(cx, |view, _| view.agent.as_ref().unwrap().mode),
            threadlane_protocol::OrchestratorMode::Fusion
        );

        let model = cx.debug_bounds("fast-model-picker").unwrap();
        cx.simulate_click(model.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down down down enter");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            preview.read_with(cx, |view, _| view.agent.as_ref().unwrap().model.clone()),
            Some("sample-standard".into())
        );
        assert!(cx.debug_bounds("fast-reasoning-picker").is_none());

        // Keyboard activation and Escape return to the focused trigger. Pointer
        // activation preserves previous focus, as it does for other kit buttons.
        cx.update(|window, cx| {
            window.blur(cx);
            for _ in 0..SettingsPage::ALL.len() + 2 {
                window.focus_next(cx);
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        activate_key(cx, "enter");
        cx.simulate_keystrokes("down escape");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            preview.read_with(cx, |view, _| view.agent.as_ref().unwrap().model.clone()),
            Some("sample-standard".into())
        );
        activate_key(cx, "enter");
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(preview.read_with(cx, |view, _| view.agent.as_ref().unwrap().model.is_none()));
        assert!(cx.debug_bounds("fast-reasoning-picker").is_some());

        // Choosing inheritance retains the configured effort until explicitly reset.
        let effort = cx.debug_bounds("fast-reasoning-picker").unwrap();
        cx.simulate_click(effort.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(preview.read_with(cx, |view, _| view.agent.as_ref().unwrap().effort.is_none()));

        for (width, font) in [(480.0, 14.0), (800.0, 16.0), (1100.0, 20.0)] {
            cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(1100.0)));
            cx.update(|_, cx| gpui_component::Theme::global_mut(cx).font_size = gpui::px(font));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let heading = cx.debug_bounds("settings-page-heading").unwrap();
            let fields = [
                "fusion-model-field",
                "fusion-effort-field",
                "fusion-mode-field",
            ]
            .map(|id| cx.debug_bounds(id).unwrap());
            for field in fields {
                assert_eq!(field.left(), heading.left());
                assert_eq!(field.right(), fields[0].right());
                assert!(field.right() <= gpui::px(width));
            }
            let controls = [
                "fast-model-picker",
                "fast-reasoning-picker",
                "orchestrator-picker",
            ]
            .map(|id| cx.debug_bounds(id).unwrap());
            for (field, control) in fields.into_iter().zip(controls) {
                assert_eq!(control.left(), controls[0].left());
                assert_eq!(control.right(), controls[0].right());
                assert!(control.right() < field.right());
                assert!(control.left() > field.left());
            }
        }
        preview.update(cx, |view, cx| {
            view.agent.as_mut().unwrap().error = Some("Couldn't save. Try again.".into());
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("settings-agent-error").is_some());
        assert!(
            cx.debug_bounds("fast-model-picker").is_some(),
            "failed saves keep controls usable"
        );
        preview.update(cx, |view, cx| {
            view.agent = None;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("settings-agent-empty").is_some());
        assert!(cx.debug_bounds("fast-model-picker").is_none());
    }
}

#[cfg(test)]
#[path = "settings_catalog_tests.rs"]
mod catalog_tests;

#[cfg(test)]
#[path = "settings_external_agents_tests.rs"]
mod external_agents_tests;

#[cfg(test)]
#[path = "settings_providers_tests.rs"]
mod providers_tests;
