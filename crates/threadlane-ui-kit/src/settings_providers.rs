//! Provider settings presentation. Credential stores and authentication stay in the host.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsProvider {
    ChatGPT,
    Antigravity,
    GitHub,
    GitLab,
    OpenAI,
    OpenCode,
}
impl SettingsProvider {
    fn id(self) -> &'static str {
        match self {
            Self::ChatGPT => "chatgpt",
            Self::Antigravity => "antigravity",
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
            Self::OpenAI => "openai",
            Self::OpenCode => "opencode",
        }
    }
    /// Brand marks shared with the model picker (`threadlane-daemon::catalog`);
    /// GitLab has no vendored mark.
    fn icon(self) -> Icon {
        match self {
            Self::ChatGPT | Self::OpenAI => Icon::default().path("icons/providers/openai.svg"),
            Self::Antigravity => Icon::default().path("icons/providers/google.svg"),
            Self::OpenCode => Icon::default().path("icons/providers/opencode.svg"),
            Self::GitHub => Icon::new(IconName::Github),
            Self::GitLab => Icon::new(IconName::Globe),
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::ChatGPT => "OpenAI / ChatGPT",
            Self::Antigravity => "Google Antigravity",
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::OpenAI => "OpenAI API key",
            Self::OpenCode => "OpenCode API key",
        }
    }
}
#[derive(Clone, Copy)]
pub enum SettingsProviderStatusKind {
    Info,
    Success,
    Error,
}
#[derive(Clone)]
pub struct SettingsProviderStatus {
    pub text: String,
    pub kind: SettingsProviderStatusKind,
}
#[derive(Clone)]
pub struct SettingsProviderAccount {
    pub id: String,
    pub label: String,
    pub active: bool,
}
/// Read-only connection snapshot; carries account labels and IDs, never tokens.
pub struct SettingsProviders {
    pub accounts: Vec<SettingsProviderAccount>,
    pub antigravity_connected: bool,
    pub github_status: Option<String>,
    pub gitlab_status: Option<String>,
    pub status: Option<SettingsProviderStatus>,
}
/// The host reads its retained masked fields for SaveKey/TestKey intents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsProviderAction {
    Connect(SettingsProvider),
    Disconnect(SettingsProvider),
    TestConnection(SettingsProvider),
    SaveKey(SettingsProvider),
    TestKey(SettingsProvider),
    SetActiveAccount(String),
    RemoveAccount(String),
    /// Clear the cached model lists and re-fetch every provider's inventory.
    RefreshModels,
}
type Callback = Rc<dyn Fn(SettingsProviderAction, &mut Window, &mut App)>;

fn section() -> Div {
    div().py_4()
}
fn separator(row: Div, cx: &App) -> Div {
    row.border_b_1().border_color(cx.theme().border)
}

/// Provider identity and connection status. Callers supply the semantic action controls.
pub fn settings_provider_connection(
    provider: SettingsProvider,
    connected: bool,
    status: String,
    description: &'static str,
    controls: AnyElement,
    window: &Window,
    cx: &App,
) -> Div {
    let stacked = window.viewport_size().width < window.rem_size() * 44.0;
    section()
        .debug_selector(move || format!("provider-connection-{}", provider.id()))
        .flex()
        .items_center()
        .gap_4()
        .when(stacked, |row| row.flex_col().items_start())
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_start()
                .gap_4()
                .when(stacked, |row| row.w_full().flex_none())
                .child(
                    div()
                        .size_9()
                        .flex_none()
                        .rounded_lg()
                        .bg(cx.theme().muted)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(if connected {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(provider.icon()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(provider.title()),
                                )
                                .child(
                                    Tag::new()
                                        .child(status)
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
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .when(stacked, |row| row.w_full().justify_end())
                .child(controls),
        )
}

/// Account row keyed by durable account ID. The host retains the active selection.
pub fn settings_provider_account(
    account: &SettingsProviderAccount,
    on_action: impl Fn(SettingsProviderAction, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> Stateful<Div> {
    let callback: Callback = Rc::new(on_action);
    let remove = callback.clone();
    let remove_id = account.id.clone();
    let id = account.id.clone();
    let selector: SharedString = format!("provider-account-{}", account.id).into();
    let mut controls = div().flex().flex_none().flex_wrap().items_center().gap_2();
    if !account.active {
        let hint = format!("Set {} as the active ChatGPT account", account.label);
        let selector: SharedString = format!("provider-account-active-{}", account.id).into();
        controls = controls.child(
            Button::new(selector.clone())
                .debug_selector(move || selector.to_string())
                .icon(IconName::Check)
                .label("Set active")
                .outline()
                .accessibility_label(hint.clone())
                .tooltip(hint)
                .on_click(move |_, window, cx| {
                    callback(
                        SettingsProviderAction::SetActiveAccount(id.clone()),
                        window,
                        cx,
                    )
                }),
        );
    }
    let hint = format!("Disconnect ChatGPT account {}", account.label);
    let remove_selector: SharedString = format!("provider-account-remove-{}", account.id).into();
    controls = controls.child(
        Button::new(remove_selector.clone())
            .debug_selector(move || remove_selector.to_string())
            .icon(IconName::Delete)
            .label("Disconnect")
            .ghost()
            .accessibility_label(hint.clone())
            .tooltip(hint)
            .on_click(move |_, window, cx| {
                remove(
                    SettingsProviderAction::RemoveAccount(remove_id.clone()),
                    window,
                    cx,
                )
            }),
    );
    let stacked = window.viewport_size().width < window.rem_size() * 44.0;
    div()
        .id(selector.clone())
        .debug_selector(move || selector.to_string())
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted)
        .flex()
        .items_center()
        .gap_3()
        .when(stacked, |row| row.flex_col().items_start())
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_start()
                .gap_3()
                .when(stacked, |row| row.w_full().flex_none())
                .child(
                    div()
                        .size_7()
                        .flex_none()
                        .rounded_full()
                        .bg(cx.theme().title_bar)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(if account.active {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(
                            account
                                .label
                                .chars()
                                .next()
                                .map(|c| c.to_uppercase().to_string())
                                .unwrap_or_else(|| "U".into()),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(account.label.clone()),
                                )
                                .child(
                                    Tag::new()
                                        .child(if account.active { "Active" } else { "Backup" })
                                        .with_variant(if account.active {
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
                                .text_color(cx.theme().muted_foreground)
                                .child(if account.active {
                                    "Primary account for coding and prompt turns"
                                } else {
                                    "Standby account for rate-limit failover"
                                }),
                        ),
                ),
        )
        .child(controls.when(stacked, |row| row.w_full().justify_end()))
}

/// Labeled masked credential input. Value, masking and persistence belong to its host.
pub fn settings_provider_key(
    provider: SettingsProvider,
    input: &Entity<InputState>,
    on_action: impl Fn(SettingsProviderAction, &mut Window, &mut App) + 'static,
    window: &Window,
    _cx: &App,
) -> Div {
    let callback: Callback = Rc::new(on_action);
    let save = callback.clone();
    let label = if provider == SettingsProvider::GitHub {
        "GitHub Personal Access Token (PAT)"
    } else {
        provider.title()
    };
    let mut controls = div().flex().flex_none().items_center().gap_2();
    if provider != SettingsProvider::GitHub {
        controls = controls.child(
            Button::new(SharedString::from(format!(
                "provider-key-test-{}",
                provider.id()
            )))
            .debug_selector(move || format!("provider-key-test-{}", provider.id()))
            .icon(IconName::Play)
            .label("Test")
            .outline()
            .accessibility_label(format!("Test {label}"))
            .on_click(move |_, window, cx| {
                callback(SettingsProviderAction::TestKey(provider), window, cx)
            }),
        );
    }
    controls = controls.child(
        Button::new(SharedString::from(format!(
            "provider-key-save-{}",
            provider.id()
        )))
        .debug_selector(move || format!("provider-key-save-{}", provider.id()))
        .label("Save")
        // One of several per page; an empty field clears the key, so it stays enabled.
        .outline()
        .accessibility_label(format!("Save {label}"))
        .on_click(move |_, window, cx| save(SettingsProviderAction::SaveKey(provider), window, cx)),
    );
    let stacked = window.viewport_size().width < window.rem_size() * 44.0;
    section()
        .debug_selector(move || format!("provider-key-{}", provider.id()))
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
        .child(
            div()
                .mt_2()
                .flex()
                .items_center()
                .gap_2()
                .when(stacked, |row| row.flex_col().items_stretch())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .when(stacked, |row| row.flex_none().w_full())
                        .child(Input::new(input).mask_toggle().aria_label(label)),
                )
                .child(controls.when(stacked, |row| row.justify_end())),
        )
}

fn connection_controls(provider: SettingsProvider, connected: bool, callback: Callback) -> Div {
    let test = callback.clone();
    let connect = callback.clone();
    let label = if connected {
        "Disconnect"
    } else {
        match provider {
            SettingsProvider::ChatGPT => "Sign in with ChatGPT…",
            SettingsProvider::Antigravity => "Sign in with Google…",
            _ => "Connect via gh CLI",
        }
    };
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .when(
            connected
                && matches!(
                    provider,
                    SettingsProvider::ChatGPT | SettingsProvider::Antigravity
                ),
            |row| {
                row.child(
                    Button::new(SharedString::from(format!(
                        "provider-test-{}",
                        provider.id()
                    )))
                    .debug_selector(move || format!("provider-test-{}", provider.id()))
                    .icon(IconName::Play)
                    .label("Test")
                    .outline()
                    .accessibility_label(format!("Test {} connection", provider.title()))
                    .on_click(move |_, window, cx| {
                        test(SettingsProviderAction::TestConnection(provider), window, cx)
                    }),
                )
            },
        )
        // GitLab is detected, never signed in here: with nothing connected there is
        // no action to offer, rather than a disabled "Disconnect".
        .when(provider != SettingsProvider::GitLab || connected, |row| row.child(
            Button::new(SharedString::from(format!(
                "provider-auth-{}",
                provider.id()
            )))
            .debug_selector(move || format!("provider-auth-{}", provider.id()))
            .label(label)
            .outline()
            .accessibility_label(format!("{label}: {}", provider.title()))
            .on_click(move |_, window, cx| {
                connect(
                    if connected {
                        SettingsProviderAction::Disconnect(provider)
                    } else {
                        SettingsProviderAction::Connect(provider)
                    },
                    window,
                    cx,
                )
            }),
        ))
}

/// Manual escape hatch for a stale picker: clears the cached model lists
/// and re-fetches each connected provider's inventory on click.
fn model_catalog_row(callback: Callback, cx: &App) -> Div {
    let hint = "Clear the cached provider lists and fetch the latest models";
    section()
        .debug_selector(|| "provider-model-catalog".into())
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Model catalog"),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Provider model lists are cached briefly between fetches. Refresh to pick up newly released models right away."),
                ),
        )
        .child(
            Button::new("provider-refresh-models")
                .debug_selector(|| "provider-refresh-models".into())
                .icon(IconName::Redo)
                .label("Refresh models")
                .outline()
                .accessibility_label(hint)
                .tooltip(hint)
                .on_click(move |_, window, cx| {
                    callback(SettingsProviderAction::RefreshModels, window, cx)
                }),
        )
}

pub fn settings_providers(
    providers: &SettingsProviders,
    github: &Entity<InputState>,
    openai: &Entity<InputState>,
    opencode: &Entity<InputState>,
    on_action: impl Fn(SettingsProviderAction, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let callback: Callback = Rc::new(on_action);
    let connected = !providers.accounts.is_empty();
    let status = match providers.accounts.len() {
        0 => "Not connected".into(),
        1 => "1 account".into(),
        count => format!("{count} accounts connected"),
    };
    let controls = if connected {
        let test = callback.clone();
        let add = callback.clone();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                Button::new("provider-test-chatgpt")
                    .debug_selector(|| "provider-test-chatgpt".into())
                    .icon(IconName::Play)
                    .label("Test")
                    .outline()
                    .accessibility_label("Test ChatGPT connection")
                    .on_click(move |_, window, cx| {
                        test(
                            SettingsProviderAction::TestConnection(SettingsProvider::ChatGPT),
                            window,
                            cx,
                        )
                    }),
            )
            .child(
                Button::new("provider-add-chatgpt")
                    .debug_selector(|| "provider-add-chatgpt".into())
                    .icon(IconName::Plus)
                    .label("Add account…")
                    .outline()
                    .on_click(move |_, window, cx| {
                        add(
                            SettingsProviderAction::Connect(SettingsProvider::ChatGPT),
                            window,
                            cx,
                        )
                    }),
            )
    } else {
        connection_controls(SettingsProvider::ChatGPT, false, callback.clone())
    };
    let chatgpt = settings_provider_connection(
        SettingsProvider::ChatGPT,
        connected,
        status,
        if connected {
            "Manage connected accounts with automatic rate-limit and quota failover."
        } else {
            "GPT and Codex models via ChatGPT device login or an API key."
        },
        controls.into_any_element(),
        window,
        cx,
    );
    // Accounts are a child collection; only this level owns their indentation.
    let stacked = window.viewport_size().width < window.rem_size() * 44.0;
    let accounts = div().flex().flex_col().gap_2().pb_4()
        .when(!stacked, |rows| rows.pl(rems(3.25)))
        .children(providers.accounts.iter().map(|account| {
            let callback = callback.clone();
            settings_provider_account(account, move |action, window, cx| callback(action, window, cx), window, cx)
        }))
        .when(providers.accounts.len() > 1, |rows| rows.child(div().mt_1().text_xs().text_color(cx.theme().muted_foreground)
            .child("When the active account reaches a rate limit or quota, requests can use a backup account.")));
    let status_banner = providers.status.clone().map(|status| {
        let (background, border, foreground) = match status.kind {
            SettingsProviderStatusKind::Success => (
                cx.theme().success.opacity(0.12),
                cx.theme().success.opacity(0.4),
                cx.theme().success,
            ),
            SettingsProviderStatusKind::Error => (
                cx.theme().danger.opacity(0.12),
                cx.theme().danger.opacity(0.4),
                cx.theme().danger,
            ),
            SettingsProviderStatusKind::Info => {
                (cx.theme().muted, cx.theme().border, cx.theme().foreground)
            }
        };
        div()
            .id("provider-auth-status")
            .debug_selector(|| "provider-auth-status".into())
            .role(Role::Alert)
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(background)
            .p_3()
            .text_xs()
            .text_color(foreground)
            .child(
                TextView::markdown("provider-auth-status-markdown", status.text).selectable(true),
            )
    });

    // Model providers: sign-ins, API keys and the shared model catalog.
    let antigravity = providers.antigravity_connected;
    let mut models = group_card(cx)
        .child(separator(
            div()
                .child(chatgpt)
                .when(connected, |row| row.child(accounts)),
            cx,
        ))
        .child(separator(
            settings_provider_connection(
                SettingsProvider::Antigravity,
                antigravity,
                if antigravity { "Connected" } else { "Not connected" }.into(),
                "Gemini and other models via Google OAuth PKCE.",
                connection_controls(SettingsProvider::Antigravity, antigravity, callback.clone())
                    .into_any_element(),
                window,
                cx,
            ),
            cx,
        ));
    for (provider, input) in [
        (SettingsProvider::OpenAI, openai),
        (SettingsProvider::OpenCode, opencode),
    ] {
        let callback = callback.clone();
        let row = settings_provider_key(
            provider,
            input,
            move |action, window, cx| callback(action, window, cx),
            window,
            cx,
        );
        models = models.child(separator(row, cx));
    }
    models = models.child(model_catalog_row(callback.clone(), cx));

    // Code hosts: repository access for issues, pull requests and reviews.
    let github_connected = providers.github_status.is_some();
    let gitlab_connected = providers.gitlab_status.is_some();
    let save = callback.clone();
    let hosts = group_card(cx)
        .child(separator(
            settings_provider_connection(
                SettingsProvider::GitHub,
                github_connected,
                providers
                    .github_status
                    .clone()
                    .unwrap_or_else(|| "Not connected".into()),
                "Connect GitHub to inspect pull requests and issues.",
                connection_controls(SettingsProvider::GitHub, github_connected, callback.clone())
                    .into_any_element(),
                window,
                cx,
            ),
            cx,
        ))
        .child(separator(
            settings_provider_key(
                SettingsProvider::GitHub,
                github,
                move |action, window, cx| save(action, window, cx),
                window,
                cx,
            ),
            cx,
        ))
        .child(settings_provider_connection(
            SettingsProvider::GitLab,
            gitlab_connected,
            providers
                .gitlab_status
                .clone()
                .unwrap_or_else(|| "Not connected".into()),
            "Connect GitLab to inspect merge requests and issues.",
            connection_controls(SettingsProvider::GitLab, gitlab_connected, callback.clone())
                .into_any_element(),
            window,
            cx,
        ));

    div()
        .debug_selector(|| "settings-provider-panel".into())
        .mt_5()
        .flex()
        .flex_col()
        .gap_4()
        .children(status_banner)
        .child(group_label("Model providers", cx))
        .child(models)
        .child(group_label("Code hosts", cx).mt_2())
        .child(hosts)
        .into_any_element()
}

fn group_label(title: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(title)
}

fn group_card(cx: &App) -> Div {
    super::settings_group(cx).py_0().flex_col()
}
