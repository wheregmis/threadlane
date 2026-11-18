//! Versioned device preference. Call storage functions from a background executor.
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
struct Preference {
    version: u32,
    enabled: bool,
}

pub fn path() -> PathBuf {
    threadlane_project::global_threadlane_dir().join("gui/completion_alerts.json")
}

pub fn load(path: &Path) -> Result<bool, String> {
    let text = match std::fs::read(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Couldn't load background completion alerts: {error}"
            ))
        }
    };
    let preference: Preference = serde_json::from_slice(&text)
        .map_err(|error| format!("Couldn't load background completion alerts: {error}"))?;
    if preference.version != 1 {
        return Err("Background completion alerts use an unsupported preference version.".into());
    }
    Ok(preference.enabled)
}

pub fn save(path: &Path, enabled: bool) -> Result<(), String> {
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        let parent = path.parent().ok_or("Missing preference directory")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(
            &mut file,
            &Preference {
                version: 1,
                enabled,
            },
        )?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    };
    write()
        .map_err(|error| format!("Couldn't save background completion alerts. Try again: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{load, save};
    #[test]
    fn completion_alert_preferences_default_off_and_atomic_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("gui/completion_alerts.json");
        assert!(!load(&path).unwrap());
        save(&path, true).unwrap();
        assert!(load(&path).unwrap());
        save(&path, false).unwrap();
        assert!(!load(&path).unwrap());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }
    #[test]
    fn completion_alert_preferences_invalid_or_future_versions_are_visible() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("alerts.json");
        for text in ["not json", "{}", r#"{"version":2,"enabled":true}"#] {
            std::fs::write(&path, text).unwrap();
            assert!(load(&path).is_err());
        }
    }
    #[test]
    fn completion_alert_preferences_save_failure_keeps_old_value_and_can_retry() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("alerts.json");
        std::fs::create_dir(&path).unwrap();
        assert!(save(&path, true).unwrap_err().contains("Try again"));
        std::fs::remove_dir(&path).unwrap();
        save(&path, true).unwrap();
        assert!(load(&path).unwrap());
    }
}
