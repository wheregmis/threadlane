//! Why a queued task is waiting and how decision cards display standing rules.

use threadlane_core::client::TaskSummary;
use threadlane_core::ids::{DecisionId, TaskId};
use threadlane_core::model::{MachineStatus, TaskStatus};

use crate::permissions::allows;
use crate::{AppState, Queued};

/// The start of the hub's reason for a task whose agent is at its "at
/// once" limit (`threadlane-store`'s `waiting_reason`).
const AT_LIMIT: &str = "its agent already runs its limit";

/// `T-0001`, `T-0001 and T-0002`, `T-0001, T-0002 and T-0003`.
fn listed(ids: &[TaskId]) -> String {
    let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

impl AppState {
    /// Why queued task `summary` of `project` in `space` is not running:
    /// the hub's reason, with the tasks that hold its agent's slots named;
    /// or, where the hub gave none, what the app can tell: the project is
    /// paused, it waits for a task, its agent is at its limit, or every
    /// machine is busy. `None` when nothing is known.
    #[must_use]
    pub fn queued_reason(
        &self,
        space: &str,
        project: &str,
        summary: &TaskSummary,
    ) -> Option<String> {
        let task = &summary.task;
        let tasks = self
            .project(space, project)
            .map(|view| view.tasks.as_slice())
            .unwrap_or_default();
        let holding: Vec<TaskId> = tasks
            .iter()
            .filter(|other| {
                other.task.status == TaskStatus::Running
                    && other.agent == summary.agent
                    && other.task.id != task.id
            })
            .map(|other| other.task.id)
            .collect();
        let holds = crate::count_form(holding.len(), "holds it", "hold them");
        if let Some(reason) = &task.waiting_reason {
            if reason.starts_with(AT_LIMIT) && !holding.is_empty() {
                return Some(format!("{reason}: {} {holds}", listed(&holding)));
            }
            return Some(reason.clone());
        }
        if self.is_paused(space, project) {
            return Some("the project is paused".to_owned());
        }
        let waiting: Vec<TaskId> = task
            .depends_on
            .iter()
            .copied()
            .filter(|id| {
                tasks
                    .iter()
                    .find(|other| other.task.id == *id)
                    .is_none_or(|other| other.task.status != TaskStatus::Done)
            })
            .collect();
        if !waiting.is_empty() {
            return Some(format!("waits for {}", listed(&waiting)));
        }
        let online: Vec<_> = self
            .space(space)
            .map(|known| known.machines.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|machine| machine.status == MachineStatus::Online)
            .collect();
        let busy = !online.is_empty()
            && online
                .iter()
                .all(|machine| machine.max_parallel > 0 && machine.load >= machine.max_parallel);
        busy.then(|| {
            let each: Vec<String> = online
                .iter()
                .map(|machine| {
                    format!(
                        "{} runs {} of {}",
                        machine.name, machine.load, machine.max_parallel
                    )
                })
                .collect();
            format!("every machine is busy: {}", each.join(", "))
        })
    }

    /// The agent that asked decision `queued`: its task's, else the lead's.
    pub(crate) fn asker(&self, queued: &Queued) -> String {
        let space = &queued.space;
        let decision = &queued.decision;
        if let Some(name) = &decision.proposed_by {
            return name.clone();
        }
        let from_task = decision.task_id.and_then(|id| {
            if let Some(detail) = self.task(space, id) {
                return Some(detail.summary.agent.clone());
            }
            let known = self.space(space)?;
            known
                .tasks
                .iter()
                .chain(
                    self.projects
                        .iter()
                        .filter(|((at, _), _)| at == space)
                        .flat_map(|(_, view)| view.tasks.iter()),
                )
                .find(|summary| summary.task.id == id)
                .map(|summary| summary.agent.clone())
        });
        from_task
            .or_else(|| {
                let project = self.project_name(space, decision.project_id)?;
                self.project(space, project)?
                    .bindings
                    .iter()
                    .find(|binding| binding.is_lead)
                    .map(|binding| binding.agent.clone())
            })
            .unwrap_or_else(|| "the lead".to_owned())
    }

    /// The "Always allow" checkbox of a decision card (wireframe 1, note
    /// 3), worded from the decision, and whether it is ticked; `None` for a
    /// decision that offers no standing rule.
    #[must_use]
    pub fn always_offer(&self, queued: &Queued) -> Option<(String, bool)> {
        let decision = &queued.decision;
        if !decision.offers_standing_rule {
            return None;
        }
        let project = self.project_name(&queued.space, decision.project_id);
        let repository = decision
            .arguments
            .get("repository")
            .and_then(serde_json::Value::as_str);
        // The rule matches the target fields of the arguments only.
        let words = allows(&decision.tool, &decision.arguments);
        let ticked = self.always.contains(&(queued.space.clone(), decision.id));
        // "in threadlane, in threadlane" when the repository shares the project's name.
        let place = if repository.is_some() && repository == project {
            String::new()
        } else {
            format!(", in {}", project.unwrap_or("this project"))
        };
        Some((
            format!("Always allow {} to {words}{place}", self.asker(queued)),
            ticked,
        ))
    }

    /// Ticks or clears "Always allow" on decision `id` of `space`.
    pub fn toggle_always(&mut self, space: &str, id: DecisionId) {
        let key = (space.to_owned(), id);
        if !self.always.remove(&key) {
            self.always.insert(key);
        }
    }

    /// Whether answering decision `id` of `space` with `option` should make
    /// the standing rule too: "Always allow" is ticked, the decision offers
    /// one, and the option approves.
    #[must_use]
    pub fn answers_always(&self, space: &str, id: &str, option: &str) -> bool {
        self.queue.iter().any(|queued| {
            queued.space == space
                && queued.decision.id.to_string() == id
                && queued.decision.offers_standing_rule
                && self
                    .always
                    .contains(&(space.to_owned(), queued.decision.id))
                && queued
                    .decision
                    .options
                    .iter()
                    .any(|known| known.label.eq_ignore_ascii_case(option) && known.approves())
        })
    }
}
