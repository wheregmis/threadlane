//! Settings screen for the Threadlane desktop app.
//!
//! `SettingsView` edits provider credentials, model/effort pickers, skills,
//! external agents, and the theme. It also hosts the four
//! `refresh_*_and_update` model-catalog wrappers: they need
//! `Entity<AppState>`, so they live here (rather than in GPUI-free
//! `threadlane-ui-catalog`); the workspace shell imports them directly.

gpui::actions!(threadlane_settings, [ActivateUpdate]);

mod search;
mod view;
pub use search::{SettingsSearchItem, SETTINGS_SEARCH_ITEMS};

pub use view::SettingsView;

pub async fn refresh_discovered_models_and_update(
    model: gpui::Entity<threadlane_ui_state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    threadlane_daemon::catalog::refresh_discovered_models().await;
    refresh_models(model, cx);
}

pub async fn refresh_openai_models_and_update(
    model: gpui::Entity<threadlane_ui_state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    threadlane_daemon::catalog::refresh_openai_models().await;
    refresh_models(model, cx);
}

pub async fn refresh_antigravity_models_and_update(
    model: gpui::Entity<threadlane_ui_state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    threadlane_daemon::catalog::refresh_antigravity_models().await;
    refresh_models(model, cx);
}

pub async fn refresh_acp_models_and_update(
    model: gpui::Entity<threadlane_ui_state::AppState>,
    cx: &mut gpui::AsyncApp,
    project_root: Option<std::path::PathBuf>,
) {
    threadlane_daemon::catalog::refresh_acp_models(project_root).await;
    refresh_models(model, cx);
}

/// Clears every cached provider inventory and re-fetches them all, then
/// refreshes the app's model state. Used by the manual "Refresh models"
/// controls; providers without credentials skip their own fetch.
pub async fn refresh_all_models_and_update(
    model: gpui::Entity<threadlane_ui_state::AppState>,
    cx: &mut gpui::AsyncApp,
    project_root: Option<std::path::PathBuf>,
) {
    threadlane_daemon::catalog::refresh_all_models(project_root).await;
    refresh_models(model, cx);
}

/// Background TTL-guarded refresh of the remote curated snapshot.
pub async fn refresh_catalog_and_update(
    model: gpui::Entity<threadlane_ui_state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    threadlane_daemon::catalog::refresh_curated_catalog().await;
    refresh_models(model, cx);
}

fn refresh_models(model: gpui::Entity<threadlane_ui_state::AppState>, cx: &mut gpui::AsyncApp) {
    let _ = cx.update(|cx| {
        model.update(cx, |state, cx| {
            state.refresh_available_models();
            cx.notify();
        })
    });
}
