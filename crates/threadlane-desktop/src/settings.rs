//! Settings (the sidebar entry): what each hub reports about itself and how
//! it is reached, this app's client directory and version, and which
//! system notifications it raises, kept in the client directory.

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use threadlane_core::model::Mode;

use crate::{Alert, AppState, describe};

/// The file in the client directory that holds this app's own settings.
pub const SETTINGS_FILE: &str = "desktop.json";

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
        let saved: Json = std::fs::read_to_string(dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Json::Null);
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
        let text = json!({
            "notifications": { "decisions": self.decisions, "notices": self.notices }
        })
        .to_string();
        let path = dir.join(SETTINGS_FILE);
        let temp = dir.join(format!("{SETTINGS_FILE}.tmp"));
        std::fs::create_dir_all(dir)?;
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, &path)
    }
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

    /// Takes where this app keeps its files, and the notification settings
    /// saved there.
    pub fn set_client_dir(&mut self, dir: PathBuf) {
        self.notifications = Notifications::load(&dir);
        self.client_dir = Some(dir);
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
