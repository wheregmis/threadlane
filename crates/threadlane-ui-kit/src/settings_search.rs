/// A local, value-free settings destination exposed by the command palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsSearchItem {
    pub id: &'static str,
    pub title: &'static str,
    pub page: &'static str,
    pub keywords: &'static [&'static str],
}

/// Static metadata only: never populated from settings values or provider state.
pub const SETTINGS_SEARCH_ITEMS: &[SettingsSearchItem] = &[
    SettingsSearchItem {
        id: "general",
        title: "General",
        page: "General",
        keywords: &[
            "application",
            "updates",
            "upgrade",
            "auto review",
            "automatic review",
            "address pr reviews",
        ],
    },
    SettingsSearchItem {
        id: "appearance",
        title: "Appearance",
        page: "Appearance",
        keywords: &["theme", "dark", "light", "color"],
    },
    SettingsSearchItem {
        id: "keybindings",
        title: "Shortcuts",
        page: "Shortcuts",
        keywords: &["shortcuts", "keyboard", "hotkeys"],
    },
    SettingsSearchItem {
        id: "providers",
        title: "Providers",
        page: "Providers",
        keywords: &[
            "github token",
            "github pat",
            "api key",
            "credentials",
            "provider",
            "model",
        ],
    },
    SettingsSearchItem {
        id: "fusion",
        title: "Fusion model",
        page: "Agent & Fusion",
        keywords: &["fusion", "reasoning", "delegation", "session mode"],
    },
    SettingsSearchItem {
        id: "subagents",
        title: "Agent & Fusion",
        page: "Agent & Fusion",
        keywords: &["agents", "subagents"],
    },
    SettingsSearchItem {
        id: "skills",
        title: "Skills",
        page: "Skills",
        keywords: &["skill", "capability"],
    },
    SettingsSearchItem {
        id: "extensions",
        title: "WASI Extensions",
        page: "WASI Extensions",
        keywords: &["extension", "wasm"],
    },
    SettingsSearchItem {
        id: "acp-agents",
        title: "ACP Agents",
        page: "ACP Agents",
        keywords: &["agent client protocol", "external agents"],
    },
];

/// Presentation destination only; hosts decide which page services to load.
pub fn settings_search_page(id: &str) -> Option<crate::settings::SettingsPage> {
    use crate::settings::SettingsPage;
    Some(match id {
        "general" => SettingsPage::General,
        "appearance" => SettingsPage::Appearance,
        "keybindings" => SettingsPage::Keybindings,
        "providers" => SettingsPage::Providers,
        "fusion" | "subagents" => SettingsPage::Subagents,
        "skills" => SettingsPage::Skills,
        "extensions" => SettingsPage::Extensions,
        "acp-agents" => SettingsPage::AcpAgents,
        _ => return None,
    })
}
