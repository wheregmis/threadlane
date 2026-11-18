//! The next occurrence of a routine, evaluated in its own time zone
//! (Plan.md §7.3, docs/milestone-8.md §2 "Recurrence"). The only code that
//! reads the zone database; core keeps the zone as a name.

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{AmbiguousOffset, TimeZone};
use threadlane_core::model::{Recurrence, Rule, Weekday};
use threadlane_core::time::Timestamp;

/// Days looked through for a calendar rule; every rule matches within 31.
const LOOKAHEAD_DAYS: u32 = 400;

/// The time zone of this name, from the compiled-in database.
pub(crate) fn zone(name: &str) -> Result<TimeZone, String> {
    jiff::tz::db()
        .get(name.trim())
        .map_err(|_| format!("{name:?} is not a time zone; use a name like Europe/Berlin"))
}

/// Checks a recurrence's fields and its zone.
pub(crate) fn check(recurrence: &Recurrence) -> Result<(), String> {
    recurrence.check()?;
    zone(&recurrence.zone).map(|_| ())
}

/// The first occurrence strictly after `now`. An interval counts in steps
/// from `from` (the occurrence that fired, or the time the routine was
/// made), so lateness never shifts it; a calendar rule takes the first
/// local match after `now`. Either way, missed occurrences are skipped:
/// downtime across several fires once, then moves to the next future one.
///
/// A local time that does not exist on a spring-forward day falls at the
/// first valid time after it, the moment the clocks jump; one that occurs
/// twice on a fall-back day falls at the first of the two only.
pub(crate) fn next_occurrence(
    recurrence: &Recurrence,
    from: Timestamp,
    now: Timestamp,
) -> Result<Timestamp, String> {
    recurrence.check()?;
    let (hour, minute) = match &recurrence.rule {
        Rule::Interval { minutes } => {
            let step = i64::from(*minutes) * 60_000;
            let elapsed = now.as_millis().saturating_sub(from.as_millis()).max(0);
            let steps = elapsed / step + 1;
            return Ok(Timestamp::from_millis(
                from.as_millis().saturating_add(steps.saturating_mul(step)),
            ));
        }
        Rule::Daily { hour, minute }
        | Rule::Weekdays { hour, minute, .. }
        | Rule::Monthly { hour, minute, .. } => (*hour, *minute),
    };
    let fail = |error: jiff::Error| error.to_string();
    let tz = zone(&recurrence.zone)?;
    let now_at = jiff::Timestamp::from_millisecond(now.as_millis()).map_err(fail)?;
    let time = Time::new(
        i8::try_from(hour).map_err(|e| e.to_string())?,
        i8::try_from(minute).map_err(|e| e.to_string())?,
        0,
        0,
    )
    .map_err(fail)?;
    // From the day before: near a transition at midnight, now's local date
    // can be a day past an occurrence that is still to come.
    let mut date = now_at
        .to_zoned(tz.clone())
        .date()
        .yesterday()
        .map_err(fail)?;
    for _ in 0..LOOKAHEAD_DAYS {
        if matches(&recurrence.rule, date) {
            let at = instant(&tz, date.to_datetime(time))?;
            if at > now_at {
                return Ok(Timestamp::from_millis(at.as_millisecond()));
            }
        }
        date = date.tomorrow().map_err(fail)?;
    }
    Err(format!("{recurrence} has no occurrence within a year"))
}

/// Whether a calendar rule falls on `date`.
fn matches(rule: &Rule, date: Date) -> bool {
    match rule {
        Rule::Interval { .. } => false,
        Rule::Daily { .. } => true,
        Rule::Weekdays { days, .. } => days.iter().any(|day| {
            Weekday::ALL.iter().position(|known| known == day)
                == usize::try_from(date.weekday().to_monday_zero_offset()).ok()
        }),
        Rule::Monthly { day, .. } => {
            i8::try_from(*day)
                .unwrap_or(i8::MAX)
                .min(date.days_in_month())
                == date.day()
        }
    }
}

/// The instant of a local time in `tz`, by the rules of [`next_occurrence`].
fn instant(tz: &TimeZone, local: DateTime) -> Result<jiff::Timestamp, String> {
    boundary(tz, local, false)
}

/// Resolves a civil boundary, with a choice of occurrence during a fold.
pub(crate) fn boundary(
    tz: &TimeZone,
    local: DateTime,
    late: bool,
) -> Result<jiff::Timestamp, String> {
    let fail = |error: jiff::Error| error.to_string();
    match tz.to_ambiguous_timestamp(local).offset() {
        AmbiguousOffset::Unambiguous { offset } => offset.to_timestamp(local).map_err(fail),
        AmbiguousOffset::Fold { before, after } => if late { after } else { before }
            .to_timestamp(local)
            .map_err(fail),
        AmbiguousOffset::Gap { after, .. } => {
            // Read with the later offset, the time is before the jump; the
            // next transition is the jump itself.
            let early = after.to_timestamp(local).map_err(fail)?;
            tz.following(early)
                .next()
                .map(|transition| transition.timestamp())
                .ok_or_else(|| format!("no transition after {local} in {tz:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A timestamp from RFC 3339 text.
    fn at(text: &str) -> Timestamp {
        Timestamp::from_millis(text.parse::<jiff::Timestamp>().unwrap().as_millisecond())
    }

    fn daily(hour: u8, minute: u8, zone: &str) -> Recurrence {
        Recurrence {
            rule: Rule::Daily { hour, minute },
            zone: zone.to_owned(),
        }
    }

    /// Every occurrence after `start`, `count` of them, as RFC 3339 in UTC.
    fn occurrences(recurrence: &Recurrence, start: &str, count: usize) -> Vec<String> {
        let mut now = at(start);
        let mut out = Vec::new();
        for _ in 0..count {
            now = next_occurrence(recurrence, now, now).unwrap();
            out.push(crate::format_time(now));
        }
        out
    }

    #[test]
    fn a_daily_time_stays_local_across_both_changes_in_new_york() {
        // 2026: clocks go forward on 8 March, back on 1 November.
        let nine = daily(9, 0, "America/New_York");
        assert_eq!(
            occurrences(&nine, "2026-03-07T15:00:00Z", 3),
            [
                "2026-03-08T13:00:00Z",
                "2026-03-09T13:00:00Z",
                "2026-03-10T13:00:00Z"
            ]
        );
        assert_eq!(
            occurrences(&nine, "2026-10-31T14:00:00Z", 2),
            ["2026-11-01T14:00:00Z", "2026-11-02T14:00:00Z"]
        );
        // 02:30 does not exist on 8 March: it fires at 03:00 EDT, the jump.
        let skipped = daily(2, 30, "America/New_York");
        assert_eq!(
            occurrences(&skipped, "2026-03-07T15:00:00Z", 2),
            ["2026-03-08T07:00:00Z", "2026-03-09T06:30:00Z"]
        );
        // 01:30 happens twice on 1 November: it fires once, at the first.
        let twice = daily(1, 30, "America/New_York");
        assert_eq!(
            occurrences(&twice, "2026-10-31T14:00:00Z", 2),
            ["2026-11-01T05:30:00Z", "2026-11-02T06:30:00Z"]
        );
        // Even asked from between the two, it does not fire the second.
        assert_eq!(
            occurrences(&twice, "2026-11-01T05:45:00Z", 1),
            ["2026-11-02T06:30:00Z"]
        );
    }

    #[test]
    fn a_daily_time_stays_local_across_both_changes_in_sydney() {
        // 2026: clocks go back on 5 April (03:00 to 02:00), forward on
        // 4 October (02:00 to 03:00). AEDT is +11, AEST +10.
        let nine = daily(9, 0, "Australia/Sydney");
        assert_eq!(
            occurrences(&nine, "2026-04-03T12:00:00Z", 2),
            ["2026-04-03T22:00:00Z", "2026-04-04T23:00:00Z"]
        );
        assert_eq!(
            occurrences(&nine, "2026-10-02T12:00:00Z", 2),
            ["2026-10-02T23:00:00Z", "2026-10-03T22:00:00Z"]
        );
        // 02:30 happens twice on 5 April: once, at the first (AEDT).
        let twice = daily(2, 30, "Australia/Sydney");
        assert_eq!(
            occurrences(&twice, "2026-04-04T00:00:00Z", 2),
            ["2026-04-04T15:30:00Z", "2026-04-05T16:30:00Z"]
        );
        // 02:30 does not exist on 4 October: 03:00 AEDT, the jump.
        assert_eq!(
            occurrences(&twice, "2026-10-03T00:00:00Z", 2),
            ["2026-10-03T16:00:00Z", "2026-10-04T15:30:00Z"]
        );
    }

    #[test]
    fn weekdays_monthly_and_intervals() {
        // 2026-03-06 is a Friday.
        let weekdays = Recurrence {
            rule: Rule::Weekdays {
                days: vec![Weekday::Mon, Weekday::Fri],
                hour: 8,
                minute: 30,
            },
            zone: "Europe/Berlin".to_owned(),
        };
        assert_eq!(
            occurrences(&weekdays, "2026-03-06T08:00:00Z", 3),
            [
                "2026-03-09T07:30:00Z",
                "2026-03-13T07:30:00Z",
                "2026-03-16T07:30:00Z"
            ]
        );
        // Day 31 falls on the last day of a shorter month.
        let monthly = Recurrence {
            rule: Rule::Monthly {
                day: 31,
                hour: 9,
                minute: 0,
            },
            zone: "UTC".to_owned(),
        };
        assert_eq!(
            occurrences(&monthly, "2026-01-31T10:00:00Z", 2),
            ["2026-02-28T09:00:00Z", "2026-03-31T09:00:00Z"]
        );
        // An interval counts from where it started, skipping what was missed.
        let hourly = Recurrence {
            rule: Rule::Interval { minutes: 60 },
            zone: "UTC".to_owned(),
        };
        let start = at("2026-01-01T00:00:00Z");
        assert_eq!(
            next_occurrence(&hourly, start, start),
            Ok(at("2026-01-01T01:00:00Z"))
        );
        assert_eq!(
            next_occurrence(&hourly, start, at("2026-01-01T05:20:00Z")),
            Ok(at("2026-01-01T06:00:00Z"))
        );
    }

    #[test]
    fn a_zone_must_exist() {
        assert!(check(&daily(9, 0, "Europe/Berlin")).is_ok());
        assert!(check(&daily(9, 0, "Mars/Olympus")).is_err());
        assert!(check(&daily(24, 0, "UTC")).is_err());
    }
}
