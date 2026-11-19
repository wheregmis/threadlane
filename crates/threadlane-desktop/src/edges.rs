//! Display states and the consequences of actions that require a choice.

use threadlane_core::ids::{StandingRuleId, TaskId, TimerId};
use threadlane_core::model::Mode;

use crate::{AppState, Gap, Part, Reach, When, count_form, mode_label};

/// The copy for a data region that is loading or failed.
#[derive(Debug, PartialEq, Eq)]
pub struct StateSentence {
    /// What is happening, independently of any hub error.
    pub text: String,
    /// The hub's error, also indicating that the screen can retry.
    pub detail: Option<String>,
}

impl Gap {
    /// Describes the affected region and space.
    #[must_use]
    pub fn sentence(&self, what: &str, space: &str) -> StateSentence {
        let (verb, detail) = match self {
            Self::Loading => ("Loading", None),
            Self::Failed(why) => ("Could not load", Some(why.clone())),
        };
        StateSentence {
            text: format!("{verb} {what} from {space}."),
            detail,
        }
    }
}

/// The empty copy for each region that can have no records.
#[derive(Debug, Clone, Copy)]
#[allow(
    missing_docs,
    reason = "variant names identify the regions; sentence supplies their copy"
)]
pub enum EmptyRegion {
    Pairing,
    Projects,
    Decisions,
    Running,
    Results,
    Board,
    Repositories,
    Agents,
    ProjectAgents,
    Conversation,
    Memory,
    MemoryVersions,
    Rules,
    Routines,
    AllRoutines,
    Related,
    Activity,
    Timeline,
    Machines,
    Clients,
    MachineActive,
    MachineRecent,
    Tools,
}

impl EmptyRegion {
    /// What to show when this region is current and empty.
    #[must_use]
    pub fn sentence(self) -> &'static str {
        match self {
            Self::Pairing => "Pair this app with a space to see its work.",
            Self::Projects => "No projects are listed yet. Use New project to add one.",
            Self::Decisions => "Nothing waits on you. The team is on it.",
            Self::Running => "Nothing is running. Started tasks will appear here.",
            Self::Results => "No results yet. Delivered results will appear here.",
            Self::Board => "No tasks yet. Use New task or ask the lead to start work.",
            Self::Repositories => "No repositories yet. Use Add repository to connect one.",
            Self::ProjectAgents => "No agents are bound to this project yet.",
            Self::Agents => "No agent profiles yet. Create one to use it across projects.",
            Self::Conversation => "No messages yet. Give the lead a goal below.",
            Self::MemoryVersions => "This file has no recorded versions yet.",
            Self::Memory => "Nothing remembered yet. The lead’s memory files will appear here.",
            Self::Rules => {
                "No standing rules yet. Answering a decision with Always allow creates one."
            }
            Self::Routines => {
                "No routines yet. Add one to have the lead check something on a cadence."
            }
            Self::AllRoutines => "No routines or follow-ups in these projects yet.",
            Self::Related => "No other tasks depend on this task or need to finish before it.",
            Self::Activity => "No steps have been recorded for this task yet.",
            Self::Timeline => {
                "No timeline entries yet. Messages and run outcomes will appear here."
            }
            Self::Machines => "No machines have joined this space yet. Start a runner to join one.",
            Self::Clients => "No apps are paired over the network. Use New join code to pair one.",
            Self::MachineActive => "No runs are active on this machine.",
            Self::MachineRecent => "No finished runs have been recorded on this machine.",
            Self::Tools => "This machine has not reported its tools yet.",
        }
    }
}

/// Connection indicator state for a paired space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// The hub answered.
    Live,
    /// The app is waiting to hear from the hub.
    Reconnecting,
    /// The hub failed its last connection check.
    Unreachable,
}

/// The label and state of a space connection indicator.
#[derive(Debug, PartialEq, Eq)]
pub struct Connection {
    /// Display label.
    pub label: String,
    /// The semantic indicator state.
    pub status: ConnectionStatus,
}

impl AppState {
    /// Marks the start of another connection check.
    pub fn checking_connections(&mut self) {
        self.checking_connections = true;
    }

    /// The space's current connection indicator, formatted in local time.
    #[must_use]
    pub fn connection(&self, space: &str, when: &When) -> Option<Connection> {
        let known = self.space(space)?;
        let (status, label) = match (&known.reach, self.checking_connections) {
            (Reach::Reachable, _) => (ConnectionStatus::Live, "Live".to_owned()),
            (Reach::Unknown, _) | (Reach::Unreachable(_), true) => {
                (ConnectionStatus::Reconnecting, "Reconnecting…".to_owned())
            }
            (Reach::Unreachable(_), false) => (
                ConnectionStatus::Unreachable,
                known.unreachable_since.map_or_else(
                    || "Unreachable".to_owned(),
                    |since| format!("Unreachable since {}", when.time(since)),
                ),
            ),
        };
        Some(Connection { label, status })
    }

    /// A connection warning for the page header, if this space needs one.
    #[must_use]
    pub fn connection_warning(&self, space: &str, when: &When) -> Option<Connection> {
        self.connection(space, when)
            .filter(|connection| connection.status != ConnectionStatus::Live)
    }

    /// A single alert listing all unreachable paired spaces.
    #[must_use]
    pub fn connection_banner(&self) -> Option<String> {
        let names: Vec<_> = self
            .spaces
            .iter()
            .filter(|space| matches!(space.reach, Reach::Unreachable(_)))
            .map(|space| space.name.as_str())
            .collect();
        (!names.is_empty()).then(|| format!(
            "Can’t reach {}. You can still view saved information, edit drafts, and work in connected spaces.",
            names.join(", "),
        ))
    }
}

/// A command whose consequences may require confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    missing_docs,
    reason = "variant names identify the commands; confirmation supplies the consequences"
)]
pub enum Consequence {
    CancelTask(String, TaskId),
    CancelRoutine(String, String, TimerId),
    RevokeRule(String, StandingRuleId),
    Unpair(String),
    DeleteMemory(String, String, String),
    SetCeiling(String, Option<String>, Mode),
    ArchiveProject(String, String),
}

/// Copy shown by the kit's confirmation dialog.
#[derive(Debug, PartialEq, Eq)]
pub struct Confirmation {
    /// The question naming the action and its target.
    pub title: String,
    /// The sentence explaining what will happen.
    pub sentence: String,
    /// The verb on the destructive button.
    pub button: &'static str,
}

impl AppState {
    /// Returns the dialog copy for commands that require confirmation.
    #[must_use]
    pub fn confirmation(&self, action: &Consequence) -> Option<Confirmation> {
        let (title, sentence, button) = match action {
            Consequence::CancelTask(space, id) => {
                let detail = self.task(space, *id);
                let dependents = detail.map(|detail| detail.related.as_slice())
                    .or_else(|| self.space(space).map(|space| space.tasks.as_slice()))
                    .unwrap_or_default();
                let count = dependents.iter().filter(|other| other.task.depends_on.contains(id)
                    && !other.task.status.is_terminal()).count();
                let affected = match count {
                    0 => "this task".to_owned(),
                    n => format!("this task and its {n} {}", count_form(n, "dependent task", "dependent tasks")),
                };
                (format!("Cancel {id}?"), format!("The hub will permanently cancel {affected} and tell the lead."), "Cancel task")
            }
            Consequence::CancelRoutine(_, _, id) => (format!("Cancel routine {id}?"),
                "This permanently stops the routine and its future occurrences.".to_owned(), "Cancel routine"),
            Consequence::RevokeRule(_, id) => (format!("Revoke {id}?"),
                "The agent loses the permission this rule granted.".to_owned(), "Revoke rule"),
            Consequence::Unpair(space) => (format!("Unpair from {space}?"),
                "This app forgets the space and its key, and you will need to pair again to access its projects and memory here.".to_owned(), "Unpair"),
            Consequence::DeleteMemory(_, _, path) => (format!("Delete {path}?"),
                "Agents will lose access to this file in current memory.".to_owned(), "Delete file"),
            Consequence::ArchiveProject(_, project) => (
                format!("Archive {project}?"),
                "Work and timers stop, and the project's history stays readable under Archived. Its repository remotes stay registered to this project and unavailable to other spaces.".to_owned(),
                "Archive project",
            ),
            Consequence::SetCeiling(space, project, mode) => {
                let current = project.as_ref().map_or_else(|| self.space_ceiling(space),
                    |project| self.project_ceiling(space, project));
                if *mode >= current {
                    return None;
                }
                let caps = |project: &str| {
                    self.project_ceiling(space, project) > *mode
                        && (self.project_gap(space, project, Part::Bindings).is_some()
                            || self.project(space, project).is_some_and(|view| view.bindings.iter()
                                .any(|binding| binding.mode > *mode)))
                };
                let affects = if let Some(project) = project {
                    caps(project)
                } else {
                    self.space(space).is_none_or(|space| space.gap(Part::Projects).is_some()
                        || space.projects.iter().any(|project| caps(&project.name)))
                };
                if !affects { return None; }
                let name = project.as_ref().unwrap_or(space);
                (format!("Lower the ceiling of {name} to {}?", mode_label(*mode)),
                    format!("Agents above this ceiling will operate at {}.", mode_label(*mode)),
                    "Lower ceiling")
            }
        };
        Some(Confirmation {
            title,
            sentence,
            button,
        })
    }
}
