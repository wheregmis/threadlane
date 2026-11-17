//! Times as the app shows them: a clock time, a day mark, how long.

use std::time::Duration;

use threadlane_core::time::Timestamp;

/// The moment a screen is drawn and the local time zone's offset, so every
/// time on it reads the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct When {
    /// Now.
    pub now: Timestamp,
    /// Local time minus UTC, in minutes.
    pub offset_minutes: i64,
}

impl When {
    /// `14:20`, local time.
    #[must_use]
    pub fn time(&self, at: Timestamp) -> String {
        let minutes = self.local_minutes(at).rem_euclid(24 * 60);
        format!("{:02}:{:02}", minutes / 60, minutes % 60)
    }

    /// `Today`, `Yesterday`, or the local date as `2026-11-17`.
    #[must_use]
    pub fn day(&self, at: Timestamp) -> String {
        let day = self.local_day(at);
        match self.local_day(self.now) - day {
            0 => "Today".to_owned(),
            1 => "Yesterday".to_owned(),
            _ => {
                let (year, month, day) = civil_from_days(day);
                format!("{year:04}-{month:02}-{day:02}")
            }
        }
    }

    /// `14:20` today, else the day and the time: `Yesterday 14:20`.
    #[must_use]
    pub fn moment(&self, at: Timestamp) -> String {
        match self.day(at).as_str() {
            "Today" => self.time(at),
            day => format!("{day} {}", self.time(at)),
        }
    }

    /// How long since `since`: `less than a minute`, `12 min`, `2 h 5 min`.
    #[must_use]
    pub fn since(&self, since: Timestamp) -> String {
        duration(self.now.saturating_since(since))
    }

    fn local_minutes(&self, at: Timestamp) -> i64 {
        at.as_millis().div_euclid(60_000) + self.offset_minutes
    }

    fn local_day(&self, at: Timestamp) -> i64 {
        self.local_minutes(at).div_euclid(24 * 60)
    }
}

/// `less than a minute`, `12 min`, `2 h 5 min`.
#[must_use]
pub fn duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    match minutes {
        0 => "less than a minute".to_owned(),
        1..60 => format!("{minutes} min"),
        _ => format!("{} h {} min", minutes / 60, minutes % 60),
    }
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_days_and_durations_read_in_local_time() {
        // 2026-11-17 23:30 UTC.
        let now = Timestamp::from_millis(1_794_958_200_000);
        let utc = When {
            now,
            offset_minutes: 0,
        };
        assert_eq!(utc.time(now), "23:30");
        assert_eq!(utc.day(now), "Today");
        let hour = Duration::from_secs(3600);
        assert_eq!(utc.day(now - hour * 24), "Yesterday");
        assert_eq!(utc.day(now - hour * 48), "2026-11-15");
        assert_eq!(utc.moment(now - hour * 24), "Yesterday 23:30");
        // An hour east, it is already tomorrow there.
        let east = When {
            now,
            offset_minutes: 60,
        };
        assert_eq!(east.time(now), "00:30");
        assert_eq!(east.day(now - Duration::from_secs(30 * 60)), "Today");
        assert_eq!(east.day(now - hour), "Yesterday");

        assert_eq!(
            utc.since(now - Duration::from_secs(59)),
            "less than a minute"
        );
        assert_eq!(utc.since(now - Duration::from_secs(12 * 60)), "12 min");
        assert_eq!(utc.since(now - Duration::from_secs(125 * 60)), "2 h 5 min");
    }
}
