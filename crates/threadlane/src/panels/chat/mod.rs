//! Chat panel public API and exports.

pub mod components;
mod composer;
pub mod state;
pub mod view;

pub use components::ToolFoldHeader;
pub use composer::{
    accepts_generation_event, concise_status, draft_for_cancellation, submitted_draft,
    ComposerState, ComposerStatus, GenerationEvent,
};
#[cfg(test)]
pub use state::ChatMessage;
pub use state::{ChatData, MsgRole, StreamingKind, ToolStatus};
pub use view::{
    ChatList, ChatListWidgetRefExt, StarterPromptAction, SubagentRail, SubagentRailAction,
};
