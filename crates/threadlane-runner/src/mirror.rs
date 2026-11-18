//! Mirroring a run's backend events to the hub's journal (Plan.md §4.6).

use serde_json::json;
use threadlane_core::backend::BackendEvent;
use threadlane_core::ids::RunId;
use threadlane_core::link::{HubLink, LinkError};
use threadlane_core::model::{JournalEntry, Lease};
use threadlane_core::time::Timestamp;
use tracing::debug;

/// Whether an unreachable hub may leave entries unsent for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flush {
    /// Keep the entries and retry with the next event.
    WhenReachable,
    /// Every entry must be appended now.
    Required,
}

/// One run's journal: numbers entries from 1 and keeps those not yet appended.
#[derive(Debug)]
pub(crate) struct Mirror {
    run_id: RunId,
    next_seq: i64,
    unsent: Vec<JournalEntry>,
    /// The session id the backend last reported.
    native_session: Option<String>,
}

impl Mirror {
    pub(crate) fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            next_seq: 1,
            unsent: Vec::new(),
            native_session: None,
        }
    }

    /// The session id the backend last reported in this run, if it did.
    pub(crate) fn native_session(&self) -> Option<&str> {
        self.native_session.as_deref()
    }

    pub(crate) fn record(&mut self, event: &BackendEvent, at: Timestamp) {
        if let BackendEvent::SessionStarted { native_session_id } = event {
            self.native_session = Some(native_session_id.clone());
        }
        let entry = serde_json::to_value(event).unwrap_or_else(
            |error| json!({ "type": "unserializable", "error": error.to_string() }),
        );
        self.unsent.push(JournalEntry {
            run_id: self.run_id,
            seq: self.next_seq,
            at,
            entry,
        });
        self.next_seq += 1;
    }

    /// Appends every unsent entry, in order, in one call.
    pub(crate) async fn flush(
        &mut self,
        link: &dyn HubLink,
        lease: &Lease,
        flush: Flush,
    ) -> Result<(), LinkError> {
        if self.unsent.is_empty() {
            return Ok(());
        }
        match link.append(lease, &self.unsent).await {
            Ok(()) => {
                self.unsent.clear();
                Ok(())
            }
            Err(LinkError::Unreachable) if flush == Flush::WhenReachable => {
                debug!(
                    run = %self.run_id,
                    kept = self.unsent.len(),
                    "hub unreachable; keeping journal entries for the next append"
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}
