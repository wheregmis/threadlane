//! Compatibility shim: the chat surface now lives in
//! `threadlane-ui-chat`. This module re-exports it so existing
//! `crate::screens::chat::...` paths keep working during the split.
pub use threadlane_ui_chat::*;
