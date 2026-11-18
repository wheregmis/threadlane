//! A clock that only moves when a test moves it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use threadlane_core::time::{Clock, Sleep, Timestamp};
use tokio::sync::oneshot;

#[derive(Debug)]
struct State {
    now: Timestamp,
    sleepers: Vec<(Timestamp, oneshot::Sender<()>)>,
}

/// A deterministic [`Clock`]. Sleepers wake when [`FakeClock::advance`] or
/// [`FakeClock::set`] moves time to or past their deadline.
#[derive(Debug, Clone)]
pub struct FakeClock {
    state: Arc<Mutex<State>>,
}

impl FakeClock {
    /// A clock starting at `start`.
    #[must_use]
    pub fn new(start: Timestamp) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                now: start,
                sleepers: Vec::new(),
            })),
        }
    }

    /// A clock starting at 2026-01-01T00:00:00Z.
    #[must_use]
    pub fn at_default_start() -> Self {
        Self::new(Timestamp::from_millis(1_767_225_600_000))
    }

    /// Moves time forward and wakes every sleeper that is now due.
    pub fn advance(&self, by: Duration) {
        let target = self.now() + by;
        self.set(target);
    }

    /// Moves time to `to` (never backwards) and wakes due sleepers.
    ///
    /// # Panics
    ///
    /// Panics if `to` is earlier than the current time.
    pub fn set(&self, to: Timestamp) {
        let due = {
            let mut state = self.state.lock().expect("clock lock poisoned");
            assert!(
                to >= state.now,
                "fake clock cannot go backwards from {} to {to}",
                state.now
            );
            state.now = to;
            let (due, waiting): (Vec<_>, Vec<_>) = state
                .sleepers
                .drain(..)
                .partition(|(deadline, _)| *deadline <= to);
            state.sleepers = waiting;
            due
        };
        for (_, sender) in due {
            // The sleeper may have been dropped; that is fine.
            let _ = sender.send(());
        }
    }

    /// Number of sleepers still waiting for a future time. A sleep whose
    /// future was dropped is not counted.
    #[must_use]
    pub fn sleepers(&self) -> usize {
        let mut state = self.state.lock().expect("clock lock poisoned");
        state.sleepers.retain(|(_, sender)| !sender.is_closed());
        state.sleepers.len()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        self.state.lock().expect("clock lock poisoned").now
    }

    fn sleep_until(&self, deadline: Timestamp) -> Sleep {
        let mut state = self.state.lock().expect("clock lock poisoned");
        if deadline <= state.now {
            return Box::pin(std::future::ready(()));
        }
        let (sender, receiver) = oneshot::channel();
        state.sleepers.push((deadline, sender));
        Box::pin(async move {
            // A dropped clock never wakes its sleepers, like a stopped process.
            if receiver.await.is_err() {
                std::future::pending::<()>().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sleepers_wake_only_when_time_reaches_their_deadline() {
        let clock = FakeClock::at_default_start();
        let start = clock.now();
        let early = tokio::spawn(clock.sleep(Duration::from_secs(10)));
        let late = tokio::spawn(clock.sleep(Duration::from_secs(60)));
        tokio::task::yield_now().await;
        assert_eq!(clock.sleepers(), 2);

        clock.advance(Duration::from_secs(30));
        early.await.expect("early sleeper finishes");
        assert!(!late.is_finished());
        assert_eq!(clock.now(), start + Duration::from_secs(30));

        clock.advance(Duration::from_secs(30));
        late.await.expect("late sleeper finishes");
        assert_eq!(clock.sleepers(), 0);
    }

    #[tokio::test]
    async fn past_deadlines_are_ready_immediately() {
        let clock = FakeClock::at_default_start();
        clock.sleep_until(clock.now()).await;
        clock.sleep(Duration::ZERO).await;
    }
}
