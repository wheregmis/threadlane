//! Right-side panel for the Threadlane desktop app.
//!
//! `RightPanelView` hosts the git review, draft-PR, browser, and file
//! surfaces. Module layout mirrors the former `screens::right_panel` tree.

mod browser;
mod agents;
mod draft_pr;
#[cfg(test)]
mod tests;
mod types;
mod view;

pub use types::{DiscardOption, GitAction, ReviewTab, ReviewViewMode, Surface};
pub use view::RightPanelView;
