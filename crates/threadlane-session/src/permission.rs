//! Session permission manager: compatibility re-exports.
//!
//! The manager, handle, decisions, trace events, and persistent grants are
//! canonical in `threadlane_permission` and re-exported below for
//! compatibility. New code should import `threadlane_permission` directly.

pub use threadlane_permission::*;
