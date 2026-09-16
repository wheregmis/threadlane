//! Live computer-use mirror panel for the Threadlane desktop app.
//!
//! `MirrorView` displays the target as it changes plus where the agent's
//! input lands. It displays; it never drives anything itself. The chat
//! surface owns the entity while `AppState::mirror_open` is set.

#![recursion_limit = "8192"]

mod view;

pub use view::MirrorView;
