use gpui::*;
use gpui_component::WindowExt;
use gpui_component::input::{InputEvent, InputState, TextareaState};
use threadlane_automation::{Definition, Schedule, new_id, now};
use threadlane_protocol::ReasoningEffort;
use threadlane_protocol::automation::AutomationCommand as Command;
use threadlane_ui_kit::automation_form::{
    AutomationForm, AutomationFormAction, automation_editor_sheet, automation_form,
    automation_schedule_fields, automation_schedule_preview, schedule_from_fields,
};
use threadlane_ui_state::{AppState, automation_io};

fn default_project(state: &AppState) -> Option<&std::path::PathBuf> {
    state
        .projects
        .iter()
        .find(|p| state.active_work_dir.as_ref() == Some(&p.work_dir))
        .or_else(|| state.projects.first())
        .map(|p| &p.work_dir)
}

fn project_model(models: &[threadlane_daemon::catalog::ModelOption], preferred: &str) -> String {
    let mut native = models.iter().filter(|m| !m.id.starts_with("acp/"));
    native
        .clone()
        .find(|m| m.id == preferred)
        .or_else(|| native.next())
        .map(|m| m.id.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{default_project, project_model};
    use threadlane_automation::Schedule;
    use threadlane_daemon::catalog::{ModelOption, ModelProvider};
    use threadlane_ui_kit::automation_form::{calendar_cadence, calendar_days};
    use threadlane_ui_state::{AppState, activate_test_session};

    #[test]
    fn unrelated_edits_preserve_every_calendar_day_set_and_order() {
        // Includes all valid nonempty subsets plus unordered weekday/daily sets.
        let mut sets: Vec<Vec<u32>> = (1..128)
            .map(|mask| (0..7).filter(|d| mask & (1 << d) != 0).collect())
            .collect();
        sets.extend([
            vec![4, 2, 0, 3, 1],
            vec![6, 5, 4, 3, 2, 1, 0],
            vec![0, 0, 0, 0, 0, 0, 0],
        ]);
        for days in sets {
            let schedule = Schedule::Calendar {
                hour: 9,
                minute: 0,
                days: days.clone(),
                timezone: "America/Toronto".into(),
            };
            assert_eq!(
                calendar_days(calendar_cadence(&days), days[0], &schedule),
                days
            );
            // Explicit cadence changes may replace the old set.
            assert_eq!(calendar_days("weekly", 3, &schedule), vec![3]);
        }
    }

    #[test]
    fn defaults_and_project_switches_use_the_destination_catalog() {
        let mut state = AppState::for_tests();
        state.projects.clear();
        activate_test_session(&mut state, "a", std::path::Path::new("/project-a/a.jsonl"));
        activate_test_session(&mut state, "b", std::path::Path::new("/project-b/b.jsonl"));
        state.selected_model = "project-b-model".into();
        assert_eq!(
            default_project(&state).unwrap().clone(),
            std::path::PathBuf::from("/project-b")
        );
        let models = [
            ModelOption {
                id: "acp/external".into(),
                label: "External".into(),
                provider: ModelProvider::Acp,
            },
            ModelOption {
                id: "project-a-model".into(),
                label: "A".into(),
                provider: ModelProvider::OpenAi,
            },
        ];
        assert_eq!(
            project_model(&models, &state.selected_model),
            "project-a-model"
        );
        assert_eq!(project_model(&models, "project-a-model"), "project-a-model");
        assert_eq!(project_model(&[], &state.selected_model), "");
        state.active_work_dir = None;
        assert_eq!(
            default_project(&state).unwrap().clone(),
            std::path::PathBuf::from("/project-a")
        );
    }
}

struct Editor {
    model: Entity<AppState>,
    definition: Definition,
    name: Entity<InputState>,
    prompt: Entity<TextareaState>,
    interval: Entity<InputState>,
    time: Entity<InputState>,
    timezone: Entity<InputState>,
    cadence: String,
    weekday: u32,
    busy: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
    models: Vec<threadlane_daemon::catalog::ModelOption>,
    is_git: bool,
    closed: bool,
}
pub(super) fn open(
    model: Entity<AppState>,
    definition: Option<Definition>,
    window: &mut Window,
    cx: &mut App,
) {
    let state = model.read(cx);
    let Some(project) = default_project(state) else {
        return;
    };
    let definition = definition.unwrap_or_else(|| {
        let models = threadlane_daemon::catalog::available_models_for_project(Some(project));
        let selected_model = project_model(&models, &state.selected_model);
        let effort = threadlane_provider::model_registry::effective_effort(
            &selected_model,
            state.reasoning_effort.clone(),
            Some(project),
        );
        Definition {
            id: new_id(),
            revision: 0,
            name: String::new(),
            prompt: String::new(),
            project: project.clone(),
            model: selected_model,
            effort: effort.label().into(),
            worktree: true,
            schedule: Schedule::Interval { minutes: 60 },
            enabled: true,
            notify_all: false,
            anchor: now(),
            next_at: None,
            failures: 0,
            paused_reason: None,
        }
    });
    let editing = definition.revision != 0;
    let (cadence, interval, time, timezone, weekday) =
        automation_schedule_fields(&definition.schedule);
    let editor = cx.new(|cx| {
        let name = cx.new(|cx| InputState::new(window, cx).default_value(&definition.name));
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .default_value(&definition.prompt)
                .auto_grow(4, 10)
                .soft_wrap(true)
        });
        let interval = cx.new(|cx| InputState::new(window, cx).default_value(interval));
        let time = cx.new(|cx| InputState::new(window, cx).default_value(time));
        let timezone = cx.new(|cx| InputState::new(window, cx).default_value(timezone));
        let mut subscriptions: Vec<_> = [&name, &interval, &time, &timezone]
            .iter()
            .map(|input| cx.observe(*input, |_: &mut Editor, _, cx| cx.notify()))
            .collect();
        subscriptions.push(cx.observe(&prompt, |_: &mut Editor, _, cx| cx.notify()));
        subscriptions.push(cx.subscribe_in(
            &name,
            window,
            |this: &mut Editor, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save(window, cx);
                }
            },
        ));
        let models =
            threadlane_daemon::catalog::available_models_for_project(Some(&definition.project));
        // Keep the safe worktree default until background discovery completes.
        let is_git = true;
        Editor {
            model,
            definition,
            name,
            prompt,
            interval,
            time,
            timezone,
            cadence: cadence.into(),
            weekday,
            busy: false,
            error: None,
            _subscriptions: subscriptions,
            models,
            is_git,
            closed: false,
        }
    });
    editor.update(cx, |this, cx| this.refresh_project(cx));
    window.open_sheet(cx, move |sheet, _, _| {
        let owner = editor.downgrade();
        automation_editor_sheet(sheet, editing, editor.clone()).on_close(move |_, _, cx| {
            let _ = owner.update(cx, |this, _| this.closed = true);
        })
    });
}
impl Editor {
    fn refresh_project(&mut self, cx: &mut Context<Self>) {
        let project = self.definition.project.clone();
        let task_project = project.clone();
        let client = self.model.read(cx).daemon_client.clone();
        let task = threadlane_provider::exec::get_runtime().spawn(async move {
            threadlane_ui_state::project_io::is_repo(&client, &task_project).await
        });
        cx.spawn(async move |owner, cx| {
            // A failed probe says nothing about the repo — keep the
            // current worktree flag rather than falsing it on an error.
            if let Ok(Ok(is_git)) = task.await {
                let _ = owner.update(cx, |this, cx| {
                    if !this.closed && this.definition.project == project {
                        this.is_git = is_git;
                        if !is_git {
                            this.definition.worktree = false;
                        }
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn schedule(&self, cx: &App) -> Result<Schedule, String> {
        schedule_from_fields(
            &self.cadence,
            &self.interval.read(cx).value(),
            &self.time.read(cx).value(),
            &self.timezone.read(cx).value(),
            self.weekday,
            &self.definition.schedule,
        )
    }

    /// Validate the draft and save on the owning daemon, closing only after success.
    /// Validation and transport failures remain visible in the open editor.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let mut definition = self.definition.clone();
        definition.name = self.name.read(cx).value().trim().into();
        definition.prompt = self.prompt.read(cx).value().trim().into();
        let result = self.schedule(cx).and_then(|schedule| {
            definition.schedule = schedule;
            definition.validate()
        });
        if let Err(error) = result {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let client = self.model.read(cx).daemon_client.clone();
        self.busy = true;
        self.error = None;
        let task = threadlane_provider::exec::get_runtime().spawn(async move {
            automation_io::mutate(&client, Command::Save { definition }).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task
                .await
                .map_err(|error| format!("Automation request failed: {error}"))
                .and_then(|result| result);
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if !this.closed {
                            window.close_sheet(cx);
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = AutomationForm {
            definition: &self.definition,
            name: &self.name,
            prompt: &self.prompt,
            interval: &self.interval,
            time: &self.time,
            timezone: &self.timezone,
            cadence: &self.cadence,
            weekday: self.weekday,
            projects: self
                .model
                .read(cx)
                .projects
                .iter()
                .map(|p| (p.work_dir.to_string_lossy().into_owned(), p.name.clone()))
                .collect(),
            model_label: threadlane_daemon::catalog::selection_label(
                &self.definition.model,
                &self.models,
            ),
            models: self
                .models
                .iter()
                .filter(|m| !m.id.starts_with("acp/"))
                .map(|m| (m.id.clone(), m.label.clone()))
                .collect(),
            efforts: threadlane_daemon::catalog::efforts_for_model(
                &self.definition.model,
                Some(&self.definition.project),
            )
            .iter()
            .map(|e| (e.label().into(), e.label().into()))
            .collect(),
            show_effort: threadlane_daemon::catalog::supports_reasoning(
                &self.definition.model,
                Some(&self.definition.project),
            ),
            is_git: self.is_git,
            busy: self.busy,
            error: self.error.clone(),
            schedule_preview: automation_schedule_preview(self.schedule(cx), now()),
        };
        let owner = cx.entity().downgrade();
        automation_form(
            form,
            move |action, window, cx| {
                let _ = owner.update(cx, |this, cx| {
                    match action {
                        AutomationFormAction::Starter(starter) => {
                            if !this.busy
                                && threadlane_ui_kit::automation_form::can_apply_starter(
                                    this.definition.revision,
                                    &this.name.read(cx).value(),
                                    &this.prompt.read(cx).value(),
                                )
                            {
                                this.name.update(cx, |input, cx| {
                                    input.set_value(starter.title(), window, cx)
                                });
                                this.prompt.update(cx, |input, cx| {
                                    input.set_value(starter.prompt(), window, cx)
                                });
                            }
                        }
                        AutomationFormAction::Project(id) => {
                            this.definition.project = id.into();
                            this.is_git = true;
                            this.refresh_project(cx);
                            this.models = threadlane_daemon::catalog::available_models_for_project(
                                Some(&this.definition.project),
                            );
                            this.definition.model =
                                project_model(&this.models, &this.definition.model);
                            this.definition.effort =
                                threadlane_provider::model_registry::effective_effort(
                                    &this.definition.model,
                                    ReasoningEffort::from_label(&this.definition.effort)
                                        .unwrap_or_default(),
                                    Some(&this.definition.project),
                                )
                                .label()
                                .into();
                        }
                        AutomationFormAction::Model(id) => {
                            this.definition.effort =
                                threadlane_provider::model_registry::effective_effort(
                                    &id,
                                    ReasoningEffort::from_label(&this.definition.effort)
                                        .unwrap_or_default(),
                                    Some(&this.definition.project),
                                )
                                .label()
                                .into();
                            this.definition.model = id;
                        }
                        AutomationFormAction::Effort(id) => this.definition.effort = id,
                        AutomationFormAction::Cadence(id) => this.cadence = id,
                        AutomationFormAction::Weekday(day) => this.weekday = day,
                        AutomationFormAction::Worktree(worktree) => {
                            this.definition.worktree = worktree
                        }
                        AutomationFormAction::NotifyAll(notify) => {
                            this.definition.notify_all = notify
                        }
                        AutomationFormAction::Save => this.save(window, cx),
                        AutomationFormAction::Cancel => window.close_sheet(cx),
                    }
                    cx.notify();
                });
            },
            cx,
        )
    }
}
