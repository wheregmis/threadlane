//! Time as the system sees it, and the replaceable [`Clock`] (Plan.md §3.4).

use std::fmt;
use std::future::Future;
use std::ops::{Add, Sub};
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch, UTC.
///
/// Stored as an integer column everywhere so ordering and arithmetic in SQL
/// match ordering and arithmetic in Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(0);

    /// Builds a timestamp from milliseconds since the Unix epoch.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// Time elapsed from `earlier` to `self`, or zero if `earlier` is later.
    #[must_use]
    pub fn saturating_since(self, earlier: Self) -> Duration {
        u64::try_from(self.0 - earlier.0).map_or(Duration::ZERO, Duration::from_millis)
    }
}

fn duration_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

impl Add<Duration> for Timestamp {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self {
        Self(self.0.saturating_add(duration_millis(rhs)))
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Self;

    fn sub(self, rhs: Duration) -> Self {
        Self(self.0.saturating_sub(duration_millis(rhs)))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

/// A boxed, sendable future returned by [`Clock::sleep_until`].
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The current time, replaceable with a fake in tests.
///
/// Components never read the system clock directly. Anything that waits does
/// so through [`Clock::sleep_until`], so a fake clock can drive multi-day
/// scenarios in milliseconds (Plan.md §16).
pub trait Clock: Send + Sync + fmt::Debug {
    /// The current time.
    fn now(&self) -> Timestamp;

    /// Completes once [`Clock::now`] is at or after `deadline`.
    fn sleep_until(&self, deadline: Timestamp) -> Sleep;

    /// Completes once `duration` has elapsed on this clock.
    fn sleep(&self, duration: Duration) -> Sleep {
        self.sleep_until(self.now() + duration)
    }
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is set after 1970");
        Timestamp(duration_millis(since_epoch))
    }

    fn sleep_until(&self, deadline: Timestamp) -> Sleep {
        let wait = deadline.saturating_since(self.now());
        Box::pin(tokio::time::sleep(wait))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_saturates() {
        let t = Timestamp::from_millis(1_000);
        assert_eq!((t + Duration::from_secs(2)).as_millis(), 3_000);
        assert_eq!((t - Duration::from_secs(2)).as_millis(), -1_000);
        assert_eq!(
            t.saturating_since(t + Duration::from_secs(1)),
            Duration::ZERO
        );
        assert_eq!(
            (t + Duration::from_secs(1)).saturating_since(t),
            Duration::from_secs(1)
        );
    }
}
