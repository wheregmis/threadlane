use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;

#[derive(Clone, PartialEq, Eq)]
pub struct SavedDevice {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    token: String,
}

impl SavedDevice {
    pub fn new(
        id: String,
        name: String,
        host: String,
        port: u16,
        token: String,
    ) -> Self {
        Self {
            id,
            name,
            host,
            port,
            token,
        }
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedDeviceInfo {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredList {
    version: u32,
    selected_id: Option<String>,
    devices: Vec<StoredDevice>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredDevice {
    id: String,
    name: String,
    host: String,
    port: u16,
    token: String,
}

#[derive(Clone)]
pub struct SavedDeviceList {
    selected_id: Option<String>,
    devices: Vec<SavedDevice>,
}

impl Default for SavedDeviceList {
    fn default() -> Self {
        Self {
            selected_id: None,
            devices: Vec::new(),
        }
    }
}

impl SavedDeviceList {
    pub fn from_json(value: &str) -> Result<Self, String> {
        let stored: StoredList =
            serde_json::from_str(value).map_err(|error| format!("invalid saved-device list: {error}"))?;
        if stored.version != VERSION
            || stored.devices.iter().any(|device| {
                device.id.is_empty()
                    || device.name.is_empty()
                    || device.host.is_empty()
                    || device.port == 0
                    || device.token.is_empty()
            })
        {
            return Err("saved-device list has invalid or unsupported data".into());
        }
        if stored
            .devices
            .iter()
            .enumerate()
            .any(|(index, device)| {
                stored.devices[index + 1..]
                    .iter()
                    .any(|other| other.id == device.id)
            })
        {
            return Err("saved-device list contains duplicate IDs".into());
        }
        let selected_id = stored.selected_id.filter(|selected| {
            stored.devices.iter().any(|device| &device.id == selected)
        });
        Ok(Self {
            selected_id,
            devices: stored
                .devices
                .into_iter()
                .map(|device| {
                    SavedDevice::new(
                        device.id,
                        device.name,
                        device.host,
                        device.port,
                        device.token,
                    )
                })
                .collect(),
        })
    }

    pub fn from_legacy(
        host: String,
        port: String,
        token: String,
    ) -> Result<Self, String> {
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| "saved pairing has an invalid port".to_string())?;
        if host.trim().is_empty() || token.is_empty() {
            return Err("saved pairing is incomplete".into());
        }
        let id = legacy_id(&host, port);
        Ok(Self {
            selected_id: Some(id.clone()),
            devices: vec![SavedDevice::new(
                id,
                "Desktop".into(),
                host,
                port,
                token,
            )],
        })
    }

    pub fn to_json(&self) -> Result<String, String> {
        let stored = StoredList {
            version: VERSION,
            selected_id: self.selected_id.clone(),
            devices: self
                .devices
                .iter()
                .map(|device| StoredDevice {
                    id: device.id.clone(),
                    name: device.name.clone(),
                    host: device.host.clone(),
                    port: device.port,
                    token: device.token.clone(),
                })
                .collect(),
        };
        serde_json::to_string(&stored).map_err(|error| error.to_string())
    }

    pub fn rows(&self) -> Vec<SavedDeviceInfo> {
        self.devices
            .iter()
            .map(|device| SavedDeviceInfo {
                id: device.id.clone(),
                name: device.name.clone(),
                host: device.host.clone(),
                port: device.port,
            })
            .collect()
    }

    pub fn device(&self, id: &str) -> Option<SavedDevice> {
        self.devices.iter().find(|device| device.id == id).cloned()
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    pub fn select(&mut self, id: &str) -> bool {
        if self.devices.iter().any(|device| device.id == id) {
            self.selected_id = Some(id.to_string());
            true
        } else {
            false
        }
    }

    pub fn upsert(&mut self, device: SavedDevice, select: bool) {
        if let Some(existing) = self.devices.iter_mut().find(|row| row.id == device.id) {
            *existing = device.clone();
        } else {
            self.devices.push(device.clone());
        }
        if select {
            self.selected_id = Some(device.id);
        }
    }

    pub fn replace_identity(&mut self, previous_id: &str, device: SavedDevice, select: bool) {
        let selected_previous = self.selected_id.as_deref() == Some(previous_id);
        if previous_id != device.id {
            self.remove(previous_id);
        }
        self.upsert(device, select || selected_previous);
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let old_len = self.devices.len();
        self.devices.retain(|device| device.id != id);
        let removed = self.devices.len() != old_len;
        if self.selected_id.as_deref() == Some(id) {
            self.selected_id = None;
        }
        removed
    }
}

fn legacy_id(host: &str, port: u16) -> String {
    let host = host
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("legacy-{port}-{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, host: &str, token: &str) -> SavedDevice {
        SavedDevice::new(
            id.into(),
            format!("Desktop {id}"),
            host.into(),
            42_000,
            token.into(),
        )
    }

    #[test]
    fn migrates_legacy_pairing_and_round_trips_versioned_list() {
        let list = SavedDeviceList::from_legacy(
            "192.168.1.4".into(),
            "43123".into(),
            "old-token".into(),
        )
        .unwrap();
        assert_eq!(list.rows()[0].name, "Desktop");
        assert_eq!(list.rows()[0].port, 43123);
        assert_eq!(list.device(list.selected_id().unwrap()).unwrap().token(), "old-token");
        let encoded = list.to_json().unwrap();
        let restored = SavedDeviceList::from_json(&encoded).unwrap();
        assert_eq!(restored.rows(), list.rows());
        assert_eq!(restored.selected_id(), list.selected_id());
    }

    #[test]
    fn upsert_preserves_other_desktops_and_updates_selected_identity() {
        let mut list = SavedDeviceList::default();
        list.upsert(device("one", "192.168.1.1", "token-1"), true);
        list.upsert(device("two", "192.168.1.2", "token-2"), true);
        list.upsert(
            SavedDevice::new(
                "one".into(),
                "Renamed".into(),
                "192.168.1.3".into(),
                43_000,
                "new-token".into(),
            ),
            true,
        );
        assert_eq!(list.rows().len(), 2);
        assert_eq!(list.rows()[0].name, "Renamed");
        assert_eq!(list.selected_id(), Some("one"));
    }

    #[test]
    fn replacing_a_legacy_identity_preserves_unrelated_desktops() {
        let mut list = SavedDeviceList::default();
        list.upsert(device("legacy", "192.168.1.1", "token-1"), true);
        list.upsert(device("other", "192.168.1.2", "token-2"), false);
        list.replace_identity(
            "legacy",
            SavedDevice::new(
                "canonical".into(),
                "Desktop".into(),
                "192.168.1.1".into(),
                42_000,
                "token-1".into(),
            ),
            true,
        );

        let rows = list.rows();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.id == "canonical"));
        assert!(rows.iter().any(|row| row.id == "other"));
        assert!(!rows.iter().any(|row| row.id == "legacy"));
        assert_eq!(list.selected_id(), Some("canonical"));
    }

    #[test]
    fn removal_clears_selection_without_switching_to_another_desktop() {
        let mut list = SavedDeviceList::default();
        list.upsert(device("one", "192.168.1.1", "token-1"), false);
        list.upsert(device("two", "192.168.1.2", "token-2"), true);
        assert!(list.remove("two"));
        assert_eq!(list.rows().len(), 1);
        assert_eq!(list.selected_id(), None);
    }

    #[test]
    fn selection_rejects_unknown_devices() {
        let mut list = SavedDeviceList::default();
        list.upsert(device("one", "192.168.1.1", "token-1"), false);
        assert!(!list.select("missing"));
        assert!(list.select("one"));
        assert_eq!(list.selected_id(), Some("one"));
    }
}
