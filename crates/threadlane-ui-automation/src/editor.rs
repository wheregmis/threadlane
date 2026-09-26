use super::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use threadlane_automation::{new_id, now, Schedule};
use threadlane_protocol::ReasoningEffort;

fn default_project(state: &AppState) -> Option<&std::path::PathBuf> {
    state
        .projects
        .iter()
        .find(|p| state.active_work_dir.as_ref() == Some(&p.work_dir))
        .or_else(|| state.projects.first())
        .map(|p| &p.work_dir)
}

fn project_model(models: &[threadlane_ui_catalog::ModelOption], preferred: &str) -> String {
    let mut native = models.iter().filter(|m| !m.id.starts_with("acp/"));
    native
        .clone()
        .find(|m| m.id == preferred)
        .or_else(|| native.next())
        .map(|m| m.id.clone())
        .unwrap_or_default()
}

fn calendar_cadence(days: &[u32]) -> &'static str {
    match days {
        [0, 1, 2, 3, 4, 5, 6] => "daily",
        [0, 1, 2, 3, 4] => "weekdays",
        [_] => "weekly",
        _ => "custom",
    }
}

fn calendar_days(cadence: &str, weekday: u32, original: &Schedule) -> Vec<u32> {
    match cadence {
        "daily" => (0..7).collect(),
        "weekdays" => (0..5).collect(),
        "custom" => match original {
            Schedule::Calendar { days, .. } => days.clone(),
            _ => Vec::new(),
        },
        _ => vec![weekday],
    }
}

#[cfg(test)]
mod tests {
    use super::{calendar_cadence, calendar_days, default_project, project_model};
    use threadlane_automation::Schedule;
    use threadlane_ui_catalog::{ModelOption, ModelProvider};
    use threadlane_ui_state::{activate_test_session, AppState};

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
        let mut state = AppState::default();
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
    models: Vec<threadlane_ui_catalog::ModelOption>,
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
        let models = threadlane_ui_catalog::available_models_for_project(Some(project));
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
    let title = if definition.revision == 0 {
        "New automation"
    } else {
        "Edit automation"
    };
    let (cadence, interval, time, timezone, weekday) = match &definition.schedule {
        Schedule::Manual => ("manual", "60".into(), "09:00".into(), "UTC".into(), 0),
        Schedule::Interval { minutes } => (
            "interval",
            minutes.to_string(),
            "09:00".into(),
            "UTC".into(),
            0,
        ),
        Schedule::Calendar {
            hour,
            minute,
            days,
            timezone,
        } => (
            calendar_cadence(days),
            "60".into(),
            format!("{hour:02}:{minute:02}"),
            timezone.clone(),
            days.first().copied().unwrap_or(0),
        ),
    };
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
        let mut subscriptions: Vec<_> = [&interval, &time, &timezone]
            .iter()
            .map(|input| cx.observe(*input, |_: &mut Editor, _, cx| cx.notify()))
            .collect();
        subscriptions.push(cx.subscribe_in(
            &name,
            window,
            |this: &mut Editor, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save(window, cx);
                }
            },
        ));
        let models = threadlane_ui_catalog::available_models_for_project(Some(&definition.project));
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
        sheet
            .title(title)
            .size(rems(34.0))
            .child(editor.clone())
            .on_close(move |_, _, cx| {
                let _ = owner.update(cx, |this, _| this.closed = true);
            })
    });
}
impl Editor {
    fn refresh_project(&mut self, cx: &mut Context<Self>) {
        let project = self.definition.project.clone();
        let task_project = project.clone();
        let task = threadlane_provider::exec::get_runtime()
            .spawn_blocking(move || threadlane_git::is_git_repo(&task_project));
        cx.spawn(async move |owner, cx| {
            if let Ok(is_git) = task.await {
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
        match self.cadence.as_str() {
            "manual" => Ok(Schedule::Manual),
            "interval" => Ok(Schedule::Interval {
                minutes: self
                    .interval
                    .read(cx)
                    .value()
                    .trim()
                    .parse()
                    .map_err(|_| "Enter an interval in whole minutes")?,
            }),
            _ => {
                let value = self.time.read(cx).value();
                let (hour, minute) = value
                    .trim()
                    .split_once(':')
                    .ok_or("Enter a time as HH:MM")?;
                Ok(Schedule::Calendar {
                    hour: hour.parse().map_err(|_| "Invalid hour")?,
                    minute: minute.parse().map_err(|_| "Invalid minute")?,
                    days: calendar_days(&self.cadence, self.weekday, &self.definition.schedule),
                    timezone: self.timezone.read(cx).value().trim().into(),
                })
            }
        }
    }
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
        let Some(service) = self.model.read(cx).automation_service.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = service.command(Command::Save(definition)).await;
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
fn field(label: &'static str, control: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().child(label))
        .child(control)
}
impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let projects: Vec<_> = self
            .model
            .read(cx)
            .projects
            .iter()
            .map(|p| (p.work_dir.to_string_lossy().into_owned(), p.name.clone()))
            .collect();
        let project_id = self.definition.project.to_string_lossy().into_owned();
        let project_label = projects
            .iter()
            .find(|(id, _)| *id == project_id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| "Project unavailable".into());
        let owner = cx.entity().downgrade();
        let project = picker(
            "automation-project",
            project_label,
            projects,
            project_id,
            self.busy,
            move |id, cx| {
                let _ = owner.update(cx, |this, cx| {
                    this.definition.project = id.into();
                    this.is_git = true;
                    this.definition.worktree = true;
                    this.refresh_project(cx);
                    this.models = threadlane_ui_catalog::available_models_for_project(Some(
                        &this.definition.project,
                    ));
                    this.definition.model = project_model(&this.models, &this.definition.model);
                    this.definition.effort = threadlane_provider::model_registry::effective_effort(
                        &this.definition.model,
                        ReasoningEffort::from_label(&this.definition.effort).unwrap_or_default(),
                        Some(&this.definition.project),
                    )
                    .label()
                    .into();
                    cx.notify();
                });
            },
        );
        let models = self.models.clone();
        let model_label = threadlane_ui_catalog::selection_label(&self.definition.model, &models);
        let choices = models
            .into_iter()
            .filter(|m| !m.id.starts_with("acp/"))
            .map(|m| (m.id, m.label))
            .collect();
        let owner = cx.entity().downgrade();
        let model = picker(
            "automation-model",
            model_label,
            choices,
            self.definition.model.clone(),
            self.busy,
            move |id, cx| {
                let _ = owner.update(cx, |this, cx| {
                    this.definition.effort = threadlane_provider::model_registry::effective_effort(
                        &id,
                        ReasoningEffort::from_label(&this.definition.effort).unwrap_or_default(),
                        Some(&this.definition.project),
                    )
                    .label()
                    .into();
                    this.definition.model = id;
                    cx.notify();
                });
            },
        );
        let owner = cx.entity().downgrade();
        let efforts = threadlane_ui_catalog::efforts_for_model(
            &self.definition.model,
            Some(&self.definition.project),
        )
        .iter()
        .map(|e| (e.label().into(), e.label().into()))
        .collect();
        let effort = picker(
            "automation-effort",
            self.definition.effort.clone(),
            efforts,
            self.definition.effort.clone(),
            self.busy,
            move |id, cx| {
                let _ = owner.update(cx, |this, cx| {
                    this.definition.effort = id;
                    cx.notify();
                });
            },
        );
        let mut cadences = vec![
            ("manual", "Manual"),
            ("interval", "Every N minutes"),
            ("daily", "Daily"),
            ("weekdays", "Weekdays"),
            ("weekly", "Weekly"),
        ];
        if matches!(&self.definition.schedule, Schedule::Calendar { days, .. } if calendar_cadence(days) == "custom")
        {
            cadences.push(("custom", "Custom days"));
        }
        let label = cadences
            .iter()
            .find(|(id, _)| *id == self.cadence)
            .unwrap()
            .1
            .to_string();
        let owner = cx.entity().downgrade();
        let cadence = picker(
            "automation-cadence",
            label,
            cadences
                .into_iter()
                .map(|(id, label)| (id.into(), label.into()))
                .collect(),
            self.cadence.clone(),
            self.busy,
            move |id, cx| {
                let _ = owner.update(cx, |this, cx| {
                    this.cadence = id;
                    cx.notify();
                });
            },
        );
        let mut body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(field(
                "Name",
                Input::new(&self.name)
                    .aria_label("Automation name")
                    .disabled(self.busy),
            ))
            .child(field(
                "Prompt",
                Textarea::new(&self.prompt)
                    .aria_label("Automation prompt")
                    .disabled(self.busy),
            ))
            .child(field("Project", project))
            .child(field("Model", model))
            .when(
                threadlane_ui_catalog::supports_reasoning(
                    &self.definition.model,
                    Some(&self.definition.project),
                ),
                |body| body.child(field("Reasoning effort", effort)),
            )
            .child(field("Schedule", cadence));
        if self.cadence == "interval" {
            body = body.child(field(
                "Minutes between runs",
                Input::new(&self.interval)
                    .aria_label("Minutes between runs")
                    .disabled(self.busy),
            ));
        }
        if self.cadence == "custom" {
            if let Schedule::Calendar { days, .. } = &self.definition.schedule {
                let labels = [
                    "Monday",
                    "Tuesday",
                    "Wednesday",
                    "Thursday",
                    "Friday",
                    "Saturday",
                    "Sunday",
                ];
                let days = days
                    .iter()
                    .filter_map(|d| labels.get(*d as usize).copied())
                    .collect::<Vec<_>>()
                    .join(", ");
                body = body.child(div().text_sm().child(format!("Days: {days}")));
            }
        }
        if ["daily", "weekdays", "weekly", "custom"].contains(&self.cadence.as_str()) {
            body = body
                .child(field(
                    "Time (HH:MM)",
                    Input::new(&self.time)
                        .aria_label("Time in HH:MM")
                        .disabled(self.busy),
                ))
                .child(field(
                    "Timezone (IANA name)",
                    Input::new(&self.timezone)
                        .aria_label("IANA timezone")
                        .disabled(self.busy),
                ));
        }
        if self.cadence == "weekly" {
            let days = [
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
                "Sunday",
            ];
            let owner = cx.entity().downgrade();
            body = body.child(field(
                "Day",
                picker(
                    "automation-day",
                    days[self.weekday as usize].into(),
                    days.iter()
                        .enumerate()
                        .map(|(i, day)| (i.to_string(), day.to_string()))
                        .collect(),
                    self.weekday.to_string(),
                    self.busy,
                    move |id, cx| {
                        let _ = owner.update(cx, |this, cx| {
                            this.weekday = id.parse().unwrap_or(0);
                            cx.notify();
                        });
                    },
                ),
            ));
        }
        let preview = self.schedule(cx).and_then(|s| {
            let mut after = now();
            let mut times = Vec::new();
            for _ in 0..3 {
                if let Some(next) = s.next(after, now())? {
                    times.push(display_time(next, s.timezone()));
                    after = next;
                }
            }
            Ok(if times.is_empty() {
                "Runs only when you choose Run now".into()
            } else {
                format!("Next runs: {}", times.join(" · "))
            })
        });
        body = body.child(div().text_sm().text_color(cx.theme().muted_foreground).child(preview.unwrap_or_else(|error| error)))
            .child(Checkbox::new("automation-worktree").label("Use a fresh worktree for each run").checked(self.definition.worktree)
                .disabled(self.busy || !self.is_git)
                .on_click(cx.listener(|this, checked, _, cx| { this.definition.worktree = *checked; cx.notify(); })))
            .when(!self.definition.worktree, |body| body.child(div().text_sm().text_color(cx.theme().warning).child("Runs can modify files in the project checkout.")))
            .child(Checkbox::new("automation-notify").label("Notify on every completion").checked(self.definition.notify_all).disabled(self.busy)
                .on_click(cx.listener(|this, checked, _, cx| { this.definition.notify_all = *checked; cx.notify(); })))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Runs while Threadlane is open and your computer is awake. Permission and question requests wait for you in the run’s chat. Saving does not run the prompt immediately."))
            .children(self.error.clone().map(|error| div().text_color(cx.theme().danger).child(error)))
            .child(div().flex().justify_end().gap_2()
                .child(Button::new("automation-editor-cancel").label("Cancel").disabled(self.busy).on_click(|_, window, cx| window.close_sheet(cx)))
                .child(Button::new("automation-editor-save").debug_selector(|| "automation-editor-save".into()).primary().label(if self.busy { "Saving…" } else { "Save" }).disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)))));
        body.key_context("AutomationEditor")
            .on_action(cx.listener(|this, _: &SaveAutomation, window, cx| this.save(window, cx)))
    }
}
