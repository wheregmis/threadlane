//! Per-lane in-memory message queue with priority steering.
//!
//! These types were previously in `crate::op_log`; they are moved here
//! because they belong with the harness queue infrastructure.

use crate::types::AgentMessage;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use super::QueueKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SteerPriority {
    Low = 0,
    Normal = 1,
    High = 2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteerItem {
    message: AgentMessage,
    priority: SteerPriority,
    timestamp_ms: u128,
}

impl PartialEq for SteerItem {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.timestamp_ms == other.timestamp_ms
    }
}

impl Eq for SteerItem {}

impl PartialOrd for SteerItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SteerItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.priority.cmp(&other.priority).reverse() {
            std::cmp::Ordering::Equal => self.timestamp_ms.cmp(&other.timestamp_ms),
            ord => ord,
        }
    }
}

/// A per-lane in-memory message queue supporting steering and follow-ups.
#[derive(Debug, Clone, Default)]
pub struct LaneQueue {
    steer: Vec<SteerItem>,
    follow_up: VecDeque<AgentMessage>,
    next_run: VecDeque<AgentMessage>,
}

impl LaneQueue {
    pub fn enqueue(&mut self, kind: QueueKind, message: AgentMessage) {
        match kind {
            QueueKind::Steer => self.enqueue_steer_with_priority(message, SteerPriority::Normal),
            QueueKind::FollowUp => self.follow_up.push_back(message),
            QueueKind::NextRun => self.next_run.push_back(message),
        }
    }

    fn enqueue_steer_with_priority(&mut self, message: AgentMessage, priority: SteerPriority) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        self.steer.push(SteerItem {
            message,
            priority,
            timestamp_ms,
        });
        self.steer.sort();
    }

    #[cfg(test)]
    fn pop_steer(&mut self) -> Option<AgentMessage> {
        if self.steer.is_empty() {
            None
        } else {
            Some(self.steer.remove(0).message)
        }
    }

    pub fn pop_follow_up(&mut self) -> Option<AgentMessage> {
        self.follow_up.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.steer.is_empty() && self.follow_up.is_empty() && self.next_run.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentMessage;

    #[test]
    fn priority_steer_queue_ordering() {
        let mut queue = LaneQueue::default();
        queue.enqueue_steer_with_priority(
            AgentMessage::User {
                content: "normal".into(),
            },
            SteerPriority::Normal,
        );
        queue.enqueue_steer_with_priority(
            AgentMessage::User {
                content: "low".into(),
            },
            SteerPriority::Low,
        );
        queue.enqueue_steer_with_priority(
            AgentMessage::User {
                content: "high interrupt".into(),
            },
            SteerPriority::High,
        );

        let popped1 = queue.pop_steer().unwrap();
        let popped2 = queue.pop_steer().unwrap();
        let popped3 = queue.pop_steer().unwrap();

        assert_eq!(popped1.role_str(), "user");
        assert_eq!(popped2.role_str(), "user");
        assert_eq!(popped3.role_str(), "user");

        if let AgentMessage::User { content } = popped1 {
            assert_eq!(content, "high interrupt");
        }
        if let AgentMessage::User { content } = popped2 {
            assert_eq!(content, "normal");
        }
        if let AgentMessage::User { content } = popped3 {
            assert_eq!(content, "low");
        }
    }
}
