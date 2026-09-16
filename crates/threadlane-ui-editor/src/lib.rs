//! File editor view for the Threadlane desktop app.
//!
//! `EditorView` backs the editor tab hosted by the chat surface; the
//! workspace shell binds `SaveFile` to its save keybindings.

mod view;

pub use view::{EditorView, SaveFile};
