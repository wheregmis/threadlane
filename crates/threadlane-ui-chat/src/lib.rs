mod composer;
mod context_meter;
mod markdown;
mod trajectory;
mod transcript;
mod view;

pub use view::{init, CentralTab, ChatListView};

// Owned by the surface that offers it; the workspace handles panel navigation.
gpui::actions!(threadlane_chat, [OpenWorkspaceReview, OpenWorkspaceFiles]);
