//! Settings presentation shared by the desktop host and the local web preview.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::switch::Switch;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};
use std::rc::Rc;

#[path = "settings_projects.rs"]
mod projects;
pub use projects::{project_removal_dialog, settings_projects, SettingsProject};
#[path = "settings_agent.rs"]
mod agent;
pub use agent::{settings_agent, SettingsAgent, SettingsAgentAction, SettingsAgentModel};
#[path = "settings_catalog.rs"]
mod catalog;
pub use catalog::{settings_catalog, settings_catalog_row, settings_scope_picker, SettingsCatalog,
    SettingsCatalogAction, SettingsCatalogKind, SettingsCatalogRow, SettingsCatalogStatus};
#[path = "settings_external_agents.rs"]
mod external_agents;
pub use external_agents::{settings_external_agent_row, settings_external_agents,
    SettingsExternalAgentAction, SettingsExternalAgentRow, SettingsExternalAgents};
#[path = "settings_providers.rs"]
mod providers;
pub use providers::{settings_provider_connection, settings_provider_account, settings_provider_key,
    settings_providers, SettingsProvider, SettingsProviderAccount, SettingsProviderAction,
    SettingsProviders, SettingsProviderStatus, SettingsProviderStatusKind};

/// Stable settings destinations. The host owns selection and page loading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
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
impl SettingsPage {
    pub const ALL: [Self; 8] = [
        Self::General,
        Self::Appearance,
        Self::Keybindings,
        Self::Providers,
        Self::Subagents,
        Self::Skills,
        Self::Extensions,
        Self::AcpAgents,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::General => "settings-general",
            Self::Appearance => "settings-appearance",
            Self::Keybindings => "settings-keybindings",
            Self::Providers => "settings-providers",
            Self::Subagents => "settings-subagents",
            Self::Skills => "settings-skills",
            Self::Extensions => "settings-extensions",
            Self::AcpAgents => "settings-acp",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Keybindings => "Shortcuts",
            Self::Providers => "Providers",
            Self::Subagents => "Agent & Fusion",
            Self::Skills => "Skills",
            Self::Extensions => "WASI Extensions",
            Self::AcpAgents => "ACP Agents",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance & Themes",
            Self::Providers => "Models & Providers",
            Self::Skills => "Skills Catalog",
            _ => self.label(),
        }
    }
    pub fn description(self) -> &'static str {
        match self {
 Self::General => "Application details, release channels, and runtime options.",
 Self::Appearance => "Customize the editor theme and visual aesthetic.",
 Self::Keybindings => "Navigate the workspace, write messages, and use your tools from the keyboard.",
 Self::Providers => "Configure model providers, cloud authentication, and API credentials.",
 Self::Subagents => "Choose the Fusion model, reasoning effort, and the session mode.",
 Self::Skills => "Contextual instructions and automation skills enabled for your active workspace.",
 Self::Extensions => "Install and manage compiled WebAssembly extensions (.wasm) for tools and language servers.",
 Self::AcpAgents => "Configure external coding agents communicating over stdio (e.g. Claude Code, Copilot)." }
    }
    fn icon(self) -> IconName {
        match self {
            Self::General => IconName::Settings,
            Self::Appearance => IconName::Palette,
            Self::Keybindings => IconName::SquareTerminal,
            Self::Providers => IconName::Cpu,
            Self::Subagents => IconName::Bot,
            Self::Skills => IconName::BookOpen,
            Self::Extensions => IconName::HardDrive,
            Self::AcpAgents => IconName::Network,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsAction {
    Page(SettingsPage),
    Back,
    Theme(&'static str),
    Update,
    AutoAddressReviews(bool),
}
type SettingsCallback = Rc<dyn Fn(SettingsAction, &mut Window, &mut App)>;

/// Project and release values supplied by the host. No preference or update IO.
pub struct SettingsGeneral {
    pub version: String,
    pub active_project: String,
    pub project_count: usize,
    pub auto_address_reviews: bool,
    pub update: Option<SettingsUpdate>,
}
pub struct SettingsUpdate {
    pub status: String,
    pub action: String,
    pub busy: bool,
}

/// One navigation row; stable domain ID, selected state and keyboard activation.
pub fn settings_navigation_button(
    page: SettingsPage,
    selected: bool,
    enabled: bool,
    _cx: &App,
) -> Button {
    Button::new(page.id())
        .debug_selector(move || page.id().into())
        .accessibility_label(if selected {
            format!("{}, current page", page.label())
        } else if !enabled {
            format!("{}, unavailable in this preview", page.label())
        } else {
            page.label().into()
        })
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .child(Icon::new(page.icon()).small().flex_none())
                .child(page.label()),
        )
        .ghost()
        .selected(selected)
        .disabled(!enabled)
        .w_full()
        .justify_start()
        .when(!enabled, |button| {
            button.tooltip("This settings page is still being extracted for preview")
        })
}
/// Shared full settings navigation; unavailable capabilities stay visibly disabled.
pub fn settings_navigation(
    page: SettingsPage,
    available: &[SettingsPage],
    on_action: impl Fn(SettingsAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let callback: SettingsCallback = Rc::new(on_action);
    let colors = cx.theme().colors;
    div()
        .w(rems(15.0))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(colors.border)
        .bg(colors.title_bar)
        .child(
            div()
                .h(threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE)
                .flex_none(),
        )
        .child(
            div()
                .pl(rems(1.25))
                .pr_3()
                .pb_2()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.muted_foreground)
                .child("SETTINGS"),
        )
        .child(
            div()
                .px_3()
                .flex()
                .flex_col()
                .gap_1()
                .children(SettingsPage::ALL.into_iter().map(|destination| {
                    let callback = callback.clone();
                    settings_navigation_button(
                        destination,
                        page == destination,
                        available.contains(&destination),
                        cx,
                    )
                    .on_click(move |_, window, cx| {
                        callback(SettingsAction::Page(destination), window, cx)
                    })
                })),
        )
        .child(div().flex_1())
        .child(
            div().flex_none().px_3().py_2().child(
                Button::new("settings-back")
                    .debug_selector(|| "settings-back".into())
                    .accessibility_label("Back to workspace")
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::ArrowLeft).small().flex_none())
                            .child("Back"),
                    )
                    .ghost()
                    .w_full()
                    .justify_start()
                    .text_color(colors.muted_foreground)
                    .on_click(move |_, window, cx| callback(SettingsAction::Back, window, cx)),
            ),
        )
}
/// Content owns its vertical scroll region; navigation retains the original width.
pub fn settings_screen(
    page: SettingsPage,
    navigation: impl IntoElement,
    content: impl IntoElement,
    cx: &App,
) -> Div {
    let colors = cx.theme().colors;
    div()
        .debug_selector(|| "settings-screen".into())
        .flex_1()
        .h_full()
        .min_w_0()
        .flex()
        .bg(colors.background)
        .child(navigation)
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
                        .max_w(rems(48.0))
                        .child(
                            div()
                                .h(threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE)
                                .flex_none(),
                        )
                        .child(
                            div()
                                .id("settings-page-heading")
                                .debug_selector(|| "settings-page-heading".into())
                                .role(Role::Heading)
                                .aria_label(page.title())
                                .text_xl()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors.foreground)
                                .child(page.title()),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_sm()
                                .text_color(colors.muted_foreground)
                                .child(page.description()),
                        )
                        .child(content),
                ),
        )
}
/// Fixed palette for the Appearance page's miniature theme previews. These
/// depict the dark/light themes as static illustrations (audited exception to
/// the token rule: the preview must show its own theme, not the active one),
/// so they are defined once here instead of repeated at each swatch.
fn preview_dark_surface() -> Hsla {
    hsla(0.65, 0.10, 0.08, 1.0)
}
fn preview_dark_well() -> Hsla {
    hsla(0.65, 0.12, 0.14, 1.0)
}
fn preview_light_surface() -> Hsla {
    hsla(0.0, 0.0, 0.98, 1.0)
}
fn preview_light_well() -> Hsla {
    hsla(0.0, 0.0, 0.88, 1.0)
}
fn preview_dot_close() -> Hsla {
    hsla(0.0, 0.7, 0.6, 1.0)
}
fn preview_dot_minimize() -> Hsla {
    hsla(0.12, 0.7, 0.6, 1.0)
}
fn preview_dot_zoom() -> Hsla {
    hsla(0.35, 0.7, 0.6, 1.0)
}

pub fn settings_general(
    general: &SettingsGeneral,
    on_action: impl Fn(SettingsAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme().colors;
    let on_action: SettingsCallback = Rc::new(on_action);
    div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                settings_group(cx)
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
                                            .size_9()
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
                                                    .child("Application details"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child("A workspace for agent-assisted development"),
                                            ),
                                    ),
                            )
                            .child(
                                Tag::new()
                                    .child(format!("v{}", general.version))
                                    // Information, not a success state.
                                    .with_variant(TagVariant::Secondary)
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
                                    .child("Active workspace")
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(general.active_project.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Attached projects")
                                    .child(
                                        div()
                                            .mt_1()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(if general.project_count == 1 { "1 project".into() } else { format!("{} projects", general.project_count) }),
                                    ),
                            ),
                    ),
            )
            // Update installation only supports a packaged macOS .app bundle;
            // on Linux and Windows the controls can only fail, so hide them.
            .when_some(general.update.as_ref(), |element, update| {
                element.child(
                    settings_group(cx)
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
                                                .child("Application updates"),
                                        )
                                        .child(
                                            Tag::new()
                                                .child(update.status.clone())
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
                        )
                        .child(
                            Button::new("settings-update").debug_selector(|| "settings-update".into())
                                .label(update.action.clone())
                                .accessibility_label(update.action.clone())
                                .tooltip(update.action.clone())
                                .outline()
                                .small()
                                .flex_none()
                                .loading(update.busy)
                                .disabled(update.busy)
                                .on_click({ let callback = on_action.clone(); move |_, window, cx| callback(SettingsAction::Update, window, cx) }),
                        ),
                )
            })
            .child(
                settings_group(cx)
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
                                    // The switch beside it already shows on/off.
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child("Auto-Address PR Reviews"),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Automatically trigger the agent to address new code review feedback on open pull requests."),
                            ),
                    )
                    .child(
                        div().debug_selector(|| "general-auto-address-pr-reviews-switch".into()).flex_none().child(Switch::new("general-auto-address-pr-reviews-switch")
                            .accessibility_label(if general.auto_address_reviews { "Disable automatic PR review addressing" } else { "Enable automatic PR review addressing" })
                            .checked(general.auto_address_reviews)
                            .tooltip(if general.auto_address_reviews {
                                "Disable automatic PR review addressing"
                            } else {
                                "Enable automatic PR review addressing"
                            })
                            .on_click(move |checked, _window, cx| {
                                on_action(SettingsAction::AutoAddressReviews(*checked), _window, cx);
                            })),
                    ),
            )
            .into_any_element()
}

/// The two bundled theme choices; swatches depict their own theme, not the active one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTheme {
    Dark,
    Light,
}
impl SettingsTheme {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "Threadlane Dark",
            Self::Light => "Threadlane Light",
        }
    }
    fn id(self) -> &'static str {
        match self {
            Self::Dark => "theme-card-dark",
            Self::Light => "theme-card-light",
        }
    }
}
/// One selectable theme card. The host supplies selection and handles activation.
pub fn settings_theme_card(choice: SettingsTheme, selected: bool, cx: &App) -> Button {
    let colors = cx.theme().colors;
    let (surface, well, description) = match choice {
        SettingsTheme::Dark => (
            preview_dark_surface(),
            preview_dark_well(),
            "High-contrast deep black interface",
        ),
        SettingsTheme::Light => (
            preview_light_surface(),
            preview_light_well(),
            "Clean and crisp light aesthetic",
        ),
    };
    Button::new(choice.id())
        .debug_selector(move || choice.id().into())
        .accessibility_label(if selected {
            format!("{}, current theme", choice.name())
        } else {
            format!("Use {}", choice.name())
        })
        .tooltip(format!("Use {}", choice.name()))
        .outline()
        .selected(selected)
        .h_auto()
        .items_stretch()
        .text_left()
        .p_4()
        .rounded_xl()
        .border_2()
        .border_color(if selected {
            colors.primary
        } else {
            colors.border
        })
        .bg(colors.title_bar)
        .flex()
        .flex_col()
        .gap_3()
        .min_w_0()
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .h_20()
                        .rounded_lg()
                        .border_1()
                        .border_color(colors.border)
                        .bg(surface)
                        .p_3()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .child(
                            div().flex().gap_1_5().children(
                                [
                                    preview_dot_close(),
                                    preview_dot_minimize(),
                                    preview_dot_zoom(),
                                ]
                                .into_iter()
                                .map(|color| div().size_2().rounded_full().bg(color)),
                            ),
                        )
                        .child(div().h_4().w_3_4().rounded_md().bg(well)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .justify_between()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(colors.foreground)
                                        .child(choice.name()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_foreground)
                                        .child(description),
                                ),
                        )
                        .children(selected.then(|| {
                            Tag::new()
                                .child("Active")
                                .with_variant(TagVariant::Success)
                                .small()
                                .flex_none()
                        })),
                ),
        )
}
/// Theme selection requests use the same cards on every host.
pub fn settings_appearance(
    active_theme: &str,
    on_action: impl Fn(SettingsAction, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let callback: SettingsCallback = Rc::new(on_action);
    div()
        .mt_5()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .grid()
                .grid_cols(if window.viewport_size().width < window.rem_size() * 44.0 {
                    1
                } else {
                    2
                })
                .gap_4()
                .children(
                    [SettingsTheme::Dark, SettingsTheme::Light]
                        .into_iter()
                        .map(|choice| {
                            let callback = callback.clone();
                            settings_theme_card(choice, choice.name() == active_theme, cx).on_click(
                                move |_, window, cx| {
                                    callback(SettingsAction::Theme(choice.name()), window, cx)
                                },
                            )
                        }),
                ),
        )
        .into_any_element()
}

/// Repeated settings section surface, shared with rows and future catalog pages.
pub fn settings_group(cx: &App) -> Div {
    div()
        .rounded_xl()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().title_bar)
        .p_4()
        .flex()
}
/// Description and shortcut stay in predictable leading and trailing lanes.
pub fn settings_shortcut_row(keys: &'static str, description: &'static str, cx: &App) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .justify_between()
        .py_1()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(cx.theme().foreground)
                .child(description),
        )
        .child(
            Tag::new()
                .flex_none()
                .child(keys)
                .with_variant(TagVariant::Secondary)
                .small(),
        )
}

pub fn settings_shortcuts(cx: &App) -> AnyElement {
    let theme = cx.theme().colors;

    // Labels follow the bindings actually registered per platform (macOS
    // `cmd-*`, elsewhere `ctrl-*`; editor redo is `ctrl-y` off macOS).
    let k = |mac: &'static str, other: &'static str| {
        if cfg!(target_os = "macos") {
            mac
        } else {
            other
        }
    };
    let shortcuts = [
        (
            "Global",
            vec![
                (k("⌘ K", "Ctrl K"), "Open Command Palette"),
                (k("⇧ ⌘ K", "Ctrl Shift K"), "Switch session"),
                (k("⌘ ,", "Ctrl ,"), "Open Settings"),
                (k("⌘ B", "Ctrl B"), "Toggle Left Sidebar"),
                (k("⌘ R", "Ctrl R"), "Toggle Right Panel"),
                (k("⌘ J", "Ctrl J"), "Toggle Terminal Panel"),
                (k("⌘ N", "Ctrl N"), "New task"),
                (k("⌘ L", "Ctrl L"), "Focus composer"),
                (k("⌘ 1 / 2 / 3", "Ctrl 1 / 2 / 3"), "Chat / Trajectory / Editor tab"),
                ("Escape", "Cancel active agent turn"),
            ],
        ),
        (
            "Composer & Chat",
            vec![
                (
                    k("⌘ F", "Ctrl F"),
                    "Find in conversation (Chat or composer focused)",
                ),
                (
                    k("Enter / ⇧ Enter", "Enter / Shift Enter"),
                    "Next / previous matching message (find focused)",
                ),
                ("Escape", "Close conversation find before cancelling a turn"),
                ("Enter", "Send message or queue for the next turn"),
                (
                    k("⌘ Enter / Ctrl Enter", "Ctrl Enter"),
                    "Steer the current turn (built-in agent)",
                ),
                (k("⇧ Enter", "Shift Enter"), "Insert newline in composer"),
                ("↑ / ↓ (empty composer)", "Recall earlier / later prompts"),
                ("/ (in empty composer)", "Open Slash Commands palette"),
                (
                    "@ (in composer)",
                    "Insert a workspace file path (Git worktrees)",
                ),
            ],
        ),
        (
            "Editor & Diff",
            vec![
                (k("⌘ S", "Ctrl S"), "Save active file"),
                (k("⌘ Z", "Ctrl Z"), "Undo edit"),
                (k("⌘ ⇧ Z", "Ctrl Y"), "Redo edit"),
                (k("⌘ F", "Ctrl F"), "Find in active editor buffer"),
            ],
        ),
        (
            "Terminal",
            vec![
                (
                    k("⌘ F", "Ctrl Shift F"),
                    "Find in terminal output (terminal focused)",
                ),
                (
                    k("Enter / ⇧ Enter", "Enter / Shift Enter"),
                    "Next / previous matching line (find focused)",
                ),
                ("Escape", "Close terminal find (find focused)"),
                (k("⇧ Page Up / Page Down", "Shift Page Up / Page Down"), "Scroll retained output"),
            ],
        ),
    ];

    let mut list = div().mt_5().flex().flex_col().gap_6();

    for (section_title, items) in shortcuts {
        let mut section_div = settings_group(cx).flex_col().gap_2().child(
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
            section_div = section_div.child(settings_shortcut_row(keys, description, cx));
        }

        list = list.child(section_div);
    }

    list.into_any_element()
}
