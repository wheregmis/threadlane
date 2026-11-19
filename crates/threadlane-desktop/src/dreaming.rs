//! Settings and results of the project memory pass.

use threadlane_client::{ClientCore, parse_dreaming_cadence};
use threadlane_core::client::{
    DreamingStatus, MemoryFinding, MemoryReply, MemoryRequest, OperationsReply, OperationsRequest,
};
use threadlane_core::model::{Recurrence, Rule};

use crate::{AppState, When, describe};

#[derive(Debug, Default)]
pub(crate) struct DreamingView {
    status: Option<Result<DreamingStatus, String>>,
    findings: Option<Result<Vec<MemoryFinding>, String>>,
    saving: bool,
    revision: u64,
    previous: Option<Recurrence>,
}

/// A click in the Dreaming settings.
#[derive(Debug, Clone, Copy)]
pub enum DreamingAction<'a> {
    /// Toggle dreaming, preserving the last cadence chosen here.
    Toggle(bool),
    /// Save the cadence typed into the field.
    Cadence(&'a str),
}

/// Text and controls displayed on the Memory tab.
#[derive(Debug, PartialEq, Eq)]
pub struct DreamingCard {
    /// Whether the switch is checked.
    pub enabled: bool,
    /// Whether the control is waiting for an answer.
    pub disabled: bool,
    /// The active cadence or the reason it is unavailable.
    pub cadence: String,
    /// The reason cadence is unavailable.
    pub cadence_gap: Option<crate::Gap>,
    /// The last completed dream and the versions it wrote.
    pub last: Vec<String>,
    /// Checker findings or a status message.
    pub findings: Vec<String>,
    /// The reason the memory checker is unavailable.
    pub findings_gap: Option<crate::Gap>,
}

/// Replies for the project open in the Memory tab.
#[derive(Debug)]
pub struct DreamingLoad {
    space: String,
    project: String,
    revision: u64,
    status: Result<DreamingStatus, String>,
    findings: Result<Vec<MemoryFinding>, String>,
}

/// Loads the cadence, last dream and current findings.
pub async fn load_dreaming(
    core: &ClientCore,
    space: &str,
    project: &str,
    revision: u64,
) -> DreamingLoad {
    let name = Some(project.to_owned());
    let (status, findings) = tokio::join!(
        change_dreaming(
            core,
            space,
            MemoryRequest::Dreaming {
                project: name.clone()
            }
        ),
        core.operations(
            Some(space),
            OperationsRequest::CheckMemory { project: name }
        ),
    );
    let findings = match findings {
        Ok(OperationsReply::MemoryCheck { findings }) => Ok(findings),
        Ok(_) => Err("The hub returned an unexpected memory check reply.".to_owned()),
        Err(error) => Err(describe(&error)),
    };
    DreamingLoad {
        space: space.to_owned(),
        project: project.to_owned(),
        revision,
        status,
        findings,
    }
}

/// Sends a dreaming settings request through the paired hub.
pub async fn change_dreaming(
    core: &ClientCore,
    space: &str,
    request: MemoryRequest,
) -> Result<DreamingStatus, String> {
    match core.memory(Some(space), request).await {
        Ok(MemoryReply::Dreaming { status }) => Ok(status),
        Ok(_) => Err("The hub returned an unexpected dreaming reply.".to_owned()),
        Err(error) => Err(describe(&error)),
    }
}

impl AppState {
    /// Identifies reads started before a settings change.
    #[must_use]
    pub fn dreaming_revision(&self, space: &str, project: &str) -> u64 {
        self.dreaming
            .get(&(space.to_owned(), project.to_owned()))
            .map_or(0, |view| view.revision)
    }

    /// Applies the Memory tab's read results, ignoring a superseded settings reply.
    pub fn apply_dreaming(&mut self, load: DreamingLoad) {
        let view = self.dreaming.entry((load.space, load.project)).or_default();
        if load.revision == view.revision && !view.saving {
            view.status = Some(load.status);
            view.findings = Some(load.findings);
        }
    }

    /// Builds a save request and prevents concurrent settings changes.
    pub fn dreaming_request(
        &mut self,
        space: &str,
        project: &str,
        action: DreamingAction<'_>,
        zone: &str,
    ) -> Result<Option<MemoryRequest>, String> {
        let Some(view) = self
            .dreaming
            .get_mut(&(space.to_owned(), project.to_owned()))
        else {
            return Ok(None);
        };
        let Some(Ok(status)) = view.status.as_ref().filter(|_| !view.saving) else {
            return Ok(None);
        };
        let cadence = match action {
            DreamingAction::Toggle(false) => None,
            DreamingAction::Toggle(true) => view
                .previous
                .clone()
                .or_else(|| Some(Recurrence::default_dreaming(zone))),
            DreamingAction::Cadence(text) => {
                let zone = status
                    .cadence
                    .as_ref()
                    .filter(|cadence| matches!(cadence.rule, Rule::Daily { .. }))
                    .map_or(zone, |cadence| cadence.zone.as_str());
                let cadence = parse_dreaming_cadence(text, zone)?
                    .ok_or_else(|| "Use the Dreaming switch to turn it off.".to_owned())?;
                Some(cadence)
            }
        };
        if cadence == status.cadence {
            return Ok(None);
        }
        view.previous = status.cadence.clone().or(view.previous.take());
        view.saving = true;
        view.revision += 1;
        Ok(Some(MemoryRequest::SetDreaming {
            project: Some(project.to_owned()),
            cadence,
        }))
    }

    /// Finishes a save, leaving the previous value visible if the hub refused it.
    pub fn dreaming_saved(
        &mut self,
        space: &str,
        project: &str,
        result: Result<DreamingStatus, String>,
    ) {
        if let Some(view) = self
            .dreaming
            .get_mut(&(space.to_owned(), project.to_owned()))
        {
            view.saving = false;
            view.revision += 1;
            if let Ok(status) = result {
                view.status = Some(Ok(status));
            }
        }
    }

    /// Describes the Memory tab's settings and results.
    #[must_use]
    pub fn dreaming_card(&self, space: &str, project: &str, when: &When) -> DreamingCard {
        let view = self.dreaming.get(&(space.to_owned(), project.to_owned()));
        let mut card = DreamingCard {
            enabled: false,
            disabled: true,
            cadence: "Loading dreaming settings…".to_owned(),
            cadence_gap: Some(crate::Gap::Loading),
            last: Vec::new(),
            findings: vec!["Checking memory…".to_owned()],
            findings_gap: Some(crate::Gap::Loading),
        };
        if let Some(view) = view {
            match &view.status {
                Some(Ok(status)) => {
                    card.cadence_gap = None;
                    card.enabled = status.cadence.is_some();
                    card.disabled = view.saving;
                    card.cadence = if view.saving {
                        "Saving cadence…".to_owned()
                    } else {
                        status.cadence.as_ref().map_or_else(
                            || "Off".to_owned(),
                            |cadence| match cadence.rule {
                                Rule::Interval { minutes } => format!("Every {} h", minutes / 60),
                                _ => cadence.to_string(),
                            },
                        )
                    };
                    if let Some(dream) = &status.last {
                        card.last
                            .push(format!("Last dream · {}", when.moment(dream.at)));
                        card.last.extend(dream.changes.iter().map(|change| {
                            let action = if change.deleted { "Deleted" } else { "Wrote" };
                            format!("{action} {} · version {}", change.path, change.version)
                        }));
                        if dream.changes.is_empty() {
                            card.last.push("No memory changes.".to_owned());
                        }
                    } else {
                        card.last.push("No dreams yet.".to_owned());
                    }
                }
                Some(Err(error)) => card.cadence_gap = Some(crate::Gap::Failed(error.clone())),
                None => {}
            }
            match &view.findings {
                Some(Ok(findings)) if findings.is_empty() => {
                    card.findings_gap = None;
                    card.findings = vec!["No duplicate notes or stale references found.".to_owned()]
                }
                Some(Ok(findings)) => {
                    card.findings_gap = None;
                    card.findings = findings.iter().map(ToString::to_string).collect()
                }
                Some(Err(error)) => card.findings_gap = Some(crate::Gap::Failed(error.clone())),
                None => {}
            }
        }
        card
    }
}
