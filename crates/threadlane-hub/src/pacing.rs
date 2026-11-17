//! Space delivery settings and their civil-time quiet interval.

use jiff::civil::Time;
use jiff::tz::TimeZone;
use threadlane_core::model::{Pacing, QuietHours};
use threadlane_core::time::Timestamp;

use crate::recurrence::{boundary, zone};

pub(crate) fn check(pacing: &Pacing) -> Result<(), String> {
    if pacing.rate == Some(0) || pacing.digest_minutes == Some(0) {
        return Err("rate and digest minutes must be positive".to_owned());
    }
    pacing.quiet.as_ref().map(Quiet::new).transpose()?;
    Ok(())
}

pub(crate) struct Quiet {
    start: Time,
    end: Time,
    zone: TimeZone,
}

impl Quiet {
    pub(crate) fn new(hours: &QuietHours) -> Result<Self, String> {
        let time = |minutes: u16| {
            let hour = i8::try_from(minutes / 60).map_err(|error| error.to_string())?;
            Time::new(hour, (minutes % 60) as i8, 0, 0).map_err(|error| error.to_string())
        };
        let start = time(hours.start_minute)?;
        let end = time(hours.end_minute)?;
        if start == end {
            return Err("quiet hours must have distinct start and end times".to_owned());
        }
        Ok(Self {
            start,
            end,
            zone: zone(&hours.time_zone)?,
        })
    }

    pub(crate) fn end_at(&self, at: Timestamp) -> Result<Option<Timestamp>, String> {
        let fail = |error: jiff::Error| error.to_string();
        let instant = jiff::Timestamp::from_millisecond(at.as_millis()).map_err(fail)?;
        let today = instant.to_zoned(self.zone.clone()).date();
        for day in [today, today.yesterday().map_err(fail)?] {
            let starts = boundary(&self.zone, day.to_datetime(self.start), false)?;
            let last_day = if self.start > self.end {
                day.tomorrow().map_err(fail)?
            } else {
                day
            };
            let ends = boundary(&self.zone, last_day.to_datetime(self.end), true)?;
            if starts <= instant && instant < ends {
                return Ok(Some(Timestamp::from_millis(ends.as_millisecond())));
            }
        }
        Ok(None)
    }
}
