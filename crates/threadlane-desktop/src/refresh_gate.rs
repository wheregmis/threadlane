//! Coordinates complete desktop refreshes without blocking the UI.

#[derive(Default)]
pub(crate) struct RefreshGate {
    running: bool,
    queued: bool,
    pairing_revision: u64,
}

impl RefreshGate {
    /// A timer tick need not repeat a healthy in-flight refresh. Explicit
    /// requests keep one trailing pass, never one pass per event.
    pub(crate) fn begin(&mut self, explicit: bool) -> Option<u64> {
        if self.running {
            self.queued |= explicit;
            return None;
        }
        self.running = true;
        Some(self.pairing_revision)
    }

    /// A response from the previous set of pairings must not restore a
    /// removed hub or drop a newly paired hub from the desktop.
    pub(crate) fn pairing_changed(&mut self) {
        self.pairing_revision = self.pairing_revision.wrapping_add(1);
    }

    /// Returns whether to apply the response and whether to start another pass.
    pub(crate) fn finish(&mut self, revision: u64) -> (bool, bool) {
        self.running = false;
        (
            revision == self.pairing_revision,
            std::mem::take(&mut self.queued),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::RefreshGate;

    #[test]
    fn explicit_burst_queues_only_one_trailing_pass() {
        let mut gate = RefreshGate::default();
        let initial = gate.begin(false).unwrap();
        for _ in 0..100 {
            assert_eq!(gate.begin(true), None);
            assert_eq!(gate.begin(false), None);
        }
        assert_eq!(gate.finish(initial), (true, true));
        let trailing = gate.begin(true).unwrap();
        assert_eq!(gate.finish(trailing), (true, false));
    }

    #[test]
    fn polling_does_not_queue_a_trailing_pass() {
        let mut gate = RefreshGate::default();
        let initial = gate.begin(false).unwrap();
        assert_eq!(gate.begin(false), None);
        assert_eq!(gate.finish(initial), (true, false));
    }

    #[test]
    fn pairing_change_discards_the_old_hub_snapshot() {
        let mut gate = RefreshGate::default();
        let old = gate.begin(false).unwrap();
        gate.pairing_changed();
        assert_eq!(gate.begin(true), None);
        assert_eq!(gate.finish(old), (false, true));

        let current = gate.begin(true).unwrap();
        assert_ne!(current, old);
        assert_eq!(gate.finish(current), (true, false));
    }
}
