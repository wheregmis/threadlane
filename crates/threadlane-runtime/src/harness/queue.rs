//! Lane queue priority levels.
//!
//! Durable scheduling state lives in `QueueEnqueued` harness records;
//! the in-memory staging buffer was removed as dead code.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SteerPriority {
    Low = 0,
    Normal = 1,
    High = 2,
}
