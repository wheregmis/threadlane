//! Routines and follow-ups as a project's Routines tab and the space
//! screen show them (Plan.md §7, wireframe sidebar "Routines"): what each
//! does, when, who set it, and the "+ New routine" form.

use std::collections::BTreeMap;

use threadlane_client::{ClientCore, ClientError};
use threadlane_core::ids::{LaneId, TimerId};
use threadlane_core::model::{
    Recurrence, Rule, Timer, TimerCategory, TimerCreator, TimerStatus, Weekday,
};
use threadlane_core::time::Timestamp;

use crate::{Answer, AppState, Gap, Part, When, count_form, gap_in, take};

/// One routine or follow-up as a row shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineRow {
    /// Its number, which pause, resume and cancel name.
    pub id: TimerId,
    /// What it asks for when it fires.
    pub instruction: String,
    /// `daily at 09:00`, or `follow-up` for one that fires once.
    pub cadence: String,
    /// The zone its times are in; `None` for a follow-up or an interval.
    pub zone: Option<String>,
    /// `Next Tomorrow 09:00`, local time, or `Paused`.
    pub next: String,
    /// `Set by you · work you requested` or `Set by an agent`.
    pub by: String,
    /// `Fired 3 times · last 08:00`, or `Not fired yet`.
    pub fired: String,
    /// `Active` or `Paused`.
    pub status: &'static str,
    /// Whether it is paused, so the row offers Resume rather than Pause.
    pub paused: bool,
    /// Whether it waits for you to confirm cancelling it.
    pub confirming: bool,
}

/// The routines of one project on the Routines screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineGroup {
    /// The space it is in.
    pub space: String,
    /// The project's name.
    pub project: String,
    /// Its rows, or what to show while they are not known.
    pub rows: Result<Vec<RoutineRow>, Gap>,
}

/// The cadences the "+ New routine" form offers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Cadence {
    /// Every N minutes or hours.
    #[default]
    Every,
    /// Daily at a time.
    Daily,
    /// On chosen weekdays at a time.
    Weekdays,
    /// Monthly on a day at a time.
    Monthly,
}

impl Cadence {
    /// Every cadence, in the order the picker shows them, with its label.
    pub const ALL: [(Self, &'static str); 4] = [
        (Self::Every, "Every"),
        (Self::Daily, "Daily"),
        (Self::Weekdays, "Weekdays"),
        (Self::Monthly, "Monthly"),
    ];
}

/// The picks of the "+ New routine" form; its text fields live in the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineForm {
    /// The space and project whose form is open.
    pub open_for: Option<(String, String)>,
    /// The cadence picked.
    pub cadence: Cadence,
    /// Whether "every N" counts hours rather than minutes.
    pub hours: bool,
    /// The weekdays picked, Monday first.
    pub days: Vec<Weekday>,
}

impl Default for RoutineForm {
    fn default() -> Self {
        Self {
            open_for: None,
            cadence: Cadence::default(),
            hours: false,
            days: vec![Weekday::Mon],
        }
    }
}

/// What the form's text fields say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutineText<'a> {
    /// N, for every N minutes or hours.
    pub every: &'a str,
    /// The local time, `08:30`.
    pub at: &'a str,
    /// The day of the month.
    pub day: &'a str,
    /// The zone, `Europe/Berlin`.
    pub zone: &'a str,
}

impl RoutineForm {
    /// Whether this project owns the open routine draft.
    #[must_use]
    pub fn is_open_for(&self, space: &str, project: &str) -> bool {
        self.open_for
            .as_ref()
            .is_some_and(|(in_space, named)| in_space == space && named == project)
    }

    /// The recurrence the form describes, or what is wrong with it.
    ///
    /// # Errors
    ///
    /// A field that does not parse, in a sentence.
    pub fn recurrence(&self, text: RoutineText<'_>) -> Result<Recurrence, String> {
        let at = || {
            text.at
                .trim()
                .split_once(':')
                .and_then(|(hour, minute)| Some((hour.parse().ok()?, minute.parse().ok()?)))
                .ok_or_else(|| format!("{:?} is not a time like 08:30", text.at.trim()))
        };
        let rule = match self.cadence {
            Cadence::Every => {
                let every: u32 = text.every.trim().parse().map_err(|_| {
                    format!(
                        "{:?} is not a number of minutes or hours",
                        text.every.trim()
                    )
                })?;
                let unit = if self.hours { 60 } else { 1 };
                let minutes = every
                    .checked_mul(unit)
                    .ok_or_else(|| "that interval is too long".to_owned())?;
                Rule::Interval { minutes }
            }
            Cadence::Daily => {
                let (hour, minute) = at()?;
                Rule::Daily { hour, minute }
            }
            Cadence::Weekdays => {
                let (hour, minute) = at()?;
                Rule::Weekdays {
                    days: self.days.clone(),
                    hour,
                    minute,
                }
            }
            Cadence::Monthly => {
                let (hour, minute) = at()?;
                let day = text
                    .day
                    .trim()
                    .parse()
                    .map_err(|_| format!("{:?} is not a day of the month", text.day.trim()))?;
                Rule::Monthly { day, hour, minute }
            }
        };
        let recurrence = Recurrence {
            rule,
            zone: text.zone.trim().to_owned(),
        };
        recurrence.check()?;
        Ok(recurrence)
    }

    /// Picks `day`, or drops it if picked.
    pub fn toggle_day(&mut self, day: Weekday) {
        if let Some(at) = self.days.iter().position(|known| *known == day) {
            self.days.remove(at);
        } else {
            self.days.push(day);
            self.days
                .sort_by_key(|day| Weekday::ALL.iter().position(|known| known == day));
        }
    }
}

/// What the app knows of one project's routines.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RoutinesView {
    timers: Vec<Timer>,
    answers: BTreeMap<Part, Answer>,
}

impl RoutinesView {
    pub(crate) fn answers(&self) -> &BTreeMap<Part, Answer> {
        &self.answers
    }
}

/// The routines of one project. Made by [`load_routines`], applied with
/// [`AppState::apply_routines`].
#[derive(Debug)]
pub struct RoutinesLoad {
    space: String,
    project: String,
    timers: Result<Vec<Timer>, ClientError>,
}

/// Asks for the routines and follow-ups of each `(space, project)`.
pub async fn load_routines(
    core: &ClientCore,
    projects: Vec<(String, String)>,
) -> Vec<RoutinesLoad> {
    let mut loads = Vec::with_capacity(projects.len());
    for (space, project) in projects {
        let timers = core.routines(Some(&space), Some(&project)).await;
        loads.push(RoutinesLoad {
            space,
            project,
            timers,
        });
    }
    loads
}

/// A row of `timer`, read at `when`.
fn row_of(timer: &Timer, when: &When, confirming: bool) -> RoutineRow {
    let zone = timer
        .recurrence
        .as_ref()
        .filter(|recurrence| !matches!(recurrence.rule, Rule::Interval { .. }))
        .map(|recurrence| recurrence.zone.clone());
    let cadence = match (&timer.recurrence, timer.category) {
        (
            Some(Recurrence {
                rule: Rule::Interval { minutes },
                ..
            }),
            _,
        ) if *minutes >= 60 && minutes.is_multiple_of(60) => {
            let hours = minutes / 60;
            format!("every {hours} {}", count_form(hours, "hour", "hours"))
        }
        (Some(recurrence), _) => {
            let full = recurrence.to_string();
            match &zone {
                Some(zone) => full
                    .strip_suffix(&format!(" {zone}"))
                    .map_or(full.clone(), str::to_owned),
                None => full,
            }
        }
        (None, TimerCategory::FollowUp) => "follow-up".to_owned(),
        (None, TimerCategory::Schedule) => "routine".to_owned(),
        (None, TimerCategory::Dreaming) => "dreaming".to_owned(),
    };
    let paused = timer.status == TimerStatus::Paused;
    let by = match timer.created_by {
        TimerCreator::User => "Set by you",
        TimerCreator::Agent => "Set by an agent",
        TimerCreator::System => "Set by the system",
    };
    let by = if timer.requested {
        format!("{by} · work you requested")
    } else {
        by.to_owned()
    };
    let fired = match (timer.fire_count, timer.last_fired_at) {
        (0, _) => "Not fired yet".to_owned(),
        (count, last) => {
            let times = if count == 1 { "once" } else { "times" };
            let count = if count == 1 {
                String::new()
            } else {
                format!("{count} ")
            };
            let last = last.map_or_else(String::new, |at| format!(" · last {}", when.moment(at)));
            format!("Fired {count}{times}{last}")
        }
    };
    RoutineRow {
        id: timer.id,
        instruction: timer.instruction.clone(),
        cadence,
        zone,
        next: if paused {
            "Paused".to_owned()
        } else {
            format!("Next {}", when.moment(timer.due_at))
        },
        by,
        fired,
        status: if paused { "Paused" } else { "Active" },
        paused,
        confirming,
    }
}

impl AppState {
    /// Takes the routines of each project loaded.
    pub fn apply_routines(&mut self, loads: Vec<RoutinesLoad>) {
        for RoutinesLoad {
            space,
            project,
            timers,
        } in loads
        {
            let view = self.routines.entry((space, project)).or_default();
            view.timers = take(&mut view.answers, Part::Routines, timers);
        }
    }

    /// What to show in place of the routines of `project` in `space`.
    #[must_use]
    pub fn routines_gap(&self, space: &str, project: &str) -> Option<Gap> {
        self.routines
            .get(&(space.to_owned(), project.to_owned()))
            .map_or(Some(Gap::Loading), |view| {
                gap_in(&view.answers, Part::Routines)
            })
    }

    /// When the soonest active follow-up of `lane` in `project` is due, if
    /// one is listed.
    pub(crate) fn follow_up_due(
        &self,
        space: &str,
        project: &str,
        lane: LaneId,
    ) -> Option<Timestamp> {
        self.routines
            .get(&(space.to_owned(), project.to_owned()))?
            .timers
            .iter()
            .filter(|timer| {
                timer.lane_id == lane
                    && timer.category == TimerCategory::FollowUp
                    && timer.status == TimerStatus::Active
            })
            .map(|timer| timer.due_at)
            .min()
    }

    /// The routines and follow-ups of `project` in `space`, oldest first.
    #[must_use]
    pub fn routine_rows(&self, space: &str, project: &str, when: &When) -> Vec<RoutineRow> {
        self.routines
            .get(&(space.to_owned(), project.to_owned()))
            .map(|view| {
                view.timers
                    .iter()
                    .map(|timer| {
                        let confirming = self
                            .cancelling
                            .as_ref()
                            .is_some_and(|(at, id)| at == space && *id == timer.id);
                        row_of(timer, when, confirming)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Spaces whose project list is not yet available to list their routines.
    #[must_use]
    pub fn routines_space_gaps(&self) -> Vec<(&str, Gap)> {
        self.spaces
            .iter()
            .filter_map(|space| Some((space.name.as_str(), space.gap(Part::Projects)?)))
            .collect()
    }

    /// The Routines screen: every project of every space that has routines
    /// or could not list them, by space and project.
    #[must_use]
    pub fn routine_groups(&self, when: &When) -> Vec<RoutineGroup> {
        self.targets()
            .into_iter()
            .filter_map(|(space, project)| {
                let rows = match self.routines_gap(space, project) {
                    Some(gap) => Err(gap),
                    None => Ok(self.routine_rows(space, project, when)),
                };
                (rows.as_ref().is_err() || rows.as_ref().is_ok_and(|rows| !rows.is_empty())).then(
                    || RoutineGroup {
                        space: space.to_owned(),
                        project: project.to_owned(),
                        rows,
                    },
                )
            })
            .collect()
    }

    /// Asks to cancel timer `id` of `space`, which waits for you to confirm.
    pub fn ask_cancel_timer(&mut self, space: &str, id: TimerId) {
        self.cancelling = Some((space.to_owned(), id));
    }

    /// Keeps the timer you asked to cancel.
    pub fn keep_timer(&mut self) {
        self.cancelling = None;
    }

    /// Takes a timer as its hub now reports it after a pause, resume, cancel
    /// or create: an ended one leaves the list.
    pub fn timer_changed(&mut self, space: &str, project: &str, timer: Timer) {
        if self
            .cancelling
            .as_ref()
            .is_some_and(|(at, id)| at == space && *id == timer.id)
        {
            self.cancelling = None;
        }
        let view = self
            .routines
            .entry((space.to_owned(), project.to_owned()))
            .or_default();
        let ended = matches!(timer.status, TimerStatus::Cancelled | TimerStatus::Done);
        match view.timers.iter().position(|known| known.id == timer.id) {
            Some(at) if ended => {
                view.timers.remove(at);
            }
            Some(at) => view.timers[at] = timer,
            None if !ended => view.timers.push(timer),
            None => {}
        }
    }

    /// The picks of the "+ New routine" form.
    #[must_use]
    pub fn routine_form(&self) -> &RoutineForm {
        &self.routine_form
    }

    /// Changes the picks of the "+ New routine" form.
    pub fn routine_form_mut(&mut self) -> &mut RoutineForm {
        &mut self.routine_form
    }
}
