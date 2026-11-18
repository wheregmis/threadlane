//! What one running lane shares with its tool endpoint and the heartbeat.

use std::future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use threadlane_core::link::LinkError;
use threadlane_core::model::Lease;
use threadlane_core::runtime::Killed;
use tokio::sync::watch;

/// Why a lane must stop before its cycle ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The runner is shutting down: stop the backend and release the lease.
    Shutdown,
    /// The user stopped this lane.
    User,
    /// The lease is gone: stop the backend and write nothing more.
    LeaseLost,
    /// The process died (crash injection): stop at once, touch nothing.
    Killed(Killed),
}

impl Stop {
    /// A stronger stop replaces a weaker one, never the reverse.
    fn strength(self) -> u8 {
        match self {
            Self::Shutdown => 0,
            Self::User => 1,
            Self::LeaseLost => 2,
            Self::Killed(_) => 3,
        }
    }
}

/// How a lane's cycle ended early.
#[derive(Debug)]
pub(crate) enum LaneExit {
    Killed(Killed),
    LeaseLost,
    Shutdown,
    UserStopped,
    /// A hub call failed mid-run: the run must still be failed, then released.
    Link(LinkError),
    /// A hub call failed after the run was dealt with: release the lane.
    Release(LinkError),
}

impl From<Killed> for LaneExit {
    fn from(killed: Killed) -> Self {
        Self::Killed(killed)
    }
}

impl From<Stop> for LaneExit {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Shutdown => Self::Shutdown,
            Stop::User => Self::UserStopped,
            Stop::LeaseLost => Self::LeaseLost,
            Stop::Killed(killed) => Self::Killed(killed),
        }
    }
}

impl From<LinkError> for LaneExit {
    fn from(error: LinkError) -> Self {
        match error {
            LinkError::Killed(killed) => Self::Killed(killed),
            LinkError::Stopped => Self::UserStopped,
            LinkError::StaleLease { .. } => Self::LeaseLost,
            other => Self::Link(other),
        }
    }
}

/// The lease a lane holds, which heartbeats renew, and its stop signal.
#[derive(Debug)]
pub(crate) struct LaneState {
    lease: Mutex<Lease>,
    stop: watch::Sender<Option<Stop>>,
    /// The current run opened a decision: it must end its turn now
    /// (Plan.md §9.4).
    deferred: AtomicBool,
    /// The current run wrote its task's progress file, as the hub said in a
    /// tool result (docs/milestone-5.md §1 decision 7).
    progress_written: AtomicBool,
}

impl LaneState {
    pub(crate) fn new(lease: Lease) -> Self {
        let (stop, _) = watch::channel(None);
        Self {
            lease: Mutex::new(lease),
            stop,
            deferred: AtomicBool::new(false),
            progress_written: AtomicBool::new(false),
        }
    }

    /// Marks the current run as waiting on a decision, or, with `false`,
    /// starts a run with none.
    pub(crate) fn set_deferred(&self, deferred: bool) {
        self.deferred.store(deferred, Ordering::SeqCst);
    }

    pub(crate) fn deferred(&self) -> bool {
        self.deferred.load(Ordering::SeqCst)
    }

    /// Marks the current run as having written its progress file, or, with
    /// `false`, starts a run that has not.
    pub(crate) fn set_progress_written(&self, written: bool) {
        self.progress_written.store(written, Ordering::SeqCst);
    }

    pub(crate) fn progress_written(&self) -> bool {
        self.progress_written.load(Ordering::SeqCst)
    }

    /// The current lease, as last renewed.
    pub(crate) fn lease(&self) -> Lease {
        *self.lease.lock().expect("lane lease lock poisoned")
    }

    pub(crate) fn renew(&self, lease: Lease) {
        *self.lease.lock().expect("lane lease lock poisoned") = lease;
    }

    /// Tells the lane to stop, unless it was already told something stronger.
    pub(crate) fn signal(&self, stop: Stop) {
        self.stop.send_if_modified(|current| {
            if current.is_some_and(|held| held.strength() >= stop.strength()) {
                return false;
            }
            *current = Some(stop);
            true
        });
    }

    pub(crate) fn current(&self) -> Option<Stop> {
        *self.stop.borrow()
    }

    /// Fails if the lane has been told to stop.
    pub(crate) fn check(&self) -> Result<(), LaneExit> {
        self.current().map_or(Ok(()), |stop| Err(stop.into()))
    }

    /// Completes once the lane is told to stop.
    pub(crate) async fn stopped(&self) -> Stop {
        let mut receiver = self.stop.subscribe();
        let stop = receiver
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|stop| *stop);
        match stop {
            Some(stop) => stop,
            // The sender lives as long as `self`, so this is never reached.
            None => future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use threadlane_core::ids::{LaneId, MachineId};
    use threadlane_core::runtime::Step;
    use threadlane_core::time::Timestamp;

    use super::*;

    fn lane() -> LaneState {
        LaneState::new(Lease {
            lane_id: LaneId(1),
            machine_id: MachineId(1),
            epoch: 1,
            expires_at: Timestamp::EPOCH,
        })
    }

    #[tokio::test]
    async fn a_stronger_stop_wins_and_is_never_downgraded() {
        let lane = lane();
        assert!(lane.check().is_ok());

        lane.signal(Stop::Shutdown);
        lane.signal(Stop::LeaseLost);
        let killed = Stop::Killed(Killed {
            step: Step("test.kill"),
        });
        lane.signal(killed);
        lane.signal(Stop::LeaseLost);
        lane.signal(Stop::Shutdown);

        assert_eq!(lane.current(), Some(killed));
        assert_eq!(lane.stopped().await, killed);
        assert!(matches!(lane.check(), Err(LaneExit::Killed(_))));
    }
}
