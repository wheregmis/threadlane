//! Shared automation editor presentation and pure schedule-field conversion.
use super::automation::automation_picker;
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::sheet::Sheet;
use gpui_component::{ActiveTheme, Disableable, Sizable, StyledExt};
use std::rc::Rc;
use threadlane_automation::{Definition, Schedule, display_time};

actions!(threadlane_automation_ui, [SaveAutomation]);

/// Register the editor's contextual save shortcut on each host.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-enter", SaveAutomation, Some("AutomationEditor")),
        KeyBinding::new("ctrl-enter", SaveAutomation, Some("AutomationEditor")),
        // Match at the focused input so its secondary Enter cannot insert a newline.
        KeyBinding::new(
            "cmd-enter",
            SaveAutomation,
            Some("AutomationEditor > Input"),
        ),
        KeyBinding::new(
            "ctrl-enter",
            SaveAutomation,
            Some("AutomationEditor > Input"),
        ),
    ]);
}

/// Requested form changes. Inputs and persistence belong to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutomationFormAction {
    Starter(AutomationStarter),
    Project(String),
    Model(String),
    Effort(String),
    Cadence(String),
    Weekday(u32),
    Worktree(bool),
    NotifyAll(bool),
    Save,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationStarter {
    ProjectReview,
    TestGaps,
    DependencyReview,
}

impl AutomationStarter {
    pub fn title(self) -> &'static str {
        match self {
            Self::ProjectReview => "Review recent changes",
            Self::TestGaps => "Find test gaps",
            Self::DependencyReview => "Review dependencies",
        }
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Self::ProjectReview => {
                "Review the last 24 hours of commits in this project. Summarize the changes, identify potential regressions with file references, and suggest follow-up checks. If there are no recent commits, say so. Do not modify files, create issues, or publish anything."
            }
            Self::TestGaps => {
                "Inspect recent changes and their tests in this project. Identify important behaviors that lack coverage, prioritizing concrete regressions and edge cases. Report file references and suggested tests. Do not modify files, create issues, or publish anything."
            }
            Self::DependencyReview => {
                "Review this project's dependency manifests and lockfiles. Highlight maintenance risks and updates worth investigating, distinguishing verified findings from recommendations. Do not claim vulnerabilities without evidence. Do not modify files, create issues, or publish anything."
            }
        }
    }
}

pub fn can_apply_starter(revision: u64, name: &str, prompt: &str) -> bool {
    revision == 0 && name.trim().is_empty() && prompt.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::can_apply_starter;

    #[test]
    fn automation_starters_never_replace_existing_or_typed_tasks() {
        assert!(can_apply_starter(0, "", "  "));
        assert!(!can_apply_starter(1, "", ""));
        assert!(!can_apply_starter(0, "My task", ""));
        assert!(!can_apply_starter(0, "", "My prompt"));
    }
}

fn section(title: &'static str, description: &'static str, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .pt_2()
        .child(div().font_semibold().child(title))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
}

/// Controlled form values. Hosts retain input entities and supply their catalog.
pub struct AutomationForm<'a> {
    pub definition: &'a Definition,
    pub name: &'a Entity<InputState>,
    pub prompt: &'a Entity<TextareaState>,
    pub interval: &'a Entity<InputState>,
    pub time: &'a Entity<InputState>,
    pub timezone: &'a Entity<InputState>,
    pub cadence: &'a str,
    pub weekday: u32,
    pub projects: Vec<(String, String)>,
    pub models: Vec<(String, String)>,
    pub model_label: String,
    pub efforts: Vec<(String, String)>,
    pub show_effort: bool,
    pub is_git: bool,
    pub busy: bool,
    pub error: Option<String>,
    pub schedule_preview: String,
}

type FormCallback = Rc<dyn Fn(AutomationFormAction, &mut Window, &mut App)>;
fn form_request(
    callback: &FormCallback,
    action: AutomationFormAction,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let callback = callback.clone();
    move |_, window, cx| callback(action.clone(), window, cx)
}

/// Shared sheet chrome. The host supplies close/lifetime guards.
pub fn automation_editor_sheet<T: Render>(sheet: Sheet, editing: bool, editor: Entity<T>) -> Sheet {
    sheet
        .title(if editing {
            "Edit automation"
        } else {
            "New automation"
        })
        .size(rems(34.0))
        .max_w(relative(1.0))
        .child(editor)
}

/// Full form presentation with canonical input names, conditional schedule fields,
/// and scoped keyboard save. It requests changes; it never writes or schedules.
pub fn automation_form(
    form: AutomationForm<'_>,
    on_action: impl Fn(AutomationFormAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let on_action: FormCallback = Rc::new(on_action);
    let project_id = form.definition.project.to_string_lossy().into_owned();
    let project_label = form
        .projects
        .iter()
        .find(|(id, _)| *id == project_id)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| "Project unavailable".into());
    let callback = on_action.clone();
    let project = automation_picker(
        "automation-project",
        project_label,
        form.projects,
        project_id,
        form.busy,
        move |id, window, cx| callback(AutomationFormAction::Project(id), window, cx),
    );
    let callback = on_action.clone();
    let model = automation_picker(
        "automation-model",
        form.model_label,
        form.models,
        form.definition.model.clone(),
        form.busy,
        move |id, window, cx| callback(AutomationFormAction::Model(id), window, cx),
    );
    let callback = on_action.clone();
    let effort = automation_picker(
        "automation-effort",
        form.definition.effort.clone(),
        form.efforts,
        form.definition.effort.clone(),
        form.busy,
        move |id, window, cx| callback(AutomationFormAction::Effort(id), window, cx),
    );
    let mut cadences = vec![
        ("manual", "Manual"),
        ("interval", "Every N minutes"),
        ("daily", "Daily"),
        ("weekdays", "Weekdays"),
        ("weekly", "Weekly"),
    ];
    if matches!(&form.definition.schedule, Schedule::Calendar { days, .. } if calendar_cadence(days) == "custom")
    {
        cadences.push(("custom", "Custom days"));
    }
    let label = cadences
        .iter()
        .find(|(id, _)| *id == form.cadence)
        .map(|(_, label)| *label)
        .unwrap_or("Schedule unavailable")
        .to_string();
    let callback = on_action.clone();
    let cadence = automation_picker(
        "automation-cadence",
        label,
        cadences
            .into_iter()
            .map(|(id, label)| (id.into(), label.into()))
            .collect(),
        form.cadence.into(),
        form.busy,
        move |id, window, cx| callback(AutomationFormAction::Cadence(id), window, cx),
    );
    let mut body = div()
        .debug_selector(|| "automation-editor-form".into())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_4()
        .child(section("1. Define the task", "Give each run a clear goal and a useful result to report.", cx))
        .when(form.definition.revision == 0, |body| {
            let available = can_apply_starter(form.definition.revision, &form.name.read(cx).value(), &form.prompt.read(cx).value());
            body.child(div().flex().flex_col().gap_2()
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Start from a suggestion, or write your own. Suggestions only fill an empty task."))
                .child(div().flex().flex_wrap().gap_2().children([
                    AutomationStarter::ProjectReview, AutomationStarter::TestGaps, AutomationStarter::DependencyReview
                ].into_iter().enumerate().map(|(i, starter)| {
                    Button::new(("automation-starter", i)).small().label(starter.title())
                        .disabled(form.busy || !available)
                        .on_click(form_request(&on_action, AutomationFormAction::Starter(starter)))
                }))))
        })
        .child(crate::form_field(
            "Name",
            Input::new(form.name)
                .aria_label("Automation name")
                .disabled(form.busy),
        ))
        .child(crate::form_field(
            "Prompt",
            Textarea::new(form.prompt)
                .aria_label("Automation prompt")
                .disabled(form.busy),
        ))
        .child(section("2. Choose when to run", "Saving never starts a run immediately. Use Run now when you are ready.", cx))
        .child(crate::form_field("Schedule", cadence));
    if form.cadence == "interval" {
        body = body.child(crate::form_field(
            "Minutes between runs",
            Input::new(form.interval)
                .aria_label("Minutes between runs")
                .disabled(form.busy),
        ));
    }
    if form.cadence == "custom" {
        if let Schedule::Calendar { days, .. } = &form.definition.schedule {
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
    if ["daily", "weekdays", "weekly", "custom"].contains(&form.cadence) {
        body = body
            .child(crate::form_field(
                "Time (HH:MM)",
                Input::new(form.time)
                    .aria_label("Time in HH:MM")
                    .disabled(form.busy),
            ))
            .child(crate::form_field(
                "Timezone (IANA name)",
                Input::new(form.timezone)
                    .aria_label("IANA timezone")
                    .disabled(form.busy),
            ));
    }
    if form.cadence == "weekly" {
        let days = [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ];
        let owner = on_action.clone();
        body = body.child(crate::form_field(
            "Day",
            automation_picker(
                "automation-day",
                days.get(form.weekday as usize)
                    .copied()
                    .unwrap_or("Monday")
                    .into(),
                days.iter()
                    .enumerate()
                    .map(|(i, day)| (i.to_string(), day.to_string()))
                    .collect(),
                form.weekday.to_string(),
                form.busy,
                move |id, window, cx| {
                    owner(
                        AutomationFormAction::Weekday(id.parse().unwrap_or(0)),
                        window,
                        cx,
                    )
                },
            ),
        ));
    }
    let owner = on_action.clone();
    let mut environments = vec![("local".into(), "Local".into())];
    if form.is_git {
        environments.push(("worktree".into(), "Worktree".into()));
    }
    body = body
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child(form.schedule_preview.clone()))
            .when(!form.definition.enabled, |body| body.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("This automation will stay paused after saving. Resume it from its details to enable scheduled runs.")))
            .child(section("3. Configure execution", "Each run opens a fresh chat with these settings.", cx))
            .child(crate::form_field("Project", project))
            .child(crate::form_field("Model", model))
            .when(form.show_effort, |body| body.child(crate::form_field("Reasoning effort", effort)))
            .child(crate::form_field("Run in", automation_picker(
                "automation-environment",
                if form.definition.worktree { "Worktree" } else { "Local" }.into(),
                environments,
                if form.definition.worktree { "worktree" } else { "local" }.into(),
                form.busy,
                move |id, window, cx| owner(AutomationFormAction::Worktree(id == "worktree"), window, cx),
            )))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(
                if form.definition.worktree {
                    "For code changes. Each run gets a fresh Git worktree, separate from your project checkout."
                } else {
                    "For research and issue creation. Uses your project checkout without creating a worktree. This is not read-only; the prompt should say when files must not change."
                }))
            .when(!form.is_git, |body| body.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Worktrees require a Git repository.")))
            .child(Checkbox::new("automation-notify").label("Notify on every completion").checked(form.definition.notify_all).disabled(form.busy)
                .on_click({ let callback = on_action.clone(); move |checked, window, cx| callback(AutomationFormAction::NotifyAll(*checked), window, cx) }))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Runs while Threadlane is open and your computer is awake. Permission and question requests wait for you in the run’s chat. Saving does not run the prompt immediately."))
            .children(form.error.clone().map(|error| div().id("automation-editor-error").debug_selector(|| "automation-editor-error".into()).role(Role::Alert).aria_label(error.clone()).text_color(cx.theme().danger).child(error)))
            .child(div().flex().justify_end().gap_2()
                .child(Button::new("automation-editor-cancel").label("Cancel").disabled(form.busy).on_click(form_request(&on_action, AutomationFormAction::Cancel)))
                .child(Button::new("automation-editor-save").debug_selector(|| "automation-editor-save".into()).primary().label(if form.busy { "Saving…" } else { "Save" }).disabled(form.busy)
                    .on_click(form_request(&on_action, AutomationFormAction::Save))));
    body.key_context("AutomationEditor")
        .on_action(move |_: &SaveAutomation, window, cx| {
            on_action(AutomationFormAction::Save, window, cx)
        })
}

/// Classify stored days without changing their order or membership.
pub fn calendar_cadence(days: &[u32]) -> &'static str {
    match days {
        [0, 1, 2, 3, 4, 5, 6] => "daily",
        [0, 1, 2, 3, 4] => "weekdays",
        [_] => "weekly",
        _ => "custom",
    }
}

/// Resolve the selected cadence while preserving custom stored days exactly.
pub fn calendar_days(cadence: &str, weekday: u32, original: &Schedule) -> Vec<u32> {
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

/// Convert edited fields without normalizing a stored custom day set.
pub fn schedule_from_fields(
    cadence: &str,
    interval: &str,
    time: &str,
    timezone: &str,
    weekday: u32,
    original: &Schedule,
) -> Result<Schedule, String> {
    match cadence {
        "manual" => Ok(Schedule::Manual),
        "interval" => Ok(Schedule::Interval {
            minutes: interval
                .trim()
                .parse()
                .map_err(|_| "Enter an interval in whole minutes")?,
        }),
        _ => {
            let (hour, minute) = time.trim().split_once(':').ok_or("Enter a time as HH:MM")?;
            Ok(Schedule::Calendar {
                hour: hour.parse().map_err(|_| "Invalid hour")?,
                minute: minute.parse().map_err(|_| "Invalid minute")?,
                days: calendar_days(cadence, weekday, original),
                timezone: timezone.trim().into(),
            })
        }
    }
}

/// Preview three occurrences at one captured clock instant.
pub fn automation_schedule_preview(schedule: Result<Schedule, String>, at: i64) -> String {
    let result = schedule.and_then(|s| {
        let mut after = at;
        let mut times = Vec::new();
        for _ in 0..3 {
            if let Some(next) = s.next(after, at)? {
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
    result.unwrap_or_else(|error| error)
}

/// Initial form fields preserve the original cadence and custom weekday order.
pub fn automation_schedule_fields(
    schedule: &Schedule,
) -> (&'static str, String, String, String, u32) {
    match schedule {
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
    }
}
