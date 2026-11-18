pub mod assets;
pub mod theme;
#[cfg(target_family = "wasm")]
mod web_assets;

pub use assets::{bundled_icon, vendored_icon_bytes, Assets};
pub use theme::{
    active_theme_name, apply_theme, init, init_bundled, overlay_scrim, preview_theme,
    CHAT_CONTENT_MAX_WIDTH, QUESTION_CARD_MAX_WIDTH, USER_BUBBLE_MAX_WIDTH, WINDOW_CONTROLS_CLEARANCE,
    WINDOW_CONTROLS_CONTENT_INSET, APP_CAPTION_STRIP, SIDEBAR_TOGGLE_LEFT,
};
