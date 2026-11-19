//! Editing and deleting an agent from its card on the Agents screen: what
//! the edit view starts with, what Save asks the hub to change, and what the
//! app shows once the hub answers.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value as Json;
use threadlane_core::client::{BindingSummary, OrgRequest};
use threadlane_core::model::{Agent, BackendKind};

use crate::{AppState, BACKENDS};

/// The picks of an open edit view; its text fields live in the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEdit {
    /// The space the agent is in.
    pub space: String,
    /// The agent's name when the view opened, which Save and Delete name.
    pub agent: String,
    /// The selected preset, or `BACKENDS.len()` for the saved order.
    pub backends: usize,
    pub skills: Vec<threadlane_core::model::SkillBinding>,
    pub installed_skills: Option<Result<crate::control::InstalledSkills, String>>,
    /// The index it opened with: Save sends backends only when it moved.
    opened_with: usize,
    saved_backends: Vec<BackendKind>,
    original_skills: Vec<threadlane_core::model::SkillBinding>,
    opened_fields: Arc<AgentFields>,
    /// Whether the instructions show rendered as Markdown.
    pub preview: bool,
}

/// What the edit view's text fields hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFields {
    pub name: String,
    pub role: String,
    pub instructions: String,
    /// A model per backend, in [`BackendKind::ALL`] order; empty means the
    /// backend's default.
    pub models: Vec<(BackendKind, String)>,
}

/// One project the agent is bound in, beside the edit view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBinding {
    pub project: String,
    pub binding: BindingSummary,
}

fn model(models: &Json, backend: BackendKind) -> String {
    models
        .get(backend.as_str())
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_owned()
}

impl AgentEdit {
    /// The extra picker option for a saved order outside the presets.
    #[must_use]
    pub fn saved_backend_label(&self) -> Option<String> {
        (self.opened_with == BACKENDS.len()).then(|| {
            self.saved_backends
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", then ")
        })
    }

    fn selected_backends(&self) -> &[BackendKind] {
        BACKENDS
            .get(self.backends)
            .map_or(&self.saved_backends, |(_, kinds)| *kinds)
    }

    /// The selected backends in preference order, or all for an unrestricted agent.
    #[must_use]
    pub fn model_backends(&self) -> Vec<BackendKind> {
        match self.selected_backends() {
            [] => BackendKind::ALL.to_vec(),
            kinds => kinds.to_vec(),
        }
    }
}

impl AppState {
    fn known_agent(&self, space: &str, agent: &str) -> Option<&Agent> {
        self.space(space)?
            .agents
            .iter()
            .find(|known| known.name == agent)
    }

    /// The open edit view, if any.
    #[must_use]
    pub fn agent_edit(&self) -> Option<&AgentEdit> {
        self.agent_edit.as_ref()
    }

    /// Changes the picks of the open edit view.
    pub fn agent_edit_mut(&mut self) -> Option<&mut AgentEdit> {
        self.agent_edit.as_mut()
    }

    /// Opens the edit view of `agent` in `space`, and returns what goes in
    /// its text fields. `None` for an agent the app does not know.
    pub fn open_agent_edit(&mut self, space: &str, agent: &str) -> Option<AgentFields> {
        let known = self.known_agent(space, agent)?;
        let picked = BACKENDS
            .iter()
            .position(|(_, kinds)| *kinds == known.backends.as_slice())
            .unwrap_or(BACKENDS.len());
        let fields = AgentFields {
            name: known.name.clone(),
            role: known.role.clone(),
            instructions: known.instructions.clone(),
            models: BackendKind::ALL
                .iter()
                .map(|backend| (*backend, model(&known.models, *backend)))
                .collect(),
        };
        self.agent_edit = Some(AgentEdit {
            space: space.to_owned(),
            agent: agent.to_owned(),
            backends: picked,
            opened_with: picked,
            saved_backends: known.backends.clone(),
            skills: known.skills.clone(),
            original_skills: known.skills.clone(),
            installed_skills: None,
            opened_fields: Arc::new(fields.clone()),
            preview: false,
        });
        Some(fields)
    }

    /// Closes the edit view without saving.
    pub fn close_agent_edit(&mut self) {
        self.agent_edit = None;
    }

    /// What Save asks the hub: only the fields that differ from the agent
    /// when the editor opened. `Ok(None)` when nothing changed; `Err` says what
    /// to fix first.
    ///
    /// # Errors
    ///
    /// No edit view open, its agent gone, or an empty name or role.
    pub fn agent_update(&self, fields: &AgentFields) -> Result<Option<OrgRequest>, String> {
        let edit = self
            .agent_edit
            .as_ref()
            .ok_or("No agent is being edited.")?;
        if self.known_agent(&edit.space, &edit.agent).is_none() {
            return Err(format!("{} is no longer in {}.", edit.agent, edit.space));
        }
        let opened = &edit.opened_fields;
        let (name, role) = (fields.name.trim(), fields.role.trim());
        if name.is_empty() || role.is_empty() {
            return Err("Give the agent a name and a role.".to_owned());
        }
        let changed = |new: &str, old: &str| (new != old).then(|| new.to_owned());
        let backends = (edit.backends != edit.opened_with).then(|| {
            edit.selected_backends()
                .iter()
                .map(ToString::to_string)
                .collect()
        });
        let models: BTreeMap<String, String> = fields
            .models
            .iter()
            .filter(|(backend, value)| {
                let initial = opened.models.iter().find(|(kind, _)| kind == backend);
                value.trim() != initial.map_or("", |(_, initial)| initial.as_str())
            })
            .map(|(backend, value)| (backend.as_str().to_owned(), value.trim().to_owned()))
            .collect();
        let request = OrgRequest::UpdateAgent {
            skills: (!same_skills(&edit.skills, &edit.original_skills))
                .then(|| edit.skills.clone()),
            agent: edit.agent.clone(),
            name: changed(name, &opened.name),
            role: changed(role, &opened.role),
            instructions: changed(&fields.instructions, &opened.instructions),
            backends,
            models: (!models.is_empty()).then_some(models),
        };
        let OrgRequest::UpdateAgent {
            skills: None,
            name: None,
            role: None,
            instructions: None,
            backends: None,
            models: None,
            ..
        } = request
        else {
            return Ok(Some(request));
        };
        Ok(None)
    }

    /// Takes an agent its hub changed: its card and bindings show it at
    /// once, and the edit view closes.
    pub fn agent_updated(&mut self, saved_from: &AgentEdit, agent: Agent) {
        let (space, from) = (saved_from.space.as_str(), saved_from.agent.as_str());
        if let Some(edit) = &mut self.agent_edit {
            if Arc::ptr_eq(&edit.opened_fields, &saved_from.opened_fields) {
                self.agent_edit = None;
            } else if edit.space == space && edit.agent == from {
                edit.agent.clone_from(&agent.name);
            }
        }
        for ((at, _), view) in &mut self.projects {
            if at == space {
                for binding in view.bindings.iter_mut().filter(|b| b.agent == from) {
                    binding.agent.clone_from(&agent.name);
                    binding.role.clone_from(&agent.role);
                }
            }
        }
        if let Some(known) = self
            .spaces
            .iter_mut()
            .find(|known| known.name == space)
            .and_then(|known| known.agents.iter_mut().find(|known| known.name == from))
        {
            *known = agent;
        }
    }

    /// Where `agent` is bound in `space`, by project, as last listed.
    #[must_use]
    pub fn agent_bindings(&self, space: &str, agent: &str) -> Vec<AgentBinding> {
        let Some(known) = self.space(space) else {
            return Vec::new();
        };
        known
            .active_projects()
            .filter_map(|project| {
                let binding = self
                    .project(space, &project.name)?
                    .bindings
                    .iter()
                    .find(|binding| binding.agent == agent)?;
                Some(AgentBinding {
                    project: project.name.clone(),
                    binding: binding.clone(),
                })
            })
            .collect()
    }

    /// Whether "Delete agent…" is offered: never for a lead.
    #[must_use]
    pub fn offers_agent_delete(&self, space: &str, agent: &str) -> bool {
        self.known_agent(space, agent).is_some_and(|known| {
            known.role != "lead"
                && self
                    .agent_bindings(space, agent)
                    .iter()
                    .all(|bound| !bound.binding.is_lead)
        })
    }

    /// Drops an agent its hub deleted: its card, its column in the modes
    /// grid, and its edit view.
    pub fn agent_deleted(&mut self, space: &str, agent: &str) {
        if self
            .agent_edit
            .as_ref()
            .is_some_and(|edit| edit.space == space && edit.agent == agent)
        {
            self.agent_edit = None;
        }
        for ((at, _), view) in &mut self.projects {
            if at == space {
                view.bindings.retain(|binding| binding.agent != agent);
            }
        }
        if let Some(known) = self.spaces.iter_mut().find(|known| known.name == space) {
            known.agents.retain(|known| known.name != agent);
        }
        self.report(crate::Activity::Done(format!(
            "Deleted {agent} from {space}."
        )));
    }
}

/// Skill bindings are a set: reordering them is not an edit.
fn same_skills(
    a: &[threadlane_core::model::SkillBinding],
    b: &[threadlane_core::model::SkillBinding],
) -> bool {
    a.len() == b.len() && a.iter().all(|s| b.contains(s)) && b.iter().all(|s| a.contains(s))
}

#[cfg(test)]
mod skill_order_tests {
    use threadlane_core::model::{BackendKind, SkillBinding};

    #[test]
    fn reordered_skills_are_the_same_set() {
        let skill = |name: &str| SkillBinding {
            backend: BackendKind::Codex,
            name: name.into(),
        };
        assert!(super::same_skills(
            &[skill("a"), skill("b")],
            &[skill("b"), skill("a")]
        ));
        assert!(!super::same_skills(
            &[skill("a"), skill("a")],
            &[skill("a"), skill("b")]
        ));
    }
}
