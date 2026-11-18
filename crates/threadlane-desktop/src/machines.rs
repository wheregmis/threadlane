//! Machines as the machines screen shows them (Plan.md §5.3, §11.1).

use crate::{Destination, Gap, describe};
use threadlane_client::{ClientCore, ClientError};
use threadlane_core::ids::{MachineId, RunId};
use threadlane_core::model::{Run, RunStatus};

use threadlane_core::backend::{Probe, account_label, usable_backends};
use threadlane_core::model::{Agent, BackendKind, Machine, MachineStatus};

use crate::{AppState, Part, When};

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
                .and_then(|value| serde_json::from_value::<Probe>(value.clone()).ok());
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

impl AppState {
    /// Which online machines in this space report a usable backend for the agent.
    #[must_use]
    pub fn agent_runs_on(&self, space: &str, agent: &Agent) -> Option<String> {
        let space = self.space(space)?;
        if space.gap(Part::Machines).is_some() {
            return None;
        }
        let names: Vec<_> = space
            .machines
            .iter()
            .filter(|machine| machine.status == MachineStatus::Online)
            .filter(|machine| {
                usable_backends(&machine.backends)
                    .iter()
                    .any(|kind| agent.backends.is_empty() || agent.backends.contains(kind))
            })
            .map(|machine| machine.name.as_str())
            .collect();
        Some(if names.is_empty() {
            "No online machine has a usable backend.".to_owned()
        } else {
            format!("Runs on {}", names.join(", "))
        })
    }
}

/// A run that can be opened from its machine.
#[derive(Debug, Clone)]
pub struct MachineRun {
    /// Where the run's work can be opened.
    pub destination: Destination,
    /// Work title and timing.
    pub label: String,
    /// Whether the run still holds a slot.
    pub active: bool,
    id: RunId,
    at: threadlane_core::time::Timestamp,
    machine: MachineId,
}

impl MachineRun {
    /// The run's stable identifier.
    #[must_use]
    pub fn id(&self) -> RunId {
        self.id
    }
}

/// The selected machine's observed data.
#[derive(Debug)]
pub struct MachineDetail {
    /// The current report.
    pub report: MachineRow,
    /// The reported membership state.
    pub status: MachineStatus,
    /// OS and architecture, where reported.
    pub platform: Option<String>,
    /// When the hub heard from this machine.
    pub last_seen: Option<String>,
    /// Percent of available run slots held, if reported.
    pub occupancy: Option<f32>,
    /// Whether the machine has reported its tools.
    pub tools_reported: bool,
    /// Space accounts for the backends installed on this machine.
    pub accounts: Vec<String>,
}

/// Runs the selected space's existing overview endpoints reported.
#[derive(Debug)]
pub struct MachineHistory {
    space: String,
    runs: Result<Vec<MachineRun>, ClientError>,
}

/// Loads task and lead run records using the space's overview endpoints.
pub async fn load_machine_history(core: &ClientCore, space: &str, when: &When) -> MachineHistory {
    async fn load(
        core: &ClientCore,
        space: &str,
        when: &When,
    ) -> Result<Vec<MachineRun>, ClientError> {
        let in_space = Some(space);
        let mut runs = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let display = |run: Run, destination: Destination, title: &str| {
            let at = run.ended_at.unwrap_or(run.started_at);
            MachineRun {
                destination,
                label: format!("{title} · {} · {}", run.status, when.moment(at)),
                active: run.status == RunStatus::Active,
                id: run.id,
                at,
                machine: run.machine_id,
            }
        };
        // shortcut: overview can only list runs by lane; fetch once per task
        // until overview can return a machine-scoped history.
        let mut listed_tasks = std::collections::HashSet::new();
        for project in core.project_list(in_space, true).await?.projects {
            let named = Some(project.name.as_str());
            for row in core.lead_runs(in_space, named).await? {
                if !seen.insert(row.run.id) {
                    continue;
                }
                runs.push(display(
                    row.run,
                    Destination::Lead(space.to_owned(), project.name.clone()),
                    &format!("{} · lead", project.name),
                ));
            }
            for task in core.tasks(in_space, named).await? {
                let id = task.task.id;
                if !listed_tasks.insert(id) {
                    continue;
                }
                let title = format!("{id} {}", task.task.title);
                for row in core.task(in_space, &id.to_string()).await?.runs {
                    if !seen.insert(row.run.id) {
                        continue;
                    }
                    runs.push(display(
                        row.run,
                        Destination::Task(space.to_owned(), id),
                        &title,
                    ));
                }
            }
        }
        runs.sort_by_key(|row| std::cmp::Reverse((row.at, row.id)));
        Ok(runs)
    }
    MachineHistory {
        space: space.to_owned(),
        runs: load(core, space, when).await,
    }
}

impl AppState {
    /// Takes the history returned by the selected space.
    pub fn apply_machine_history(&mut self, history: MachineHistory) {
        self.machine_history.insert(
            history.space,
            history.runs.map_err(|error| describe(&error)),
        );
    }

    /// The current and recent runs of one machine.
    pub fn machine_runs(
        &self,
        space: &str,
        machine: MachineId,
    ) -> Result<(Vec<&MachineRun>, Vec<&MachineRun>), Gap> {
        let runs = self
            .machine_history
            .get(space)
            .ok_or(Gap::Loading)?
            .as_ref()
            .map_err(|why| Gap::Failed(why.clone()))?;
        let (active, mut recent): (Vec<_>, Vec<_>) = runs
            .iter()
            .filter(|row| row.machine == machine)
            .partition(|row| row.active);
        recent.truncate(10);
        Ok((active, recent))
    }

    /// Formats only the observations available in the hub's machine report.
    #[must_use]
    pub fn machine_detail(&self, space: &str, id: MachineId, when: &When) -> Option<MachineDetail> {
        let space = self.space(space)?;
        let machine = space.machines.iter().find(|machine| machine.id == id)?;
        let platform: Vec<_> = ["os", "arch"]
            .into_iter()
            .filter_map(|field| machine.capabilities[field].as_str())
            .collect();
        let account_matches = |account: &&threadlane_core::model::Account| {
            let reported = &machine.backends[account.backend.as_str()];
            reported["installed"].as_bool() == Some(true)
                && account.label
                    == account_label(
                        account.backend,
                        reported["account_label"].as_str(),
                        machine.id,
                    )
        };
        Some(MachineDetail {
            report: machine_row(machine, when),
            status: machine.status,
            platform: (!platform.is_empty()).then(|| platform.join(" · ")),
            tools_reported: machine.last_seen.is_some(),
            last_seen: machine.last_seen.map(|seen| when.moment(seen)),
            occupancy: (machine.last_seen.is_some() && machine.max_parallel != 0).then(|| {
                100. * machine.load.min(machine.max_parallel) as f32 / machine.max_parallel as f32
            }),
            accounts: space
                .accounts
                .iter()
                .filter(account_matches)
                .map(|account| {
                    let base = format!("{} · {}", account.label, account.backend);
                    account.cooldown_until.map_or(base.clone(), |until| {
                        format!("{base} · cooldown until {}", when.moment(until))
                    })
                })
                .collect(),
        })
    }
}
