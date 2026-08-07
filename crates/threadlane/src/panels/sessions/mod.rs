//! Sessions panel public API and exports.

pub mod components;
pub mod project_registry;
pub mod state;
pub mod view;

pub use components::{SessionContextMenu, SessionContextMenuAction};
pub use project_registry::ProjectRegistry;
pub use state::{
    active_session_entry, archive_session, begin_title_generation, create_new_session,
    delete_session, end_title_generation, is_project_working, is_session_working,
    normalize_session_title, project_work_dir_at_row, refresh_sessions, session_entry_at_row,
    session_entry_for_file, session_health, session_overflow_at_row, session_title_eligible,
    set_active_project, set_active_session, set_search_query, set_session_context_target,
    set_session_health, set_session_working, title_prompt_for_submission, toggle_project_collapsed,
    toggle_project_show_all, SessionEntry,
};
pub use view::{SessionList, SessionListAction};
