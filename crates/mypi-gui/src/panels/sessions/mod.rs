//! Sessions panel public API and exports.

pub mod components;
pub mod state;
pub mod view;

pub use components::{SessionContextMenu, SessionContextMenuAction};
pub use state::{
    active_session_entry, archive_session, create_new_session, delete_session, refresh_sessions,
    session_entry_at_row, set_active_session, set_session_context_target, set_sessions_working,
    SessionEntry,
};
pub use view::SessionList;
