//! Workspace shell for the Threadlane desktop app.
//!
//! `WorkspaceView` is the window root: it composes the chat surface,
//! sidebar, right panel, terminal tabs, settings, and GitHub views over one
//! `AppState`, and owns the global keybindings and background event pumps.
//! It is the last screen extracted from `threadlane-gpui`; the binary crate
//! constructs it directly.

mod view;

pub use view::{init, WorkspaceView};
