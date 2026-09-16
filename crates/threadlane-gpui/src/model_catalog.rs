//! Compatibility shim: the picker-facing catalog now lives in
//! `threadlane-ui-catalog`. This module re-exports the pure catalog and
//! keeps the four GPUI refresh wrappers that need `AppState`.
pub use threadlane_ui_catalog::*;

/// Refreshes the live Zen list, then rebuilds the picker's model list.
/// Call from a background task at startup and after the OpenCode key changes.
pub async fn refresh_discovered_models_and_update(
    model: gpui::Entity<crate::state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    refresh_discovered_models().await;
    let _ = cx.update(|cx| {
        model.update(cx, |state, cx| {
            state.refresh_available_models();
            cx.notify();
        })
    });
}

/// Refreshes the live OpenAI list, then rebuilds the picker's model list.
pub async fn refresh_openai_models_and_update(
    model: gpui::Entity<crate::state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    refresh_openai_models().await;
    let _ = cx.update(|cx| {
        model.update(cx, |state, cx| {
            state.refresh_available_models();
            cx.notify();
        })
    });
}

/// Refreshes the live Antigravity inventory, then rebuilds the picker's
/// model list.
pub async fn refresh_antigravity_models_and_update(
    model: gpui::Entity<crate::state::AppState>,
    cx: &mut gpui::AsyncApp,
) {
    refresh_antigravity_models().await;
    let _ = cx.update(|cx| {
        model.update(cx, |state, cx| {
            state.refresh_available_models();
            cx.notify();
        })
    });
}

/// Refreshes the cached external-agent settings, then rebuilds the picker's
/// model list. Call from a background task at startup and whenever the
/// active project changes.
pub async fn refresh_acp_models_and_update(
    model: gpui::Entity<crate::state::AppState>,
    cx: &mut gpui::AsyncApp,
    project_root: Option<std::path::PathBuf>,
) {
    refresh_acp_models(project_root).await;
    let _ = cx.update(|cx| {
        model.update(cx, |state, cx| {
            state.refresh_available_models();
            cx.notify();
        })
    });
}
