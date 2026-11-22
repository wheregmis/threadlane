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
    /// Space-wide composable skill playbook slugs attached to this agent.
    pub playbooks: Vec<String>,
    pub installed_skills: Option<Result<crate::control::InstalledSkills, String>>,
    /// The index it opened with: Save sends backends only when it moved.
    opened_with: usize,
    saved_backends: Vec<BackendKind>,
    original_skills: Vec<threadlane_core::model::SkillBinding>,
    original_playbooks: Vec<String>,
    opened_fields: Arc<AgentFields>,
    /// Whether the instructions show rendered as Markdown.
    pub preview: bool,
    save_error: Option<String>,
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

/// Model names longer than this are cut on team cards.
const CARD_MODEL_CHARS: usize = 20;

/// An agent's backends in preference order with each one's model, for the
/// team cards and task view: `(short, full)`, e.g. `claude · opus → codex ·
/// gpt-5`, where the backends after the first are fallbacks. `short` cuts
/// long model names; `full` is for the tooltip. A backend without a model
/// runs the hub's default for it, from `defaults`, marked `(default)`, or
/// else its own default model.
#[must_use]
pub fn backend_summary(
    agent: &Agent,
    defaults: &BTreeMap<BackendKind, String>,
) -> (String, String) {
    let line = |cut: bool| {
        let chosen = |backend: &BackendKind| {
            let set = model(&agent.models, *backend);
            let (model, suffix) = match (set.is_empty(), defaults.get(backend)) {
                (false, _) => (set, ""),
                (true, Some(default)) => (default.clone(), " (default)"),
                (true, None) => return format!("{backend} · default model"),
            };
            let model = match model.char_indices().nth(CARD_MODEL_CHARS) {
                Some((at, _)) if cut => format!("{}…", &model[..at]),
                _ => model,
            };
            format!("{backend} · {model}{suffix}")
        };
        if agent.backends.is_empty() {
            // Any backend may run it, each with its own model.
            let all: Vec<_> = BackendKind::ALL.iter().map(chosen).collect();
            format!("Any backend: {}", all.join(", "))
        } else {
            agent
                .backends
                .iter()
                .map(chosen)
                .collect::<Vec<_>>()
                .join(" → ")
        }
    };
    (line(true), line(false))
}

impl AgentEdit {
    /// The extra picker option for a saved order outside the presets.
    #[must_use]
    pub fn saved_backend_label(&self) -> Option<String> {
        (!self.saved_backends.is_empty()
            && !BACKENDS
                .iter()
                .any(|(_, kinds)| *kinds == self.saved_backends))
        .then(|| {
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

    /// Why the latest save failed, if any.
    #[must_use]
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
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
            playbooks: known.playbooks.clone(),
            original_playbooks: known.playbooks.clone(),
            installed_skills: None,
            opened_fields: Arc::new(fields.clone()),
            preview: false,
            save_error: None,
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
            playbooks: (!same_playbooks(&edit.playbooks, &edit.original_playbooks))
                .then(|| edit.playbooks.clone()),
            agent: edit.agent.clone(),
            name: changed(name, &opened.name),
            role: changed(role, &opened.role),
            instructions: changed(&fields.instructions, &opened.instructions),
            backends,
            models: (!models.is_empty()).then_some(models),
        };
        let OrgRequest::UpdateAgent {
            skills: None,
            playbooks: None,
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

    /// Whether this agent already has a save in flight, even across visits.
    #[must_use]
    pub fn agent_saving(&self, edit: &AgentEdit) -> bool {
        self.agent_saves
            .contains(&(edit.space.clone(), edit.agent.clone()))
    }

    /// Acquires the save slot for the open editor, clearing any prior error.
    pub fn begin_agent_save(&mut self) -> Option<AgentEdit> {
        let edit = self.agent_edit.as_mut()?;
        if !self
            .agent_saves
            .insert((edit.space.clone(), edit.agent.clone()))
        {
            return None;
        }
        edit.save_error = None;
        Some(edit.clone())
    }

    /// Releases the originating save slot and reports failure only in that editor.
    pub fn fail_agent_save(&mut self, saved_from: &AgentEdit, error: String) {
        self.agent_saves
            .remove(&(saved_from.space.clone(), saved_from.agent.clone()));
        if let Some(edit) = &mut self.agent_edit
            && Arc::ptr_eq(&edit.opened_fields, &saved_from.opened_fields)
        {
            edit.save_error = Some(error);
        }
    }

    /// Shows a validation failure without dropping the draft.
    pub fn agent_save_validation(&mut self, error: String) {
        if let Some(edit) = &mut self.agent_edit {
            edit.save_error = Some(error);
        }
    }

    /// Reconciles a saved agent with the draft that is currently visible.
    /// Returns true if there are later edits to save in the same form.
    pub fn finish_agent_save(
        &mut self,
        saved_from: &AgentEdit,
        mut submitted: AgentFields,
        current: &AgentFields,
        agent: Agent,
    ) -> bool {
        self.agent_saves
            .remove(&(saved_from.space.clone(), saved_from.agent.clone()));
        let newer_edits = self.agent_edit.as_ref().is_some_and(|edit| {
            Arc::ptr_eq(&edit.opened_fields, &saved_from.opened_fields)
                && (current != &submitted
                    || edit.backends != saved_from.backends
                    || !same_skills(&edit.skills, &saved_from.skills)
                    || !same_playbooks(&edit.playbooks, &saved_from.playbooks))
        });
        if newer_edits {
            // These are the values represented by the completed request. Comparing the next
            // Save against them sends only the additional edits, including deliberate undo.
            submitted.name = submitted.name.trim().to_owned();
            submitted.role = submitted.role.trim().to_owned();
            for (_, model) in &mut submitted.models {
                *model = model.trim().to_owned();
            }
            if let Some(edit) = &mut self.agent_edit {
                edit.opened_fields = Arc::new(submitted);
                edit.opened_with = saved_from.backends;
                edit.original_skills.clone_from(&saved_from.skills);
                edit.original_playbooks.clone_from(&saved_from.playbooks);
            }
        }
        self.agent_updated(saved_from, agent);
        newer_edits
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

/// Space playbook bindings are a set: reordering them is not an edit.
fn same_playbooks(a: &[String], b: &[String]) -> bool {
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
