//! What the fifth stage's buttons ask of a hub: a new agent or a copy of
//! one, a repository, a task, cancelling a task, pausing a project, and
//! the notification settings. Each runs on the Tokio threads and reports
//! in the status area.

use std::sync::Arc;

use gpui_kit::*;
use threadlane_core::client::LaneTarget;
use threadlane_core::ids::{StandingRuleId, TaskId};
use threadlane_core::model::{Agent, Cell, Mode};
use threadlane_core::tools::Tier;
use threadlane_desktop::{Activity, add_repository, copy_agent, describe, mode_label};

use crate::Desktop;

impl Desktop {
    /// Requests a lane stop, then reloads the screen with the hub's lane status.
    pub(crate) fn stop_lane(&mut self, space: String, target: LaneTarget, cx: &mut Context<Self>) {
        if !self.state.begin_stop(&space, &target) {
            return;
        }
        self.state
            .report(Activity::Working("Requesting a stop…".to_owned()));
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let result = core.read().await.stop(Some(&space), target.clone()).await;
                (space, target, result)
            },
            |this, (space, target, result), cx| {
                this.state.finish_stop(&space, &target, result);
                this.load_screen(cx);
            },
            cx,
        );
        cx.notify();
    }

    /// Saves the draft entered in the focused space's pacing controls.
    pub(crate) fn save_pacing(&mut self, space: String, cx: &mut Context<Self>) {
        let values = self
            .pacing_inputs
            .each_ref()
            .map(|input| input.read(cx).value());
        let Some(pacing) = self
            .state
            .submit_pacing(&space, values.each_ref().map(|value| value.as_ref()))
        else {
            cx.notify();
            return;
        };
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let result = core.read().await.set_pacing(Some(&space), pacing).await;
                (space, result)
            },
            |this, (space, result), cx| {
                let saved = result.is_ok();
                this.state.finish_pacing(&space, result);
                if saved {
                    this.refresh(cx);
                }
            },
            cx,
        );
        cx.notify();
    }

    /// Opens the "+ New agent" form in `space` and lists its templates.
    pub(crate) fn open_agent_form(&mut self, space: String, cx: &mut Context<Self>) {
        self.state.open_agent_form(&space);
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let templates = core.read().await.agent_templates(Some(&space)).await;
                (space, templates)
            },
            |this, (space, templates), _| this.state.templates_loaded(&space, templates),
            cx,
        );
        cx.notify();
    }

    /// Creates the agent the "+ New agent" form describes in `space`.
    pub(crate) fn create_agent(&mut self, space: String, cx: &mut Context<Self>) {
        let name = self.agent_name.read(cx).value().trim().to_owned();
        let role = self.agent_role.read(cx).value().trim().to_owned();
        let instructions = self.agent_instructions.read(cx).value().trim().to_owned();
        if name.is_empty() || role.is_empty() {
            self.state.report(Activity::Failed(
                "Give the agent a name and a role.".to_owned(),
            ));
            cx.notify();
            return;
        }
        let backends = self.state.agent_form().backends();
        let models = threadlane_core::model::BackendKind::ALL
            .iter()
            .zip(&self.agent_models)
            .filter(|(backend, _)| backends.is_empty() || backends.contains(backend))
            .filter_map(|(backend, field)| {
                let value = field.read(cx).value();
                let model = value.trim();
                (!model.is_empty()).then(|| (backend.as_str().to_owned(), model.to_owned()))
            })
            .collect();
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Creating {name} in {space}…")));
        self.run(
            async move {
                let created = core
                    .read()
                    .await
                    .create_agent_with_models(
                        Some(&space),
                        &name,
                        &role,
                        &instructions,
                        &backends,
                        models,
                    )
                    .await;
                (space, created)
            },
            |this, (space, created), _| match created {
                Ok(agent) => {
                    this.state.report(Activity::Done(format!(
                        "Created {} in {space}. Bind it to a project in the grid below.",
                        agent.name
                    )));
                    this.state.agent_created(&space, agent);
                    this.clear_fields.extend(this.agent_models.iter().cloned());
                    this.clear_fields.extend([
                        this.agent_name.clone(),
                        this.agent_role.clone(),
                        this.agent_instructions.clone(),
                    ]);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Creates `agent`'s definition in the space `to`.
    pub(crate) fn copy_agent(&mut self, agent: Agent, to: String, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.state.report(Activity::Working(format!(
            "Copying {} to {to}…",
            agent.name
        )));
        self.run(
            async move {
                let copied = copy_agent(&*core.read().await, &agent, &to).await;
                (to, copied)
            },
            |this, (to, copied), _| match copied {
                Ok(agent) => {
                    this.state
                        .report(Activity::Done(format!("Copied {} to {to}.", agent.name)));
                    this.state.agent_created(&to, agent);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Registers the repository the "+ Add repository" form describes,
    /// once no other paired space has its remote.
    pub(crate) fn add_repository(
        &mut self,
        space: String,
        project: String,
        cx: &mut Context<Self>,
    ) {
        let name = self.repository_name.read(cx).value().to_string();
        let remote = self.repository_remote.read(cx).value().to_string();
        let branch = self.repository_branch.read(cx).value().to_string();
        let core = Arc::clone(&self.core);
        self.state.report(Activity::Working(format!(
            "Checking every space, then adding {} to {space} / {project}…",
            name.trim()
        )));
        self.run(
            async move {
                let added = add_repository(
                    &*core.read().await,
                    &space,
                    &project,
                    &name,
                    &remote,
                    &branch,
                )
                .await;
                (space, project, added)
            },
            |this, (space, project, added), _| match added {
                Ok(repository) => {
                    this.state.report(Activity::Done(format!(
                        "Added {} to {space} / {project}.",
                        repository.name
                    )));
                    this.state.repository_added(&space, &project, repository);
                    this.clear_fields.extend([
                        this.repository_name.clone(),
                        this.repository_remote.clone(),
                        this.repository_branch.clone(),
                    ]);
                }
                Err(why) => this.state.report(Activity::Failed(why)),
            },
            cx,
        );
        cx.notify();
    }

    /// Creates the task the "+ New task" form describes, as yours.
    pub(crate) fn create_task(&mut self, space: String, project: String, cx: &mut Context<Self>) {
        let title = self.task_title.read(cx).value().to_string();
        let goal = self.task_goal.read(cx).value().to_string();
        let spec = match self.state.task_spec(&space, &project, &title, &goal) {
            Ok(spec) => spec,
            Err(why) => {
                self.state.report(Activity::Failed(why));
                cx.notify();
                return;
            }
        };
        let core = Arc::clone(&self.core);
        self.state.report(Activity::Working(format!(
            "Creating a task in {space} / {project}…"
        )));
        self.run(
            async move {
                let created = core.read().await.create_task(Some(&space), spec).await;
                (space, project, created)
            },
            |this, (space, project, created), cx| match created {
                Ok(task) => {
                    this.state.report(Activity::Done(format!(
                        "Created {} in {space} / {project}.",
                        task.id
                    )));
                    this.state.task_created(&space, &project);
                    this.clear_fields
                        .extend([this.task_title.clone(), this.task_goal.clone()]);
                    this.load_screen(cx);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Cancels task `task` of `space`, once you confirmed.
    pub(crate) fn cancel_task(&mut self, space: String, task: TaskId, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Cancelling {space} / {task}…")));
        self.run(
            async move {
                let cancelled = core
                    .read()
                    .await
                    .cancel_task(Some(&space), &task.to_string())
                    .await;
                (space, cancelled)
            },
            |this, (space, cancelled), cx| match cancelled {
                Ok(task) => {
                    this.state
                        .report(Activity::Done(format!("Cancelled {space} / {}.", task.id)));
                    this.state.task_cancelled(&space, task);
                    this.load_screen(cx);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Queues failed task `task` of `space` again.
    pub(crate) fn retry_task(&mut self, space: String, task: TaskId, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Retrying {space} / {task}…")));
        self.run(
            async move {
                let retried = core
                    .read()
                    .await
                    .retry_task(Some(&space), &task.to_string())
                    .await;
                (space, retried)
            },
            |this, (space, retried), cx| match retried {
                Ok(task) => {
                    this.state.report(Activity::Done(format!(
                        "Queued {space} / {} again.",
                        task.id
                    )));
                    this.load_screen(cx);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Pauses `project` of `space`, or resumes it.
    pub(crate) fn set_paused(
        &mut self,
        space: String,
        project: String,
        paused: bool,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        let doing = if paused { "Pausing" } else { "Resuming" };
        self.state
            .report(Activity::Working(format!("{doing} {space} / {project}…")));
        self.run(
            async move {
                let set = core
                    .read()
                    .await
                    .set_paused(Some(&space), Some(&project), paused)
                    .await;
                (space, set)
            },
            |this, (space, set), _| match set {
                Ok(project) => {
                    let done = if project.paused {
                        "Paused; none of its work starts until you resume it."
                    } else {
                        "Resumed."
                    };
                    this.state.report(Activity::Done(format!(
                        "{space} / {}: {done}",
                        project.name
                    )));
                    this.state.project_updated(&space, project);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Archives or restores a project and updates the visible state.
    pub(crate) fn set_archived(
        &mut self,
        space: String,
        project: String,
        archived: bool,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        let doing = if archived { "Archiving" } else { "Unarchiving" };
        self.state
            .report(Activity::Working(format!("{doing} {space} / {project}…")));
        self.run(
            async move {
                let set = core
                    .read()
                    .await
                    .set_archived(Some(&space), &project, archived)
                    .await;
                (space, set)
            },
            |this, (space, set), cx| match set {
                Ok(project) => {
                    let done = if project.archived {
                        "Archived"
                    } else {
                        "Unarchived"
                    };
                    this.state.report(Activity::Done(format!(
                        "{done} {space} / {}.",
                        project.name
                    )));
                    this.state.project_updated(&space, project);
                    this.refresh(cx);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Turns the notifications for decisions, or for system notices, on or
    /// off.
    pub(crate) fn set_notifications(&mut self, decisions: bool, on: bool, cx: &mut Context<Self>) {
        if let Err(why) = self.state.set_notifications(decisions, on) {
            self.state.report(Activity::Failed(why));
        }
        cx.notify();
    }
}

impl Desktop {
    /// Sets the ceiling of `project` in `space`.
    pub(crate) fn set_ceiling(
        &mut self,
        space: String,
        project: String,
        mode: Mode,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let set = core
                    .read()
                    .await
                    .set_ceiling(Some(&space), Some(&project), mode)
                    .await;
                (space, project, set)
            },
            move |this, (space, project, set), _| match set {
                Ok(ceiling) => {
                    this.state.ceiling_set(&space, &project, mode);
                    this.state.report(Activity::Done(format!(
                        "The ceiling of {space} / {project} is now {}.",
                        mode_label(ceiling)
                    )));
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Sets, or with `None` clears, the cell of `tier` for `agent` in
    /// `project` of `space`.
    pub(crate) fn set_override(
        &mut self,
        space: String,
        project: String,
        agent: String,
        tier: Tier,
        cell: Option<Cell>,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let set = core
                    .read()
                    .await
                    .set_override(Some(&space), Some(&project), &agent, tier, cell)
                    .await;
                (space, project, agent, set)
            },
            |this, (space, project, agent, set), _| match set {
                Ok(overrides) => {
                    this.state
                        .overrides_set(&space, &project, &agent, overrides);
                    this.state.report(Activity::Done(format!(
                        "Changed what {agent} may do in {space} / {project}."
                    )));
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Revokes standing rule `rule` of `space` once you confirmed.
    pub(crate) fn revoke_rule(
        &mut self,
        space: String,
        rule: StandingRuleId,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let revoked = core
                    .read()
                    .await
                    .revoke_rule(Some(&space), &rule.to_string())
                    .await;
                (space, revoked)
            },
            |this, (space, revoked), _| match revoked {
                Ok(revoked) => {
                    this.state.rule_revoked(&space, &revoked);
                    this.state.report(Activity::Done(format!(
                        "Revoked rule {} of {}; its next matching call asks you again.",
                        revoked.rule.id, revoked.agent
                    )));
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Sets the ceiling of `space` itself once you confirmed.
    pub(crate) fn set_space_ceiling(&mut self, space: String, mode: Mode, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let set = core
                    .read()
                    .await
                    .set_space_ceiling(Some(&space), mode)
                    .await;
                (space, set)
            },
            |this, (space, set), _| match set {
                Ok(ceiling) => {
                    this.state.space_ceiling_set(&space, ceiling);
                    this.state.report(Activity::Done(format!(
                        "The ceiling of {space} is now {}.",
                        mode_label(ceiling)
                    )));
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }
}
