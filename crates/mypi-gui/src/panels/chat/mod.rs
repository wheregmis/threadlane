//! Chat panel public API and exports.

pub mod components;
mod composer;
pub mod events;
pub mod state;
pub mod view;

pub use components::ToolFoldHeader;
pub use composer::{
    accepts_generation_event, concise_status, draft_for_cancellation, ComposerPresentation,
    ComposerState, ComposerStatus, GenerationEvent,
};
pub use state::{
    truncate_chars, ChatData, ChatMessage, MsgRole, StreamingKind, ToolPresentation, ToolStatus,
};
pub use view::ChatList;
