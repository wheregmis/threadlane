#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{px, App, Hsla, Pixels, SharedString};
use gpui_component::{ActiveTheme, Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use serde::{Deserialize, Serialize};

#[cfg(not(target_family = "wasm"))]
use threadlane_project::global_threadlane_dir;

const DEFAULT_THEME_NAME: &str = "Threadlane Dark";
const BUNDLED_THEMES: &str = include_str!("../themes/threadlane.json");

/// Top clearance reserved under the macOS traffic lights (positioned at
/// y=12 in a frameless window). All panel headers share this inset so
/// coincident header content forms one continuous line. This is a physical
/// platform-window boundary, hence fixed pixels rather than `rem`.
///
/// Where the app draws its own caption strip ([`APP_CAPTION_STRIP`]) nothing
/// sits under the headers, so they only need a compact row height.
pub const WINDOW_CONTROLS_CLEARANCE: Pixels = if APP_CAPTION_STRIP {
    px(40.0)
} else {
    px(48.0)
};

/// Leading header space, in rem, for window controls and the sidebar toggle
/// when the sidebar is hidden. Shared by chat and compact inspector headers.
/// With a caption strip there are no traffic lights, only the toggle.
pub const WINDOW_CONTROLS_CONTENT_INSET: f32 = if APP_CAPTION_STRIP { 2.75 } else { 6.875 };

/// Leading offset, in rem, of the floating sidebar toggle: after the traffic
/// lights on macOS, at the content edge under a caption strip.
pub const SIDEBAR_TOGGLE_LEFT: f32 = if APP_CAPTION_STRIP { 0.625 } else { 4.75 };

/// True where the frameless window has no native caption (Windows): the
/// shell draws a caption strip with the window controls, drag area and
/// sidebar toggle. macOS keeps its traffic lights in the transparent
/// titlebar; the web preview has no window chrome. Linux is excluded: under
/// server-side decorations (most X11, some Wayland compositors) the window
/// manager already draws a title bar, and the strip would stack a second one.
pub const APP_CAPTION_STRIP: bool = cfg!(target_os = "windows");

/// Shared reading width for user messages, relative to the interface font size.
pub const USER_BUBBLE_MAX_WIDTH: f32 = 40.0;

/// Shared maximum reading widths for session surfaces, in rem.
pub const CHAT_CONTENT_MAX_WIDTH: f32 = 48.0;
pub const QUESTION_CARD_MAX_WIDTH: f32 = 32.0;

/// Dimming scrim behind modal overlays (permission details, dialogs).
/// Defined once here so every overlay dims identically in any theme.
pub fn overlay_scrim() -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.0,
        a: 0.6,
    }
}

#[derive(Default, Deserialize, Serialize)]
struct ThemePreferences {
    selected_theme: Option<String>,
}

pub fn init(cx: &mut App) {
    init_bundled(cx);

    #[cfg(not(target_family = "wasm"))]
    {
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
}

/// Register bundled themes and apply the saved/default selection, without
/// the `~/.threadlane/themes` directory watch — for platforms where that
/// directory is unavailable or unwatchable (the iOS client).
pub fn init_bundled(cx: &mut App) {
    // Every host uses the same licensed font data, including GPUI Web, which
    // has no access to native system fonts. Load every UI weight and its italic
    // face so hierarchy does not depend on synthetic styles or system fallback.
    // Custom theme typography can override it.
    if let Err(error) = cx.text_system().add_fonts(vec![
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Bold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Italic.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-MediumItalic.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-SemiBoldItalic.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-BoldItalic.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
    ]) {
        tracing::error!(?error, "failed to register bundled Threadlane fonts");
    }
    Theme::update(cx, |theme| {
        theme.font_family = "IBM Plex Sans".into();
        theme.mono_font_family = "JetBrains Mono".into();
    });
    register_bundled_themes(cx);
    apply_saved_or_default_theme(cx);
}

pub fn active_theme_name(cx: &App) -> SharedString {
    cx.theme().theme_name().clone()
}

pub fn apply_theme(theme_name: &str, cx: &mut App) -> bool {
    if !preview_theme(theme_name, cx) {
        return false;
    }
    #[cfg(not(target_family = "wasm"))]
    if let Err(error) = save_preferences(&ThemePreferences {
        selected_theme: Some(theme_name.to_string()),
    }) {
        tracing::warn!("failed to save selected theme: {error}");
    }
    true
}

/// Apply a theme to this app instance without writing the user's preference.
pub fn preview_theme(theme_name: &str, cx: &mut App) -> bool {
    let Some(theme) = find_theme(theme_name, cx) else {
        return false;
    };

    apply_theme_config(theme, cx);
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

#[cfg(not(target_family = "wasm"))]
fn preferences_path() -> PathBuf {
    global_threadlane_dir().join("gui").join("preferences.json")
}

fn load_preferences() -> ThemePreferences {
    #[cfg(target_family = "wasm")]
    {
        // Browser previews use the bundled default and never access host preferences.
        ThemePreferences::default()
    }
    #[cfg(not(target_family = "wasm"))]
    std::fs::read(preferences_path())
        .ok()
        .and_then(|contents| serde_json::from_slice(&contents).ok())
        .unwrap_or_default()
}

#[cfg(not(target_family = "wasm"))]
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

    #[test]
    fn bundled_theme_switches_restore_shared_typography() {
        let themes: ThemeSet = serde_json::from_str(BUNDLED_THEMES).unwrap();
        let mut theme = gpui_component::Theme::default();
        for config in themes.themes {
            // A prior custom theme must not leak platform fonts or sizing into
            // either bundled theme, including after a registry reload.
            theme.font_family = ".SystemUIFont".into();
            theme.mono_font_family = "monospace".into();
            theme.font_size = gpui::px(20.);
            theme.mono_font_size = gpui::px(18.);
            theme.apply_config(&std::rc::Rc::new(config));
            assert_eq!(theme.font_family, "IBM Plex Sans");
            assert_eq!(theme.mono_font_family, "JetBrains Mono");
            assert_eq!(theme.font_size, gpui::px(16.));
            assert_eq!(theme.mono_font_size, gpui::px(13.));
        }
    }
}
