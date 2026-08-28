use std::path::PathBuf;
use std::rc::Rc;

use gpui::{App, SharedString};
use gpui_component::{ActiveTheme, Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use serde::{Deserialize, Serialize};

fn global_threadlane_dir() -> PathBuf {
    threadlane_protocol::project::default_global_threadlane_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".threadlane")
    })
}

const DEFAULT_THEME_NAME: &str = "Threadlane Dark";
const BUNDLED_THEMES: &str = include_str!("../themes/threadlane.json");

#[derive(Default, Deserialize, Serialize)]
struct ThemePreferences {
    selected_theme: Option<String>,
}

pub fn init(cx: &mut App) {
    register_bundled_themes(cx);
    apply_saved_or_default_theme(cx);

    let themes_dir = global_threadlane_dir().join("themes");
    if let Err(error) = std::fs::create_dir_all(&themes_dir) {
        tracing::warn!(
            "failed to create theme directory {}: {error}",
            themes_dir.display()
        );
        return;
    }

    if let Err(error) = ThemeRegistry::watch_dir(themes_dir, cx, |cx| {
        // Registry reloads rebuild its map, so restore themes embedded in the binary.
        register_bundled_themes(cx);
        apply_saved_or_default_theme(cx);
        cx.refresh_windows();
    }) {
        tracing::warn!("failed to watch Threadlane themes: {error}");
    }
}

pub(crate) fn active_theme_name(cx: &App) -> SharedString {
    cx.theme().theme_name().clone()
}

pub(crate) fn apply_theme(theme_name: &str, cx: &mut App) -> bool {
    let Some(theme) = find_theme(theme_name, cx) else {
        return false;
    };

    apply_theme_config(theme, cx);
    if let Err(error) = save_preferences(&ThemePreferences {
        selected_theme: Some(theme_name.to_string()),
    }) {
        tracing::warn!("failed to save selected theme: {error}");
    }
    cx.refresh_windows();
    true
}

fn register_bundled_themes(cx: &mut App) {
    if let Err(error) = ThemeRegistry::global_mut(cx).load_themes_from_str(BUNDLED_THEMES) {
        tracing::error!("failed to load bundled Threadlane themes: {error}");
    }
}

fn apply_saved_or_default_theme(cx: &mut App) {
    let preferred = load_preferences()
        .selected_theme
        .unwrap_or_else(|| DEFAULT_THEME_NAME.to_string());
    let theme = find_theme(&preferred, cx)
        .or_else(|| find_theme(DEFAULT_THEME_NAME, cx))
        .or_else(|| {
            ThemeRegistry::global(cx)
                .default_themes()
                .get(&ThemeMode::Dark)
                .cloned()
        });

    if let Some(theme) = theme {
        apply_theme_config(theme, cx);
    } else {
        Theme::change(ThemeMode::Dark, None, cx);
    }
}

fn find_theme(theme_name: &str, cx: &App) -> Option<Rc<ThemeConfig>> {
    let lookup_name = match theme_name {
        "Threadlane Black" | "Default Dark" => "Threadlane Dark",
        "Default Light" => "Threadlane Light",
        other => other,
    };
    ThemeRegistry::global(cx).themes().get(lookup_name).cloned()
}

fn apply_theme_config(theme: Rc<ThemeConfig>, cx: &mut App) {
    let mode = theme.mode;
    Theme::global_mut(cx).apply_config(&theme);
    Theme::change(mode, None, cx);
}

fn preferences_path() -> PathBuf {
    global_threadlane_dir().join("gui").join("preferences.json")
}

fn load_preferences() -> ThemePreferences {
    std::fs::read(preferences_path())
        .ok()
        .and_then(|contents| serde_json::from_slice(&contents).ok())
        .unwrap_or_default()
}

fn save_preferences(preferences: &ThemePreferences) -> Result<(), String> {
    let path = preferences_path();
    let parent = path
        .parent()
        .ok_or_else(|| "Theme preferences path has no parent".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let json = serde_json::to_vec_pretty(preferences).map_err(|error| error.to_string())?;
    std::fs::write(path, json).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use gpui_component::ThemeSet;

    use super::BUNDLED_THEMES;

    #[test]
    fn bundled_theme_uses_gpui_component_theme_set_schema() {
        let themes: ThemeSet = serde_json::from_str(BUNDLED_THEMES).unwrap();
        assert!(themes
            .themes
            .iter()
            .any(|theme| theme.name == "Threadlane Dark"));
        assert!(themes
            .themes
            .iter()
            .any(|theme| theme.name == "Threadlane Light"));
    }
}
