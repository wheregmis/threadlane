//! Settings (the sidebar entry): what each hub reports about itself and how
//! it is reached, this app's client directory and version, and which
//! system notifications it raises, kept in the client directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value as Json, json};
use threadlane_core::model::{Mode, Pacing, QuietHours};

use crate::{Alert, AppState, count_form, describe};

/// The file in the client directory that holds this app's own settings.
pub const SETTINGS_FILE: &str = "desktop.json";

/// How the desktop chooses its light or dark theme.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    /// Follow the window's system appearance.
    #[default]
    System,
    /// Always use the light theme.
    Light,
    /// Always use the dark theme.
    Dark,
}

impl Appearance {
    fn from_saved(saved: &Json) -> Self {
        match saved["appearance"].as_str() {
            Some("light") => Self::Light,
            Some("dark") => Self::Dark,
            _ => Self::System,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// Which system notifications the app raises. Both are on until you turn
/// them off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notifications {
    /// For a new decision.
    pub decisions: bool,
    /// For a system notice.
    pub notices: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            decisions: true,
            notices: true,
        }
    }
}

impl Notifications {
    /// What [`SETTINGS_FILE`] in `dir` says; the defaults where it is
    /// missing, unreadable, or says nothing.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let saved = read(dir);
        let on = |key: &str| saved["notifications"][key].as_bool().unwrap_or(true);
        Self {
            decisions: on("decisions"),
            notices: on("notices"),
        }
    }

    /// Writes them to [`SETTINGS_FILE`] in `dir`, replacing it whole.
    ///
    /// # Errors
    ///
    /// The file could not be written.
    pub fn save(self, dir: &Path) -> std::io::Result<()> {
        let mut saved = read(dir);
        saved["notifications"] = json!({ "decisions": self.decisions, "notices": self.notices });
        write(dir, &saved)
    }
}

fn read(dir: &Path) -> Json {
    std::fs::read_to_string(dir.join(SETTINGS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Json>(&text).ok())
        .filter(Json::is_object)
        .unwrap_or_else(|| json!({}))
}

fn write(dir: &Path, saved: &Json) -> std::io::Result<()> {
    let path = dir.join(SETTINGS_FILE);
    let temp = dir.join(format!("{SETTINGS_FILE}.tmp"));
    std::fs::create_dir_all(dir)?;
    std::fs::write(&temp, saved.to_string())?;
    std::fs::rename(&temp, &path)
}

/// One space as Settings shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceRow {
    /// The name it was paired under.
    pub paired_as: String,
    /// The space's own name, as its hub reports it.
    pub space: Option<String>,
    /// The space's mode ceiling.
    pub ceiling: Option<Mode>,
    /// The hub's schema version.
    pub schema_version: Option<i64>,
    /// The installed release version if the hub supports updates.
    pub update_version: Option<String>,
    /// The machine that hosts the hub, as its hub lists it.
    pub host: Option<String>,
    /// `Unix socket /…/hub.sock` or `Network, hub key 1a2b3c4d5e`.
    pub reached_by: String,
}

impl AppState {
    /// Where this app keeps its files, once known.
    #[must_use]
    pub fn client_dir(&self) -> Option<&Path> {
        self.client_dir.as_deref()
    }

    /// Sets the client directory and reads its saved desktop preferences.
    pub fn set_client_dir(&mut self, dir: PathBuf) {
        self.notifications = Notifications::load(&dir);
        let saved = read(&dir);
        self.last_targets =
            serde_json::from_value(saved["last_projects"].clone()).unwrap_or_default();
        self.appearance = Appearance::from_saved(&saved);
        self.skip_send_hold = saved["hold_messages"].as_bool() == Some(false);
        self.selected_space = saved["selected_space"].as_str().map(str::to_owned);
        let seen = serde_json::from_value::<BTreeMap<String, BTreeMap<String, i64>>>(
            saved["conversation_seen"].clone(),
        )
        .unwrap_or_default();
        self.conversation_read = seen
            .into_iter()
            .flat_map(|(space, projects)| {
                projects.into_iter().map(move |(project, seq)| {
                    (
                        (space.clone(), project),
                        crate::conversation::ConversationRead::saved(seq),
                    )
                })
            })
            .collect();
        self.client_dir = Some(dir);
    }

    /// Which appearance the desktop should use.
    #[must_use]
    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    /// Applies the chosen appearance and saves it alongside the other preferences.
    ///
    /// # Errors
    ///
    /// Returns why the preference could not be saved. It still applies this session.
    pub fn set_appearance(&mut self, appearance: Appearance) -> Result<(), String> {
        self.appearance = appearance;
        let Some(dir) = &self.client_dir else {
            return Ok(());
        };
        let mut saved = read(dir);
        saved["appearance"] = json!(appearance.key());
        write(dir, &saved)
            .map_err(|error| format!("Could not save {SETTINGS_FILE}: {}", describe(&error)))
    }

    pub(crate) fn save_conversation_seen(&mut self) {
        let Some(dir) = &self.client_dir else { return };
        let mut seen: BTreeMap<&str, BTreeMap<&str, i64>> = BTreeMap::new();
        for ((space, project), position) in &self.conversation_read {
            seen.entry(space)
                .or_default()
                .insert(project, position.seen);
        }
        let mut saved = read(dir);
        saved["conversation_seen"] = json!(seen);
        if let Err(error) = write(dir, &saved) {
            self.report(crate::Activity::Failed(format!(
                "Could not save conversation read positions: {}",
                describe(&error)
            )));
        }
    }

    /// Whether a send can be undone during the first three seconds.
    #[must_use]
    pub fn hold_messages(&self) -> bool {
        !self.skip_send_hold
    }

    /// Applies and saves the send hold preference.
    ///
    /// # Errors
    ///
    /// Returns why the preference could not be saved. It still applies this session.
    pub fn set_hold_messages(&mut self, on: bool) -> Result<(), String> {
        self.skip_send_hold = !on;
        let Some(dir) = &self.client_dir else {
            return Ok(());
        };
        let mut saved = read(dir);
        saved["hold_messages"] = json!(on);
        write(dir, &saved)
            .map_err(|error| format!("Could not save {SETTINGS_FILE}: {}", describe(&error)))
    }

    /// Which system notifications the app raises.
    #[must_use]
    pub fn notifications(&self) -> Notifications {
        self.notifications
    }

    /// Turns the notifications for decisions, or with `decisions: false`
    /// those for system notices, on or off, and saves that.
    ///
    /// # Errors
    ///
    /// Why it could not be saved; it still applies until the app quits.
    pub fn set_notifications(&mut self, decisions: bool, on: bool) -> Result<(), String> {
        if decisions {
            self.notifications.decisions = on;
        } else {
            self.notifications.notices = on;
        }
        let Some(dir) = &self.client_dir else {
            return Ok(());
        };
        self.notifications
            .save(dir)
            .map_err(|error| format!("Could not save {SETTINGS_FILE}: {}", describe(&error)))
    }

    /// Whether `alert` raises a system notification, as Settings say.
    #[must_use]
    pub fn notifies(&self, alert: &Alert) -> bool {
        match alert {
            Alert::Decision { .. } => self.notifications.decisions,
            Alert::Notice { .. } => self.notifications.notices,
        }
    }

    /// Every paired space as Settings shows it, in the order they were
    /// paired.
    #[must_use]
    pub fn settings_rows(&self) -> Vec<SpaceRow> {
        self.spaces
            .iter()
            .map(|space| SpaceRow {
                paired_as: space.name.clone(),
                space: space.info.as_ref().map(|info| info.space.clone()),
                ceiling: space.info.as_ref().map(|info| info.mode_ceiling),
                schema_version: space.info.as_ref().map(|info| info.schema_version),
                update_version: space
                    .info
                    .as_ref()
                    .and_then(|info| info.update_version.clone()),
                host: space
                    .machines
                    .iter()
                    .find(|machine| machine.is_hub)
                    .map(|machine| machine.name.clone()),
                reached_by: space.reached_by.clone(),
            })
            .collect()
    }
}

/// A pacing control's label, explanation, and input hint.
#[derive(Debug)]
pub struct PacingField {
    /// Its visible label.
    pub label: &'static str,
    /// What the setting does, when applicable.
    pub explanation: &'static str,
    /// An example or the disabled value.
    pub placeholder: &'static str,
}

/// The settings form fields, in display order.
pub const PACING_FIELDS: [PacingField; 4] = [
    PacingField {
        label: "Hourly limit",
        explanation: "Limits non-urgent deliveries to this many per hour and holds the rest.",
        placeholder: "Off",
    },
    PacingField {
        label: "Quiet hours",
        explanation: "Holds non-urgent results until quiet hours end in the chosen time zone.",
        placeholder: "Off or 22:00–08:00",
    },
    PacingField {
        label: "Time zone",
        explanation: "",
        placeholder: "America/Toronto",
    },
    PacingField {
        label: "Digest window",
        explanation: "Collects low-urgency results for this many minutes and delivers them together.",
        placeholder: "Off",
    },
];

fn pacing_values(pacing: &Pacing) -> [String; 4] {
    let time = |minutes| format!("{:02}:{:02}", minutes / 60, minutes % 60);
    let (quiet, zone) = pacing.quiet.as_ref().map_or_else(
        || ("Off".to_owned(), String::new()),
        |quiet| {
            (
                format!("{}–{}", time(quiet.start_minute), time(quiet.end_minute)),
                quiet.time_zone.clone(),
            )
        },
    );
    let number = |value: Option<u32>| value.map_or_else(|| "Off".to_owned(), |n| n.to_string());
    [
        number(pacing.rate),
        quiet,
        zone,
        number(pacing.digest_minutes),
    ]
}

/// Parses the four fields of the delivery pacing form.
///
/// # Errors
///
/// A disabled or positive number is required for each limit, and a quiet
/// interval needs two different times and a named time zone.
pub fn parse_pacing([rate, quiet, zone, digest]: [&str; 4]) -> Result<Pacing, String> {
    let off = |text: &str| text.trim().is_empty() || text.trim().eq_ignore_ascii_case("off");
    let number = |text: &str, label| {
        if off(text) {
            return Ok(None);
        }
        text.trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| format!("{label} needs a positive whole number or Off."))
    };
    let quiet = if off(quiet) {
        None
    } else {
        let minute = |time: &str| {
            let (hour, min) = time.trim().split_once(':')?;
            let (hour, min) = (hour.parse::<u16>().ok()?, min.parse::<u16>().ok()?);
            (hour < 24 && min < 60).then(|| hour * 60 + min)
        };
        let (start, end) = quiet
            .split_once(['-', '–'])
            .and_then(|(start, end)| Some((minute(start)?, minute(end)?)))
            .filter(|(start, end)| start != end)
            .ok_or_else(|| {
                "Enter two different quiet hours, such as 22:00–08:00, or Off.".to_owned()
            })?;
        let zone = zone.trim();
        if zone.is_empty() {
            return Err("Enter a time zone for quiet hours, such as America/Toronto.".to_owned());
        }
        Some(QuietHours {
            start_minute: start,
            end_minute: end,
            time_zone: zone.to_owned(),
        })
    };
    Ok(Pacing {
        rate: number(rate, "Hourly limit")?,
        quiet,
        digest_minutes: number(digest, "Digest window")?,
    })
}

impl AppState {
    /// Saves the last-used project independently of Home's space filter.
    pub(crate) fn remember_target(&mut self, space: &str, project: &str) {
        if self
            .last_targets
            .get(space)
            .is_none_or(|known| known != project)
        {
            self.last_targets
                .insert(space.to_owned(), project.to_owned());
            self.save_targets();
        }
    }

    /// The space the sidebar shows: the one last chosen while it is paired,
    /// else the first.
    #[must_use]
    pub fn selected_space(&self) -> Option<&crate::Space> {
        self.selected_space
            .as_deref()
            .and_then(|name| self.space(name))
            .or_else(|| self.spaces().first())
    }

    /// Shows `space` in the sidebar and remembers it; a name that is not
    /// paired is ignored.
    pub fn select_space(&mut self, space: &str) {
        if self.space(space).is_none() || self.selected_space.as_deref() == Some(space) {
            return;
        }
        self.selected_space = Some(space.to_owned());
        if let Some(dir) = &self.client_dir {
            let mut saved = read(dir);
            saved["selected_space"] = json!(space);
            if let Err(error) = write(dir, &saved) {
                self.report(crate::Activity::Failed(format!(
                    "Could not save the selected space: {}",
                    describe(&error)
                )));
            }
        }
    }

    pub(crate) fn save_targets(&mut self) {
        if let Some(dir) = &self.client_dir {
            let mut saved = read(dir);
            saved["last_projects"] = json!(self.last_targets);
            if let Err(error) = write(dir, &saved) {
                self.report(crate::Activity::Failed(format!(
                    "Could not save the last project: {}",
                    describe(&error)
                )));
            }
        }
    }

    /// Takes the pacing settings reported by one hub.
    pub fn apply_pacing(
        &mut self,
        space: String,
        pacing: Result<Pacing, threadlane_client::ClientError>,
    ) {
        self.pacing
            .insert(space, pacing.map_err(|error| describe(&error)));
    }

    /// The reason pacing is not currently available.
    #[must_use]
    pub fn pacing_gap(&self, space: &str) -> crate::Gap {
        self.pacing
            .get(space)
            .and_then(|value| value.as_ref().err())
            .cloned()
            .map_or(crate::Gap::Loading, crate::Gap::Failed)
    }

    /// The reason metrics are not currently available.
    #[must_use]
    pub fn metrics_gap(&self, space: &str) -> crate::Gap {
        self.metrics
            .get(space)
            .and_then(|value| value.as_ref().err())
            .cloned()
            .map_or(crate::Gap::Loading, crate::Gap::Failed)
    }

    /// The pacing values for this space, or a message about their availability.
    pub fn pacing_values(&self, space: &str) -> Result<[String; 4], &str> {
        match self.pacing.get(space) {
            Some(Ok(pacing)) => Ok(pacing_values(pacing)),
            Some(Err(why)) => Err(why),
            None => Err("Loading delivery pacing…"),
        }
    }

    /// Whether a pacing editor can be opened without replacing a draft.
    #[must_use]
    pub fn can_edit_pacing(&self) -> bool {
        self.pacing_edit.is_none() && !self.pacing_saving
    }

    /// Opens the editor and returns the reported values to prefill it with.
    pub fn edit_pacing(&mut self, space: &str) -> Option<[String; 4]> {
        if !self.can_edit_pacing() {
            return None;
        }
        let values = self.pacing_values(space).ok()?;
        self.pacing_edit = Some(space.to_owned());
        Some(values)
    }

    /// Whether this space's editor is open, and whether a save is in progress.
    #[must_use]
    pub fn pacing_editor(&self, space: &str) -> Option<bool> {
        (self.pacing_edit.as_deref() == Some(space)).then_some(self.pacing_saving)
    }

    /// Closes the form without editing delivery pacing.
    pub fn keep_pacing(&mut self) {
        if !self.pacing_saving {
            self.pacing_edit = None;
        }
    }

    /// Validates the current draft and marks the edit as saving.
    pub fn submit_pacing(&mut self, space: &str, fields: [&str; 4]) -> Option<Pacing> {
        if self.pacing_editor(space) != Some(false) {
            return None;
        }
        match parse_pacing(fields) {
            Ok(pacing) => {
                self.pacing_saving = true;
                Some(pacing)
            }
            Err(why) => {
                self.report(crate::Activity::Failed(why));
                None
            }
        }
    }

    /// Applies a successful save and leaves the draft open if the hub refused it.
    pub fn finish_pacing(
        &mut self,
        space: &str,
        result: Result<Pacing, threadlane_client::ClientError>,
    ) {
        self.pacing_saving = false;
        match result {
            Ok(pacing) => {
                self.apply_pacing(space.to_owned(), Ok(pacing));
                self.keep_pacing();
                self.report(crate::Activity::Done(format!(
                    "Saved delivery pacing for {space}."
                )));
            }
            Err(error) => self.report(crate::Activity::Failed(describe(&error))),
        }
    }
}

/// Reads the space-wide metrics used in Settings.
pub async fn load_metrics(
    core: &threadlane_client::ClientCore,
    space: &str,
) -> Result<threadlane_core::client::OperationsMetrics, String> {
    use threadlane_core::client::{OperationsReply, OperationsRequest};

    match core
        .operations(Some(space), OperationsRequest::Metrics { project: None })
        .await
    {
        Ok(OperationsReply::Metrics { metrics }) => Ok(*metrics),
        Ok(_) => Err("The hub returned an unexpected metrics reply.".to_owned()),
        Err(error) => Err(describe(&error)),
    }
}

impl AppState {
    /// Takes the metrics reported by one hub.
    pub fn apply_metrics(
        &mut self,
        space: String,
        metrics: Result<threadlane_core::client::OperationsMetrics, String>,
    ) {
        self.metrics.insert(space, metrics);
    }

    /// Labels and values displayed in one space's Metrics card.
    pub fn metrics_rows(
        &self,
        space: &str,
        when: &crate::When,
    ) -> Result<Vec<(&'static str, String)>, &str> {
        use threadlane_core::client::Latency;

        let metrics = self
            .metrics
            .get(space)
            .ok_or("Loading metrics…")?
            .as_ref()
            .map_err(String::as_str)?;
        let number = |values: Vec<String>, empty: &str| {
            if values.is_empty() {
                empty.to_owned()
            } else {
                values.join(" · ")
            }
        };
        let latency = |latency: &Latency| match (latency.mean_ms, latency.max_ms) {
            (Some(mean), Some(max)) => format!(
                "{mean:.0} ms avg · {max} ms max · {} {}",
                latency.count,
                count_form(latency.count, "observation", "observations")
            ),
            _ => "No observations".to_owned(),
        };
        let age = |value: Option<i64>| match value {
            Some(ms @ 60_000..) => crate::clock::duration(Duration::from_millis(ms as u64)),
            Some(ms @ 1_000..) => format!("{:.1} s", ms as f64 / 1_000.0),
            Some(ms) => format!("{ms} ms"),
            None => "Empty".to_owned(),
        };
        let delivery = metrics.delivery_health.as_ref().map_or_else(
            || "Unavailable on this hub".to_owned(),
            |health| {
                let timing = |at, label| match at {
                    Some(at) if at <= metrics.at => format!(" · {label} due"),
                    Some(at) => format!(" · {label} {}", when.moment(at)),
                    None => String::new(),
                };
                format!(
                    "{} held{}; {} retrying{}; {} permanently failed (created in the past 7 days)",
                    health.held,
                    timing(health.next_hold_check, "pacing check"),
                    health.retrying,
                    timing(health.next_retry, "retry"),
                    health.failed_created_in_last_week,
                )
            },
        );
        Ok(vec![
            ("Snapshot", when.moment(metrics.at)),
            (
                "Project queues",
                number(
                    metrics
                        .queue_depth
                        .iter()
                        .map(|(name, count)| format!("{name} {count}"))
                        .collect(),
                    "No projects",
                ),
            ),
            (
                "Oldest pending event",
                age(metrics.lane_lag_ms.values().copied().max()),
            ),
            ("Run duration", latency(&metrics.run_duration)),
            (
                "Account cooldowns",
                number(
                    metrics
                        .account_cooldowns
                        .iter()
                        .map(|(account, at)| format!("{account} until {}", when.moment(*at)))
                        .collect(),
                    "None",
                ),
            ),
            ("Account wait", format!("{} ms", metrics.account_wait_ms)),
            ("Dead events", metrics.dead_events.to_string()),
            ("Timer lateness", latency(&metrics.timer_lateness)),
            ("Outbox lag", age(metrics.outbox_lag_ms)),
            ("Delivery health", delivery),
            (
                "Messages per day",
                number(
                    metrics
                        .messages_per_day
                        .iter()
                        .rev()
                        .take(7)
                        .map(|(day, count)| format!("{day} {count}"))
                        .collect(),
                    "None sent",
                ),
            ),
            (
                "Silent runs",
                metrics.silent_run_ratio.map_or_else(
                    || "No completed runs".to_owned(),
                    |ratio| format!("{:.0}%", 100.0 * ratio),
                ),
            ),
            (
                "Decisions in 24 hours",
                metrics.decisions_per_day.to_string(),
            ),
            ("Answer latency", latency(&metrics.answer_latency)),
            (
                "Standing rules created",
                metrics.standing_rules_created.to_string(),
            ),
        ])
    }
}
