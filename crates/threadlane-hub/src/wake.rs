//! The in-process wake mechanism (Plan.md §6.5, docs/milestone-1.md §9).
//!
//! Hints are best effort: every consumer also polls on the hub's `poll`
//! interval, so a lost hint only delays work.

use std::collections::VecDeque;
use std::pin::pin;
use std::sync::{Arc, Mutex, Weak};

use threadlane_core::link::HubMessage;
use threadlane_core::runtime::{WakeTarget, Waker};
use tokio::sync::{Notify, watch};

/// One link's queue of pushed messages.
#[derive(Debug, Default)]
pub(crate) struct Mailbox {
    queue: Mutex<VecDeque<HubMessage>>,
    notify: Notify,
}

impl Mailbox {
    fn push(&self, message: HubMessage) {
        {
            let mut queue = self.queue.lock().expect("mailbox lock poisoned");
            // Nudges carry no data, so one waiting is as good as many.
            if message == HubMessage::Nudge && queue.contains(&HubMessage::Nudge) {
                return;
            }
            queue.push_back(message);
        }
        self.notify.notify_one();
    }

    /// The next message, waiting for one if none is queued.
    pub(crate) async fn next(&self) -> HubMessage {
        loop {
            let mut notified = pin!(self.notify.notified());
            notified.as_mut().enable();
            if let Some(message) = self
                .queue
                .lock()
                .expect("mailbox lock poisoned")
                .pop_front()
            {
                return message;
            }
            notified.await;
        }
    }
}

/// Routes wake hints to links, the dispatcher loop, feed waiters, and open
/// client connections (on revocation).
#[derive(Debug)]
pub(crate) struct WakeHub {
    links: Mutex<Vec<Weak<Mailbox>>>,
    dispatcher: Notify,
    deliveries: watch::Sender<u64>,
    revocations: watch::Sender<u64>,
}

impl Default for WakeHub {
    fn default() -> Self {
        Self {
            links: Mutex::default(),
            dispatcher: Notify::new(),
            deliveries: watch::Sender::new(0),
            revocations: watch::Sender::new(0),
        }
    }
}

impl WakeHub {
    /// A new mailbox that receives every runner nudge from now on.
    pub(crate) fn subscribe(&self) -> Arc<Mailbox> {
        let mailbox = Arc::new(Mailbox::default());
        self.links
            .lock()
            .expect("wake lock poisoned")
            .push(Arc::downgrade(&mailbox));
        mailbox
    }

    /// Pushes a cancellation or wake hint to the links.
    pub(crate) fn push(&self, message: HubMessage) {
        let mut links = self.links.lock().expect("wake lock poisoned");
        links.retain(|link| match link.upgrade() {
            Some(mailbox) => {
                mailbox.push(message.clone());
                true
            }
            None => false,
        });
    }

    /// Completes after the next dispatcher hint. A hint sent while nobody
    /// waits is kept for the next wait.
    pub(crate) async fn dispatcher_woken(&self) {
        self.dispatcher.notified().await;
    }

    /// Tells feed waiters that something was delivered.
    pub(crate) fn delivered(&self) {
        self.deliveries
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// A receiver that sees every delivery after this call.
    pub(crate) fn deliveries(&self) -> watch::Receiver<u64> {
        self.deliveries.subscribe()
    }

    /// Tells open client connections that a key was revoked.
    pub(crate) fn revoked(&self) {
        self.revocations
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// A receiver that sees every revocation after this call.
    pub(crate) fn revocations(&self) -> watch::Receiver<u64> {
        self.revocations.subscribe()
    }
}

impl Waker for WakeHub {
    fn wake(&self, target: WakeTarget) {
        match target {
            WakeTarget::Runners | WakeTarget::Lane(_) => self.push(HubMessage::Nudge),
            WakeTarget::Dispatcher => self.dispatcher.notify_one(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_core::ids::LaneId;

    #[tokio::test]
    async fn nudges_reach_every_live_link_and_coalesce() {
        let hub = WakeHub::default();
        let first = hub.subscribe();
        let second = hub.subscribe();
        hub.wake(WakeTarget::Lane(LaneId(1)));
        hub.wake(WakeTarget::Runners);
        assert_eq!(first.next().await, HubMessage::Nudge);
        assert_eq!(second.next().await, HubMessage::Nudge);
        assert!(first.queue.lock().unwrap().is_empty(), "nudges coalesce");
        drop(second);
        hub.wake(WakeTarget::Runners);
        assert_eq!(hub.links.lock().unwrap().len(), 1, "dead links are dropped");
    }

    #[tokio::test]
    async fn a_dispatcher_hint_before_the_wait_is_kept() {
        let hub = WakeHub::default();
        hub.wake(WakeTarget::Dispatcher);
        hub.dispatcher_woken().await;
    }
}
