//! Project/session sidebar for the Threadlane desktop app.
//!
//! `SidebarView` renders attached projects, session history, and the new-task
//! entry points. It also owns the keybindable `BeginNewTask` action, which
//! the workspace shell binds to cmd/ctrl-N: the action type must be shared,
//! so it lives here rather than in the shell.

use gpui::actions;

actions!(threadlane_sidebar, [BeginNewTask]);

mod view;

pub use view::SidebarView;
