//! File editor view for the Threadlane desktop app.
//!
//! `EditorView` backs the editor tab hosted by the chat surface; the
//! workspace shell binds `SaveFile` to its save keybindings.

mod view;
mod closed_files;
mod lsp_mapping;

pub use view::{detect_language, EditorView, SaveFile};
