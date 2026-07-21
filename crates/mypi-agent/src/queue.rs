use crate::types::{AgentMessage, QueueMode};

#[derive(Debug, Clone)]
pub struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    pub mode: QueueMode,
}

impl PendingMessageQueue {
    pub fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    pub fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    pub fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn drain(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => std::mem::take(&mut self.messages),
            QueueMode::OneAtATime => {
                if self.messages.is_empty() {
                    Vec::new()
                } else {
                    vec![self.messages.remove(0)]
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}
