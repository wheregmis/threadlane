//! Sessions panel public API and exports.

pub mod components;
pub mod state;
pub mod view;

pub use components::{SessionContextMenu, SessionContextMenuAction};
pub use state::{
    active_session_entry, archive_session, create_new_session, delete_session, is_session_working,
    project_work_dir_at_row, refresh_sessions, session_entry_at_row, set_active_session,
    set_session_context_target, set_session_working, SessionEntry,
};
pub use view::SessionList;
