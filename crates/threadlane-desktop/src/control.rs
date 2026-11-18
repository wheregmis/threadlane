//! What you create and stop from the app (wireframes 2, 5, 6): a new agent
//! and copying one to another space, a repository checked against every
//! other paired space, a task made skipping the lead, cancelling a task,
//! pausing a project, and the events recorded but acted on by nobody.

use threadlane_client::{ClientCore, ClientError, NewTaskSpec};
use threadlane_core::client::{AgentTemplate, DeletePreview, ProjectSummary, RecordedEvent};
use threadlane_core::github::normalize_remote;
use threadlane_core::ids::TaskId;
use threadlane_core::model::{Agent, BackendKind, Mode, Repository, Task};

use crate::{AppState, InSpace, Part, When, describe};

/// The backends a new agent may use, in order of preference, as the
/// "+ New agent" form offers them.
pub const BACKENDS: [(&str, &[BackendKind]); 6] = [
    ("Any backend", &[]),
    (
        "Claude, then Codex",
        &[BackendKind::Claude, BackendKind::Codex],
    ),
    (
        "Codex, then Claude",
        &[BackendKind::Codex, BackendKind::Claude],
    ),
    ("Claude only", &[BackendKind::Claude]),
    ("Codex only", &[BackendKind::Codex]),
    ("Antigravity only", &[BackendKind::Antigravity]),
];

/// How many recorded events a project's Conversation tab lists.
pub(crate) const RECORDED_SHOWN: u32 = 20;

/// How many characters of a recorded event's first line are shown.
const RECORDED_CHARS: usize = 160;

/// The picks of the "+ New agent" form; its text fields live in the view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentForm {
    /// The space it creates the agent in, while the form is open.
    pub space: Option<String>,
    /// The index in [`BACKENDS`] picked.
    pub backends: usize,
    /// What "Start from a template" offers: `None` while the hub has not
    /// answered, else its templates or why it could not list them.
    pub templates: Option<Result<Vec<AgentTemplate>, String>>,
}

/// What picking a template puts in the form's text fields, to edit before
/// saving (docs/agent-templates.md §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDraft {
    pub name: String,
    /// The template's id, as `agent add --template` stores it.
    pub role: String,
    pub instructions: String,
}

impl AgentForm {
    /// The backends picked, in order of preference.
    #[must_use]
    pub fn backends(&self) -> Vec<BackendKind> {
        BACKENDS
            .get(self.backends)
            .map(|(_, kinds)| kinds.to_vec())
            .unwrap_or_default()
    }

    /// Fills the form from the template `id`: picks its backends, and
    /// returns what goes in the text fields. `None` for one not offered.
    pub fn pick_template(&mut self, id: &str) -> Option<AgentDraft> {
        let template = self
            .templates
            .as_ref()?
            .as_ref()
            .ok()?
            .iter()
            .find(|template| template.id == id)?;
        // shortcut: backends no BACKENDS entry matches fall back to "Any backend".
        self.backends = BACKENDS
            .iter()
            .position(|(_, kinds)| *kinds == template.backends.as_slice())
            .unwrap_or(0);
        Some(AgentDraft {
            name: template.name.clone(),
            role: template.id.clone(),
            instructions: template.instructions.clone(),
        })
    }
}

/// The picks of the "+ New task" form; its title and goal live in the view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskForm {
    /// The project it creates the task in, while the form is open.
    at: Option<InSpace<String>>,
    /// The agent picked.
    pub agent: Option<String>,
    /// The repositories picked, by name.
    pub repositories: Vec<String>,
    /// The tasks picked that it waits for.
    pub depends_on: Vec<TaskId>,
}

impl TaskForm {
    /// Picks `repository`, or drops it if picked.
    pub fn toggle_repository(&mut self, repository: &str) {
        toggle(&mut self.repositories, repository.to_owned());
    }

    /// Picks `task` to wait for, or drops it if picked.
    pub fn toggle_dependency(&mut self, task: TaskId) {
        toggle(&mut self.depends_on, task);
    }
}

fn toggle<T: PartialEq>(picked: &mut Vec<T>, value: T) {
    if let Some(at) = picked.iter().position(|known| *known == value) {
        picked.remove(at);
    } else {
        picked.push(value);
    }
}

/// What the task screen offers to stop a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskControl {
    /// It has ended: nothing to stop.
    Ended,
    /// "Cancel" can be pressed.
    Cancel,
    /// Waiting for you to confirm, with what the confirmation says.
    Confirm(String),
}

/// One event recorded and acted on by nobody, as the Conversation tab
/// lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRow {
    /// Where it came from, for example `github`.
    pub source: String,
    /// The first line of what it says, cut short.
    pub text: String,
    /// When it arrived, local time.
    pub time: String,
}

/// The collapsed "Recorded, not acted on (N)" section of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// `Recorded, not acted on (3)`.
    pub title: String,
    /// Whether it is expanded.
    pub open: bool,
    /// The newest events, newest first.
    pub rows: Vec<RecordedRow>,
    /// `And 12 earlier.` when more were recorded than are listed.
    pub more: Option<String>,
}

/// Whether two remotes name the same repository: by
/// [`normalize_remote`] where both have a form it knows, else as typed.
#[must_use]
pub fn same_remote(a: &str, b: &str) -> bool {
    match (normalize_remote(a), normalize_remote(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a.trim() == b.trim(),
    }
}

/// Registers a repository in `project` of `space`, unless another paired
/// space already has its remote (Plan.md §11.3: hubs cannot see each
/// other, so the app checks). A space that cannot be asked refuses it too,
/// since it might have it.
///
/// # Errors
///
/// What to tell you: an empty field, the space that has the remote, one
/// that could not be checked, or the hub's refusal.
pub async fn add_repository(
    core: &ClientCore,
    space: &str,
    project: &str,
    name: &str,
    remote: &str,
    branch: &str,
) -> Result<Repository, String> {
    let (name, remote, branch) = (name.trim(), remote.trim(), branch.trim());
    if name.is_empty() || remote.is_empty() {
        return Err("Give the repository a name and a remote.".to_owned());
    }
    let others: Vec<String> = core
        .hubs()
        .iter()
        .map(|hub| hub.name.clone())
        .filter(|other| other != space)
        .collect();
    for other in others {
        let unchecked = |error: &ClientError| {
            format!(
                "Could not check {other} for this remote ({}). A repository belongs to one \
                 space; try again once {other} answers.",
                describe(error)
            )
        };
        let projects = core
            .project_list(Some(&other), true)
            .await
            .map_err(|error| unchecked(&error))?;
        for listed in projects.projects {
            let repositories = core
                .repositories(Some(&other), Some(&listed.name))
                .await
                .map_err(|error| unchecked(&error))?;
            if let Some(known) = repositories
                .iter()
                .find(|known| same_remote(&known.remote_url, remote))
            {
                return Err(format!(
                    "{remote} is already registered in {other}, as repository {} of project {}. \
                     A repository belongs to one space.",
                    known.name, listed.name
                ));
            }
        }
    }
    let branch = (!branch.is_empty()).then_some(branch);
    core.add_repository(Some(space), Some(project), name, remote, branch)
        .await
        .map_err(|error| describe(&error))
}

/// Creates `agent`'s definition in the space `to`, as it is: name, role,
/// instructions and backends (Plan.md §11.3, only when you ask).
///
/// # Errors
///
/// The hub's refusal, for example a name it already has.
pub async fn copy_agent(core: &ClientCore, agent: &Agent, to: &str) -> Result<Agent, ClientError> {
    core.create_agent(
        Some(to),
        &agent.name,
        &agent.role,
        &agent.instructions,
        &agent.backends,
    )
    .await
}

/// The first line of `text`, cut to [`RECORDED_CHARS`].
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    match line.char_indices().nth(RECORDED_CHARS) {
        Some((at, _)) => format!("{}…", line[..at].trim_end()),
        None => line.to_owned(),
    }
}

impl AppState {
    /// The picks of the "+ New agent" form.
    #[must_use]
    pub fn agent_form(&self) -> &AgentForm {
        &self.agent_form
    }

    /// Changes the picks of the "+ New agent" form.
    pub fn agent_form_mut(&mut self) -> &mut AgentForm {
        &mut self.agent_form
    }

    /// Opens the "+ New agent" form in `space`, with nothing picked and its
    /// templates not yet listed.
    pub fn open_agent_form(&mut self, space: &str) {
        self.agent_form = AgentForm {
            space: Some(space.to_owned()),
            ..AgentForm::default()
        };
    }

    /// Takes the templates `space`'s hub listed, if its form is still open.
    pub fn templates_loaded(
        &mut self,
        space: &str,
        templates: Result<Vec<AgentTemplate>, ClientError>,
    ) {
        if self.agent_form.space.as_deref() == Some(space) {
            self.agent_form.templates = Some(templates.map_err(|error| describe(&error)));
        }
    }

    /// Takes an agent its hub created: the Agents screen lists it, and the
    /// modes grid gives it a column of "—" cells to bind it with.
    pub fn agent_created(&mut self, space: &str, agent: Agent) {
        if self.agent_form.space.as_deref() == Some(space) {
            self.agent_form = AgentForm::default();
        }
        if let Some(known) = self.spaces.iter_mut().find(|known| known.name == space)
            && !known.agents.iter().any(|other| other.name == agent.name)
        {
            known.agents.push(agent);
        }
    }

    /// For each other paired space, the agents of `from` it has none of by
    /// name, which "Copy an agent to <space>…" offers.
    #[must_use]
    pub fn copy_targets(&self, from: &str) -> Vec<(String, Vec<Agent>)> {
        let Some(source) = self.space(from) else {
            return Vec::new();
        };
        self.spaces
            .iter()
            .filter(|other| other.name != from)
            .map(|other| {
                let missing = source
                    .agents
                    .iter()
                    .filter(|agent| !other.agents.iter().any(|known| known.name == agent.name))
                    .cloned()
                    .collect();
                (other.name.clone(), missing)
            })
            .collect()
    }

    /// Opens or closes the "+ Add repository" form of `project` in `space`.
    pub fn open_repository_form(&mut self, space: &str, project: &str, open: bool) {
        self.repository_form = open.then(|| (space.to_owned(), project.to_owned()));
    }

    /// Whether the "+ Add repository" form of `project` in `space` is open.
    #[must_use]
    pub fn repository_form_open(&self, space: &str, project: &str) -> bool {
        self.repository_form
            .as_ref()
            .is_some_and(|(at, named)| at == space && named == project)
    }

    /// Takes a repository its hub registered: the project lists it, and
    /// the form closes.
    pub fn repository_added(&mut self, space: &str, project: &str, repository: Repository) {
        self.repository_form = None;
        let view = self
            .projects
            .entry((space.to_owned(), project.to_owned()))
            .or_default();
        if !view
            .repositories
            .iter()
            .any(|known| known.id == repository.id)
        {
            view.repositories.push(repository);
        }
    }

    /// Opens the "+ New task" form of `project` in `space`, with nothing
    /// picked but the only agent offered, or closes it.
    pub fn open_task_form(&mut self, space: &str, project: &str, open: bool) {
        self.task_form = TaskForm::default();
        if open {
            let agents = self.task_agents(space, project);
            self.task_form.at = Some((space.to_owned(), project.to_owned()));
            self.task_form.agent = (agents.len() == 1).then(|| agents[0].clone());
        }
    }

    /// The "+ New task" form of `project` in `space`, while it is open.
    #[must_use]
    pub fn task_form(&self, space: &str, project: &str) -> Option<&TaskForm> {
        self.task_form
            .at
            .as_ref()
            .is_some_and(|(at, named)| at == space && named == project)
            .then_some(&self.task_form)
    }

    /// Changes the picks of the "+ New task" form.
    pub fn task_form_mut(&mut self) -> &mut TaskForm {
        &mut self.task_form
    }

    /// The agents a new task in `project` of `space` can go to: those
    /// bound there and not Off.
    #[must_use]
    pub fn task_agents(&self, space: &str, project: &str) -> Vec<String> {
        self.project(space, project)
            .map(|view| {
                view.bindings
                    .iter()
                    .filter(|binding| binding.mode != Mode::Off)
                    .map(|binding| binding.agent.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The repositories of `project` in `space`, by name.
    #[must_use]
    pub fn task_repositories(&self, space: &str, project: &str) -> Vec<String> {
        self.project(space, project)
            .map(|view| {
                view.repositories
                    .iter()
                    .map(|repository| repository.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The tasks of `project` in `space` that have not ended, which a new
    /// task can wait for: `(id, "T-0001 Title")`.
    #[must_use]
    pub fn task_dependencies(&self, space: &str, project: &str) -> Vec<(TaskId, String)> {
        self.project(space, project)
            .map(|view| {
                view.tasks
                    .iter()
                    .filter(|summary| !summary.task.status.is_terminal())
                    .map(|summary| {
                        let task = &summary.task;
                        (task.id, format!("{} {}", task.id, task.title))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The task the open "+ New task" form describes, with `title` and
    /// `goal` from its fields.
    ///
    /// # Errors
    ///
    /// What is missing, in a sentence.
    pub fn task_spec(
        &self,
        space: &str,
        project: &str,
        title: &str,
        goal: &str,
    ) -> Result<NewTaskSpec, String> {
        let form = self
            .task_form(space, project)
            .ok_or_else(|| "Open the new task form first.".to_owned())?;
        let (title, goal) = (title.trim(), goal.trim());
        if title.is_empty() || goal.is_empty() {
            return Err("Give the task a title and a goal.".to_owned());
        }
        let agent = form
            .agent
            .clone()
            .filter(|agent| self.task_agents(space, project).contains(agent))
            .ok_or_else(|| "Choose the agent that does it.".to_owned())?;
        Ok(NewTaskSpec {
            project: Some(project.to_owned()),
            title: title.to_owned(),
            goal: goal.to_owned(),
            agent,
            repositories: form.repositories.clone(),
            depends_on: form.depends_on.clone(),
        })
    }

    /// Takes a task its hub created from the form: the form closes.
    pub fn task_created(&mut self, space: &str, project: &str) {
        if self.task_form(space, project).is_some() {
            self.task_form = TaskForm::default();
        }
    }

    /// What the task screen offers to stop task `task` of `space`.
    #[must_use]
    pub fn task_control(&self, space: &str, task: TaskId) -> TaskControl {
        let Some(detail) = self.task(space, task) else {
            return TaskControl::Ended;
        };
        if detail.summary.task.status.is_terminal() {
            return TaskControl::Ended;
        }
        if self.cancelling_task.as_ref() != Some(&(space.to_owned(), task)) {
            return TaskControl::Cancel;
        }
        let waiting = detail
            .related
            .iter()
            .filter(|other| {
                other.task.depends_on.contains(&task) && !other.task.status.is_terminal()
            })
            .count();
        let also = match waiting {
            0 => String::new(),
            1 => " The task that waits for it is cancelled too.".to_owned(),
            n => format!(" The {n} tasks that wait for it are cancelled too."),
        };
        TaskControl::Confirm(format!(
            "Cancel {task} for good? It cannot be resumed.{also} The lead is told."
        ))
    }

    /// Asks to cancel task `task` of `space`, which waits for you to confirm.
    pub fn ask_cancel_task(&mut self, space: &str, task: TaskId) {
        self.cancelling_task = Some((space.to_owned(), task));
    }

    /// Keeps the task you asked to cancel.
    pub fn keep_task(&mut self) {
        self.cancelling_task = None;
    }

    /// Takes a task its hub cancelled: the task screen shows it ended.
    pub fn task_cancelled(&mut self, space: &str, task: Task) {
        if self.cancelling_task.as_ref() == Some(&(space.to_owned(), task.id)) {
            self.cancelling_task = None;
        }
        if let Some(detail) = self.tasks.get_mut(&(space.to_owned(), task.id)) {
            detail.summary.task = task;
        }
    }

    /// Whether `project` of `space` is paused, as its hub last listed it.
    #[must_use]
    pub fn is_paused(&self, space: &str, project: &str) -> bool {
        self.project_summary(space, project)
            .is_some_and(|summary| summary.paused)
    }

    fn project_summary(&self, space: &str, project: &str) -> Option<&ProjectSummary> {
        self.space(space)?
            .projects
            .iter()
            .find(|known| known.name == project)
    }

    /// Whether the project should be shown read-only.
    #[must_use]
    pub fn is_archived(&self, space: &str, project: &str) -> bool {
        self.project_summary(space, project)
            .is_some_and(|summary| summary.archived)
    }

    /// Whether a known project can offer the archive action.
    #[must_use]
    pub fn offers_archive(&self, space: &str, project: &str) -> bool {
        self.project_summary(space, project)
            .is_some_and(|summary| !summary.is_general && !summary.archived)
    }

    /// Whether a task belongs to an archived project.
    #[must_use]
    pub fn task_is_archived(&self, space: &str, task: TaskId) -> bool {
        self.task(space, task)
            .is_some_and(|detail| self.is_archived(space, &detail.summary.project))
    }

    /// Takes a project its hub paused, resumed, archived or restored.
    pub fn project_updated(&mut self, space: &str, project: ProjectSummary) {
        if let Some(known) = self
            .spaces
            .iter_mut()
            .find(|known| known.name == space)
            .and_then(|known| {
                known
                    .projects
                    .iter_mut()
                    .find(|other| other.id == project.id)
            })
        {
            *known = project;
        }
    }

    /// The "Recorded, not acted on (N)" section of `project` in `space`;
    /// `None` while nothing is recorded, or its hub cannot say.
    #[must_use]
    pub fn recorded(&self, space: &str, project: &str, when: &When) -> Option<Recorded> {
        let (events, total) = self
            .project(space, project)?
            .recorded
            .as_ref()?
            .as_ref()
            .ok()?;
        if *total == 0 {
            return None;
        }
        let shown = u64::try_from(events.len()).unwrap_or(u64::MAX);
        Some(Recorded {
            title: format!("Recorded, not acted on ({total})"),
            open: self
                .recorded_open
                .contains(&(space.to_owned(), project.to_owned())),
            rows: events
                .iter()
                .map(|event: &RecordedEvent| RecordedRow {
                    source: event.source.clone(),
                    text: first_line(&event.text),
                    time: when.moment(event.received_at),
                })
                .collect(),
            more: (*total > shown).then(|| format!("And {} earlier.", total - shown)),
        })
    }

    /// Expands or collapses the recorded section of `project` in `space`.
    pub fn toggle_recorded(&mut self, space: &str, project: &str) {
        let key = (space.to_owned(), project.to_owned());
        if !self.recorded_open.remove(&key) {
            self.recorded_open.insert(key);
        }
    }
}

/// Whether the deletion dialog has a preview and the owner typed the exact name.
#[must_use]
pub fn can_confirm_deletion(project: &str, typed: &str, preview: Option<&DeletePreview>) -> bool {
    preview.is_some() && project == typed
}

impl AppState {
    /// Whether this project can offer permanent deletion.
    #[must_use]
    pub fn offers_delete(&self, space: &str, project: &str) -> bool {
        self.project_summary(space, project)
            .is_some_and(|summary| !summary.is_general)
    }

    /// Whether the latest successful project listing confirms the project is gone.
    #[must_use]
    pub fn project_absent(&self, space: &str, project: &str) -> bool {
        self.space(space).is_some_and(|space| {
            space.gap(Part::Projects).is_none()
                && space.projects.iter().all(|known| known.name != project)
        })
    }

    /// Drops a deleted project's state and reports the durable result.
    pub fn project_deleted(
        &mut self,
        space: &str,
        project: &str,
        deleted: &DeletePreview,
        leftovers: &[String],
    ) {
        if let Some(id) = self
            .project_summary(space, project)
            .map(|project| project.id)
        {
            self.forget_project(space, project, id);
        }
        let mut text = format!("Deleted {project:?} and {}.", deleted.inventory());
        if !leftovers.is_empty() {
            text.push_str(&format!(
                " Workspace cleanup still needed for {}.",
                leftovers.join(", ")
            ));
        }
        self.report(crate::Activity::Done(text));
    }
}
