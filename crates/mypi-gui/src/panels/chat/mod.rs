//! Chat panel public API and exports.

pub mod components;
pub mod events;
pub mod state;
pub mod view;

pub use components::ToolFoldHeader;
pub use state::{
    truncate_chars, ChatData, ChatMessage, MsgRole, StreamingKind, ToolPresentation, ToolStatus,
};
pub use view::ChatList;
