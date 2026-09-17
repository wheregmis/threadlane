//! GitHub screen for the Threadlane desktop app.
//!
//! `GitHubView` aggregates issues and pull requests across the scoped
//! projects, with diff viewing, review timelines, and issue-to-session
//! handoff. Module layout mirrors the former `screens::github` tree so
//! paths like `github::view::init` keep resolving.

mod diff;
mod issue_dialog;
mod timeline;
mod types;
pub mod view;

pub use view::GitHubView;
