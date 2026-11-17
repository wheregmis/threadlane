//! Machines as the machines screen shows them (Plan.md §5.3, §11.1).

use threadlane_core::backend::Probe;
use threadlane_core::model::{BackendKind, Machine, MachineStatus};

use crate::When;

/// One machine of one space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    /// Its name, and `Hub` when it hosts the space's hub.
    pub name: String,
    /// `online`, `offline · last seen 14:20`, `draining`, `revoked`.
    pub state: String,
    /// What each backend reported: `claude 2.1 logged in`, `codex not
    /// installed`.
    pub backends: Vec<String>,
    /// `gh logged in` or `gh not logged in`.
    pub gh: String,
    /// `1 of 2 running`.
    pub load: String,
}

/// How the machines screen shows `machine`.
#[must_use]
pub fn machine_row(machine: &Machine, when: &When) -> MachineRow {
    let state = match (machine.status, machine.last_seen) {
        (MachineStatus::Offline, Some(seen)) => {
            format!("offline · last seen {}", when.moment(seen))
        }
        (status, _) => status.to_string(),
    };
    let backends = BackendKind::ALL
        .iter()
        .map(|kind| {
            let probe = machine
                .backends
                .get(kind.as_str())
                .and_then(|probe| serde_json::from_value::<Probe>(probe.clone()).ok());
            match probe {
                None => format!("{kind} not reported"),
                Some(probe) if !probe.installed => format!("{kind} not installed"),
                Some(probe) => {
                    let version = probe
                        .version
                        .map_or_else(String::new, |version| format!(" {version}"));
                    let login = if probe.logged_in {
                        "logged in"
                    } else {
                        "not logged in"
                    };
                    format!("{kind}{version} {login}")
                }
            }
        })
        .collect();
    MachineRow {
        name: if machine.is_hub {
            format!("{} · Hub", machine.name)
        } else {
            machine.name.clone()
        },
        state,
        backends,
        gh: if machine.gh_login {
            "gh logged in".to_owned()
        } else {
            "gh not logged in".to_owned()
        },
        load: format!("{} of {} running", machine.load, machine.max_parallel),
    }
}
