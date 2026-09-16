//! Compatibility shim: provider auth now lives in
//! `threadlane-ui-state`. This module re-exports it so existing
//! `crate::services::provider_auth::...` paths keep working during the split.
pub use threadlane_ui_state::provider_auth::*;
