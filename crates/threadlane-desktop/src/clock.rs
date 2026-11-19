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

    /// `Today`, `Yesterday`, `Tomorrow`, or the local date as `2026-11-17`.
    #[must_use]
    pub fn day(&self, at: Timestamp) -> String {
        let day = self.local_day(at);
        match self.local_day(self.now) - day {
            0 => "Today".to_owned(),
            1 => "Yesterday".to_owned(),
            -1 => "Tomorrow".to_owned(),
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

/// What [`duration`] says for under a minute.
pub const UNDER_A_MINUTE: &str = "less than a minute";

/// Formats an integer with ASCII comma thousands separators (`241900` -> `241,900`).
#[must_use]
pub fn format_count(value: u128) -> String {
    let digits = value.to_string();
    let len = digits.len();
    if len <= 3 {
        return digits;
    }
    let mut out = String::with_capacity(len + (len - 1) / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (len - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Parses an RFC 3339 / ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ` or with
/// fractional seconds) into a [`Timestamp`].
#[must_use]
pub fn parse_iso_utc(text: &str) -> Option<Timestamp> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || !text.ends_with('Z') {
        return None;
    }
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year: i64 = text[0..4].parse().ok()?;
    let month: u32 = text[5..7].parse().ok()?;
    let day: u32 = text[8..10].parse().ok()?;
    let hour: u32 = text[11..13].parse().ok()?;
    let minute: u32 = text[14..16].parse().ok()?;
    let second: u32 = text[17..19].parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let frac = &text[19..text.len() - 1];
    let millis: i64 = if frac.is_empty() {
        0
    } else {
        let digits = frac.strip_prefix('.')?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut padded = [b'0'; 3];
        for (slot, byte) in padded.iter_mut().zip(digits.bytes()) {
            *slot = byte;
        }
        std::str::from_utf8(&padded).ok()?.parse().ok()?
    };
    let days = days_from_civil(year, i64::from(month), i64::from(day));
    let secs = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))?;
    let total_millis = secs.checked_mul(1_000)?.checked_add(millis)?;
    Some(Timestamp::from_millis(total_millis))
}

/// Proleptic Gregorian date to days since 1970-01-01 (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * shifted_month + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `less than a minute`, `12 min`, `2 h 5 min`.
#[must_use]
pub fn duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    match minutes {
        0 => UNDER_A_MINUTE.to_owned(),
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
        assert_eq!(utc.moment(now + hour * 24), "Tomorrow 23:30");
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

    #[test]
    fn format_count_adds_thousands_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(12), "12");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(241_900), "241,900");
        assert_eq!(format_count(1_234_567_890), "1,234,567,890");
    }

    #[test]
    fn parse_iso_utc_round_trips_and_rejects_malformed() {
        // 2026-11-17 23:30:00 UTC
        let ts = parse_iso_utc("2026-11-17T23:30:00Z").unwrap();
        assert_eq!(ts, Timestamp::from_millis(1_794_958_200_000));
        let frac = parse_iso_utc("2026-11-17T23:30:00.25Z").unwrap();
        assert_eq!(frac, Timestamp::from_millis(1_794_958_200_250));
        assert!(parse_iso_utc("2026-11-17").is_none());
        assert!(parse_iso_utc("2026-13-01T00:00:00Z").is_none());
        assert!(parse_iso_utc("not-a-timestamp").is_none());
    }
}
