//! Per-lane in-memory message queue with priority steering.
//!
//! These types were previously in `crate::op_log`; they are moved here
//! because they belong with the harness queue infrastructure.

use crate::types::AgentMessage;
use serde::{Deserialize, Serialize};
use std::collections::{BinaryHeap, VecDeque};

use super::QueueKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SteerPriority {
    Low = 0,
    Normal = 1,
    High = 2,
}

/// Upper bound per queue so an in-memory staging buffer cannot grow without
/// limit. Durable scheduling state lives in `QueueEnqueued` harness records;
/// this buffer is presentation/staging only and never the source of truth.
pub const MAX_QUEUE_DEPTH: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteerItem {
    message: AgentMessage,
    priority: SteerPriority,
    timestamp_ms: u128,
    /// Monotonic tiebreaker: FIFO among equal `(priority, timestamp_ms)` and
    /// keeps `Ord` consistent with `Eq`.
    #[serde(default)]
    seq: u64,
}

impl PartialEq for SteerItem {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
            && self.timestamp_ms == other.timestamp_ms
            && self.seq == other.seq
            && self.message == other.message
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
        // BinaryHeap is a max-heap: the "greatest" item pops first, so
        // higher priority must compare greater, with earlier timestamps
        // (and lower seq) winning ties.
        match self.priority.cmp(&other.priority) {
            std::cmp::Ordering::Equal => match other.timestamp_ms.cmp(&self.timestamp_ms) {
                std::cmp::Ordering::Equal => other.seq.cmp(&self.seq),
                ord => ord,
            },
            ord => ord,
        }
    }
}

/// Per-lane in-memory staging queue for steering and follow-ups.
///
/// This buffer is not durable: scheduled work that must survive restarts
/// belongs in `QueueEnqueued` harness records. Every sub-queue can be
/// drained (`pop_steer` / `pop_follow_up` / `pop_next_run` / `pop`), so a
/// filled steer lane no longer leaks behind a test-only drain.
#[derive(Debug, Clone, Default)]
pub struct LaneQueue {
    steer: BinaryHeap<SteerItem>,
    follow_up: VecDeque<AgentMessage>,
    next_run: VecDeque<AgentMessage>,
    next_seq: u64,
}

impl LaneQueue {
    pub fn enqueue(&mut self, kind: QueueKind, message: AgentMessage) {
        match kind {
            QueueKind::Steer => self.enqueue_steer_with_priority(message, SteerPriority::Normal),
            QueueKind::FollowUp => {
                if self.follow_up.len() >= MAX_QUEUE_DEPTH {
                    self.follow_up.pop_front();
                }
                self.follow_up.push_back(message);
            }
            QueueKind::NextRun => {
                if self.next_run.len() >= MAX_QUEUE_DEPTH {
                    self.next_run.pop_front();
                }
                self.next_run.push_back(message);
            }
        }
    }

    fn enqueue_steer_with_priority(&mut self, message: AgentMessage, priority: SteerPriority) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.steer.push(SteerItem {
            message,
            priority,
            timestamp_ms,
            seq,
        });
        // Bound the heap by evicting the lowest-priority oldest item, which
        // is the most expensive to locate; overflow is unexpected (durable
        // records are the real queue), so keep this path simple.
        if self.steer.len() > MAX_QUEUE_DEPTH {
            let mut items = std::mem::take(&mut self.steer).into_vec();
            if let Some(victim) = items
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    a.priority
                        .cmp(&b.priority)
                        .then_with(|| a.timestamp_ms.cmp(&b.timestamp_ms))
                        .then_with(|| a.seq.cmp(&b.seq))
                })
                .map(|(index, _)| index)
            {
                items.swap_remove(victim);
            }
            self.steer = items.into_iter().collect();
        }
    }

    pub fn pop_steer(&mut self) -> Option<AgentMessage> {
        self.steer.pop().map(|item| item.message)
    }

    pub fn pop_follow_up(&mut self) -> Option<AgentMessage> {
        self.follow_up.pop_front()
    }

    pub fn pop_next_run(&mut self) -> Option<AgentMessage> {
        self.next_run.pop_front()
    }

    /// Drain the next item for one queue kind.
    pub fn pop(&mut self, kind: QueueKind) -> Option<AgentMessage> {
        match kind {
            QueueKind::Steer => self.pop_steer(),
            QueueKind::FollowUp => self.pop_follow_up(),
            QueueKind::NextRun => self.pop_next_run(),
        }
    }

    pub fn len(&self) -> usize {
        self.steer.len() + self.follow_up.len() + self.next_run.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
