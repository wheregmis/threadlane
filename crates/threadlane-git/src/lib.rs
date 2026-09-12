//! Project-local Git operations used by threadlane workspace.
//!
//! Git is intentionally invoked through the user's configured `git` executable
//! so existing credential helpers, SSH keys, remotes, hooks, and repository
//! configuration continue to work unchanged.

pub mod error;
pub mod git;
pub mod github;
pub mod pr_review;
pub mod types;

#[cfg(test)]
mod tests;

pub use error::*;
pub use git::*;
pub use github::*;
pub use pr_review::*;
pub use types::*;
