//! Controlled automation presentation. Hosts own navigation and every service command.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::collapsible::Collapsible;
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable, StyledExt};
use std::{path::PathBuf, rc::Rc};
use threadlane_automation::{Definition, Run, RunStatus, Snapshot, display_time};

/// Intent from an automation control. The host validates and applies it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutomationAction {
    Scope(Option<PathBuf>),
    Select(String),
    Back,
    History(bool),
    AttentionOnly(bool),
    ExpandPrompt(Option<String>),
    Page(usize),
    Edit(Option<String>),
    Duplicate(String),
    ClearSearch,
    RunNow(String),
    SetEnabled(String, bool),
    Delete(String),
    OpenChat(String),
    CancelRun(String),
    ReviewRun(String),
    DeleteRun(String),
}

/// Host-controlled navigation and availability; no services or hidden state.
#[derive(Clone, Debug, Default)]
pub struct AutomationScreen {
    pub search: Option<Entity<InputState>>,
    pub query: String,
    pub compact: bool,
    pub projects: Vec<(String, String)>,
    pub selected: Option<String>,
    pub expanded_prompt: Option<String>,
    pub scope: Option<PathBuf>,
    pub history: bool,
    pub attention_only: bool,
    pub page: usize,
    pub error: Option<String>,
    pub busy: bool,
    /// Leading header inset that clears the window controls when the sidebar
    /// is hidden; `None` keeps the default padding.
    pub header_inset: Option<Pixels>,
}

type Callback = Rc<dyn Fn(AutomationAction, &mut Window, &mut App)>;
fn request(
    on_action: &Callback,
    action: AutomationAction,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let on_action = on_action.clone();
    move |_, window, cx| on_action(action.clone(), window, cx)
}

pub use crate::picker::choice_picker as automation_picker;

#[cfg(test)]
mod tests {
    use super::{duplicate_automation, matches_automation_query};
    use threadlane_automation::{Definition, Schedule};

    fn definition() -> Definition {
        Definition {
            id: "original".into(),
            revision: 7,
            name: "Review changes".into(),
            prompt: "Inspect the project".into(),
            project: "/Projects/Threadlane".into(),
            model: "test-model".into(),
            effort: "high".into(),
            worktree: true,
            schedule: Schedule::Interval { minutes: 60 },
            enabled: true,
            notify_all: true,
            anchor: 100,
            next_at: Some(200),
            failures: 3,
            paused_reason: Some("failed".into()),
        }
    }

    #[test]
    fn automation_search_matches_name_and_project_without_case_or_outer_whitespace() {
        let d = definition();
        for query in ["", "  ", " REVIEW ", "threadLANE", "/projects/"] {
            assert!(matches_automation_query(&d, query), "{query}");
        }
        assert!(!matches_automation_query(&d, "not present"));
    }

    #[test]
    fn automation_duplicate_is_a_paused_draft_without_execution_bookkeeping() {
        let source = definition();
        let copy = duplicate_automation(&source, "copy-id".into(), 300);
        assert_eq!(copy.id, "copy-id");
        assert_eq!(copy.name, "Review changes (copy)");
        assert_eq!(copy.revision, 0);
        assert!(!copy.enabled);
        assert_eq!(copy.anchor, 300);
        assert_eq!(copy.next_at, None);
        assert_eq!(copy.failures, 0);
        assert_eq!(copy.paused_reason, None);
        assert_eq!(copy.prompt, source.prompt);
        assert_eq!(copy.project, source.project);
        assert_eq!(copy.model, source.model);
        assert_eq!(copy.effort, source.effort);
        assert_eq!(copy.schedule, source.schedule);
        assert_eq!(copy.worktree, source.worktree);
        assert_eq!(copy.notify_all, source.notify_all);
        assert!(source.enabled);
        assert_eq!(source.revision, 7);
    }
}

pub fn matches_automation_query(definition: &Definition, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || definition.name.to_lowercase().contains(&query)
        || definition
            .project
            .to_string_lossy()
            .to_lowercase()
            .contains(&query)
}

/// A copy is an unsaved, paused draft; it cannot inherit execution bookkeeping.
pub fn duplicate_automation(source: &Definition, id: String, at: i64) -> Definition {
    Definition {
        id,
        revision: 0,
        name: format!("{} (copy)", source.name),
        enabled: false,
        anchor: at,
        next_at: None,
        failures: 0,
        paused_reason: None,
        ..source.clone()
    }
}

/// Render the same list, detail, and paged history on desktop and web.
/// Requests are synchronous; the host updates its controlled state and notifies.
pub fn automation_screen(
    snapshot: &Snapshot,
    view: &AutomationScreen,
    on_action: impl Fn(AutomationAction, &mut Window, &mut App) + 'static,
    cx: &mut App,
) -> Div {
    let on_action: Callback = Rc::new(on_action);
    let empty_projects = view.projects.is_empty();
    let selected = view.selected.clone();
    let scope = view.scope.clone();
    let mut choices = vec![(String::new(), "All projects".into())];
    choices.extend(view.projects.clone());
    let scope_id = scope
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let scope_label = choices
        .iter()
        .find(|(id, _)| *id == scope_id)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| "Project unavailable".into());
    let owner = on_action.clone();
    let scope_picker = automation_picker(
        "automation-project-scope",
        scope_label,
        choices,
        scope_id,
        false,
        move |id, window, cx| {
            owner(
                AutomationAction::Scope((!id.is_empty()).then(|| PathBuf::from(id))),
                window,
                cx,
            )
        },
    );
    let definition = snapshot
        .definitions
        .iter()
        .find(|d| Some(&d.id) == selected.as_ref())
        .filter(|d| scope.as_ref().is_none_or(|p| *p == d.project))
        .cloned();
    let muted = cx.theme().muted_foreground;
    let show_list = !view.history && (!view.compact || definition.is_none());
    let definitions: Vec<_> = snapshot
        .definitions
        .iter()
        .filter(|d| scope.as_ref().is_none_or(|p| *p == d.project))
        .filter(|d| matches_automation_query(d, &view.query))
        .collect();
    let mut list = div().flex().flex_col().gap_2().p_4();
    if let Some(search) = &view.search {
        list = list.child(Input::new(search).aria_label("Search automations by name or project"));
    }
    list = list.child(
        div()
            .text_sm()
            .text_color(muted)
            .child(format!("Automations · {}", definitions.len())),
    );
    if definitions.is_empty() {
        list = list.child(
            div()
                .debug_selector(|| "automation-empty".into())
                .text_sm()
                .text_color(muted)
                .child(if !view.query.trim().is_empty() {
                    "No matching automations. Clear the search or choose another project."
                } else if empty_projects {
                    "Attach a project from the sidebar to create an automation."
                } else {
                    "No automations yet. Create one to run a saved prompt on a schedule."
                }),
        );
    }
    if !view.query.is_empty() {
        list = list.child(
            Button::new("automation-clear-search")
                .debug_selector(|| "automation-clear-search".into())
                .ghost()
                .label("Clear search")
                .on_click(request(&on_action, AutomationAction::ClearSearch)),
        );
    }
    for d in definitions {
        let latest = snapshot.runs.iter().rev().find(|r| r.definition.id == d.id);
        let callback = on_action.clone();
        let id = d.id.clone();
        list = list.child(
            automation_definition_row(
                d,
                latest,
                move |window, cx| callback(AutomationAction::Select(id.clone()), window, cx),
                cx,
            )
            .selected(selected.as_ref() == Some(&d.id)),
        );
    }
    let mut content = div().flex().flex_col().w_full().min_w_0().gap_4().p_4();
    if let Some(d) = definition {
        let active = snapshot
            .runs
            .iter()
            .any(|r| r.definition.id == d.id && r.status.active());
        let callback = on_action.clone();
        content = content.child(automation_detail(
            &d,
            active,
            view.busy,
            view.expanded_prompt.as_ref() == Some(&d.id),
            move |action, window, cx| callback(action, window, cx),
            cx,
        ));
    } else if !view.history {
        content = content.child(div().text_lg().font_semibold().child("Your recurring work, in one place"))
            .child(div().text_sm().text_color(muted).child("Select an automation to inspect its schedule and runs, or create one from a starter prompt."));
    }
    if view.history || selected.is_some() {
        let mut runs: Vec<_> = snapshot
            .runs
            .iter()
            .rev()
            .filter(|r| {
                scope.as_ref().is_none_or(|p| *p == r.definition.project)
                    && selected.as_ref().is_none_or(|id| *id == r.definition.id)
            })
            .collect();
        let total_runs = runs.len();
        let attention_count = runs.iter().filter(|run| run.needs_attention()).count();
        if view.attention_only {
            runs.retain(|run| run.needs_attention());
        }
        content = content.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("automation-history-heading")
                        .role(Role::Heading)
                        .aria_label(format!("Run history, {}", runs.len()))
                        .font_semibold()
                        .child(format!("Run history · {}", runs.len())),
                )
                .child(div().flex_1())
                .child(
                    Button::new("automation-all-runs")
                        .debug_selector(|| "automation-all-runs".into())
                        .small()
                        .ghost()
                        .label(format!("All runs · {total_runs}"))
                        .selected(!view.attention_only)
                        .accessibility_label(format!(
                            "All runs, {total_runs} runs, {}",
                            if view.attention_only {
                                "filter inactive"
                            } else {
                                "filter active"
                            }
                        ))
                        .on_click(request(&on_action, AutomationAction::AttentionOnly(false))),
                )
                .child(
                    Button::new("automation-attention")
                        .debug_selector(|| "automation-attention".into())
                        .small()
                        .ghost()
                        .label(format!("Needs attention · {attention_count}"))
                        .selected(view.attention_only)
                        .accessibility_label(format!(
                            "Needs attention, {attention_count} runs, {}",
                            if view.attention_only {
                                "filter active"
                            } else {
                                "filter inactive"
                            }
                        ))
                        .tooltip("Show runs awaiting review or input")
                        .on_click(request(&on_action, AutomationAction::AttentionOnly(true))),
                ),
        );
        let page = view.page.min(runs.len().saturating_sub(1) / 25);
        if runs.is_empty() {
            // Name the way out: an empty history is only useful if it
            // says how a run gets created.
            let hint = if view.attention_only {
                "No runs need attention in this scope. Choose All runs to see the full history."
            } else if selected.is_some() {
                "No runs yet. Choose Run now above, or wait for the next scheduled run."
            } else {
                "No runs yet. Runs appear here when an automation fires or you choose Run now."
            };
            content = content.child(div().text_color(muted).child(hint));
        }
        for run in runs.iter().skip(page * 25).take(25) {
            let callback = on_action.clone();
            content = content.child(automation_run_row(
                run,
                view.busy,
                move |action, window, cx| callback(action, window, cx),
                cx,
            ));
        }
        if runs.len() > 25 {
            content = content.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("automation-prev")
                            .debug_selector(|| "automation-prev".into())
                            .label("Previous")
                            .disabled(page == 0)
                            .on_click(request(
                                &on_action,
                                AutomationAction::Page(page.saturating_sub(1)),
                            )),
                    )
                    .child(div().text_sm().text_color(muted).child(format!(
                        "Page {} of {}",
                        page + 1,
                        runs.len().div_ceil(25)
                    )))
                    .child(
                        Button::new("automation-next")
                            .debug_selector(|| "automation-next".into())
                            .label("Next")
                            .disabled((page + 1) * 25 >= runs.len())
                            .on_click(request(&on_action, AutomationAction::Page(page + 1))),
                    ),
            );
        }
    }
    let attention_count = snapshot
        .runs
        .iter()
        .filter(|r| scope.as_ref().is_none_or(|p| *p == r.definition.project))
        .filter(|r| r.needs_attention())
        .count();
    let attention_owner = on_action.clone();
    div().flex().flex_col().size_full().min_w_0().bg(cx.theme().background)
            // Shared header row (as Chat and GitHub): title in the window-controls band.
            // It wraps onto a second line at narrow widths rather than overflowing.
            .child(div().flex_none().flex().flex_wrap().items_center().gap_3().px_4().py_1().when_some(view.header_inset, |row, inset| row.pl(inset))
                .min_h(threadlane_ui_theme::theme::WINDOW_CONTROLS_CLEARANCE).border_b_1().border_color(cx.theme().title_bar_border).bg(cx.theme().title_bar)
                .child(div().id("automation-heading").role(Role::Heading).aria_label("Automations").text_sm().font_semibold().child("Automations"))
                .child(scope_picker).child(div().flex_1())
                .child(Button::new("automation-inbox").debug_selector(|| "automation-inbox".into()).ghost().label(format!("Needs attention · {attention_count}"))
                    .on_click(move |_, window, cx| {
                        attention_owner(AutomationAction::History(true), window, cx);
                        attention_owner(AutomationAction::AttentionOnly(true), window, cx);
                    }))
                .child(Button::new("automation-history").debug_selector(|| "automation-history".into()).label(if view.history { "All automations" } else { "Run history" }).ghost().selected(view.history)
                    .on_click(request(&on_action, AutomationAction::History(!view.history))))
                .child(Button::new("new-automation").debug_selector(|| "new-automation".into()).primary().label("New automation…").disabled(empty_projects || view.busy)
                    .tooltip(if empty_projects { "Attach a project from the sidebar to create an automation" } else { "Create an automation" })
                    .accessibility_label(if empty_projects { "New automation, attach a project from the sidebar first" } else { "New automation" })
                    .on_click(request(&on_action, AutomationAction::Edit(None)))))
            .child(div().px_4().pt_3().pb_3().text_sm().text_color(muted).child("Runs while Threadlane is open and your computer is awake. Missed runs are combined into one."))
            .children(view.error.clone().map(|error| div().id("automation-error").px_4().pb_3().role(Role::Alert).text_color(cx.theme().danger).child(error)))
            .child(div().flex().items_stretch().flex_1().min_h_0().min_w_0()
                .when(show_list, |body| body.child(div().id("automation-list").debug_selector(|| "automation-list".into())
                    .when(!view.compact, |list| list.w_80().flex_shrink_0().border_r_1().border_color(cx.theme().border))
                    .when(view.compact, |list| list.flex_1().min_w_0())
                    .min_h_0().overflow_y_scrollbar().child(list)))
                .when(!view.compact || !show_list, |body| body.child(div().id("automation-detail-scroll")
                    .debug_selector(|| "automation-content".into()).flex_1().min_w_0().min_h_0().overflow_y_scrollbar()
                    .child(content))))
}

/// An automation identity, schedule, and last outcome. Selection stays in the host.
pub fn automation_definition_row(
    d: &Definition,
    latest: Option<&Run>,
    on_select: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> Button {
    let muted = cx.theme().muted_foreground;
    let next_run = if !d.enabled {
        "Paused".into()
    } else if matches!(d.schedule, threadlane_automation::Schedule::Manual) {
        "Runs manually".into()
    } else {
        d.next_at
            .map(|t| format!("Next run: {}", display_time(t, d.schedule.timezone())))
            .unwrap_or_else(|| "Next run unavailable".into())
    };
    let last_run = format!(
        "Last run: {}",
        latest.map(|r| r.status.label()).unwrap_or("none yet")
    );
    Button::new(SharedString::from(format!("automation-{}", d.id)))
        .debug_selector({
            let id = d.id.clone();
            move || format!("automation-row-{id}")
        })
        .accessibility_label(format!(
            "Open automation {}. {}. {}. {}",
            d.name,
            d.schedule.label(),
            next_run,
            last_run
        ))
        .ghost()
        .w_full()
        .h_auto()
        .py_3()
        .justify_start()
        .child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_3()
                .w_full()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .flex_basis(relative(1.0))
                        .min_w_0()
                        .items_start()
                        .gap_1()
                        .child(div().font_semibold().child(d.name.clone()))
                        .child(div().text_sm().text_color(muted).child(format!(
                            "{} · {} · {}",
                            d.project.file_name().unwrap_or_default().to_string_lossy(),
                            d.schedule.label(),
                            if d.worktree { "Worktree" } else { "Local" }
                        ))),
                )
                .child(div().text_sm().text_color(muted).child(next_run))
                .child(div().text_sm().child(last_run)),
        )
        .on_click(move |_, window, cx| on_select(window, cx))
}

/// A run's status and available actions, using canonical active/attention rules.
/// Remove only requests history removal; the host retains chats and worktrees.
pub fn automation_run_row(
    run: &Run,
    busy: bool,
    on_action: impl Fn(AutomationAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let on_action: Callback = Rc::new(on_action);
    let muted = cx.theme().muted_foreground;
    let id = run.id.clone();
    let cancel_id = id.clone();
    let review_id = id.clone();
    let delete_id = id.clone();
    let run_label = format!(
        "{} · {}",
        run.definition.name,
        display_time(run.created_at, run.definition.schedule.timezone())
    );
    let open_label = match run.status {
        RunStatus::WaitingPermission => "Review request",
        RunStatus::WaitingAnswer => "Answer question",
        RunStatus::Failed | RunStatus::Interrupted => "Inspect run",
        RunStatus::Succeeded if !run.reviewed => "Review result",
        _ => "Open chat",
    };
    let mut row = div()
                    .id(SharedString::from(format!("automation-run-{id}")))
                    .debug_selector({ let id = id.clone(); move || format!("automation-run-{id}") })
                    .role(Role::Group)
                    .aria_label(format!("{run_label}, {}", run.status.label()))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .flex_wrap()
                            .child(div().w_full().flex_none().min_w_0()
                                .debug_selector({ let id = id.clone(); move || format!("automation-run-title-{id}") })
                                .child(run_label.clone()))
                            .child(run.status.label())
                            .child(div().flex_1())
                            .child(
                                Button::new(SharedString::from(format!("open-{id}")))
                                    .small()
                                    .label(open_label)
                                    .accessibility_label(if run.session_file.is_some() {
                                        format!("{open_label} for {run_label}")
                                    } else {
                                        format!("{open_label} for {run_label}, unavailable because this run has no chat yet")
                                    })
                                    .disabled(run.session_file.is_none())
                                    .on_click(request(&on_action, AutomationAction::OpenChat(id))),
                            )
                            .when(run.status.active(), |row| {
                                row.child(
                                    Button::new(SharedString::from(format!("cancel-{cancel_id}")))
                                        .debug_selector({ let id = cancel_id.clone(); move || format!("cancel-run-{id}") })
                                        .small()
                                        .label("Cancel run")
                                        .accessibility_label(format!("Cancel run for {run_label}"))
                                        .disabled(busy)
                                        .on_click(request(&on_action, AutomationAction::CancelRun(cancel_id))),
                                )
                            })
                            .when(run.needs_attention() && !run.status.active(), |row| {
                                row.child(
                                    Button::new(SharedString::from(format!("review-{review_id}")))
                                        .debug_selector({ let id = review_id.clone(); move || format!("review-run-{id}") })
                                        .small()
                                        .label("Mark reviewed")
                                        .accessibility_label(format!("Mark reviewed: {run_label}"))
                                        .disabled(busy)
                                        .on_click(request(&on_action, AutomationAction::ReviewRun(review_id))),
                                )
                            })
                            .when(!run.status.active(), |row| {
                                row.child(
                                    Button::new(SharedString::from(format!("remove-{delete_id}")))
                                        .debug_selector(move || format!("remove-run-{delete_id}"))
                                        .small()
                                        .ghost()
                                        .label("Remove")
                                        .tooltip(
                                            "Remove from run history; keep the chat and worktree",
                                        )
                                        .accessibility_label(format!(
                                            "Remove {run_label} from run history; keep its chat and worktree"
                                        ))
                                        .disabled(busy)
                                        .on_click({
                                            let id = run.id.clone();
                                            request(&on_action, AutomationAction::DeleteRun(id))
                                        }),
                                )
                            }),
                    )
                    .child(div().debug_selector({ let id = run.id.clone(); move || format!("automation-run-context-{id}") }).text_xs().text_color(muted).child(format!("{} · {}",
                        run.definition.project.file_name().unwrap_or_default().to_string_lossy(),
                        if run.definition.worktree { "Worktree" } else { "Local" })));
    if let Some(error) = &run.error {
        row = row.child(
            div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error.clone()),
        );
    }
    if run.needs_attention() && !run.status.active() {
        row = row.child(
            div()
                .text_sm()
                .text_color(muted)
                .child("Awaiting your review"),
        );
    }
    if run.status == RunStatus::Queued {
        row = row.child(
            div()
                .text_sm()
                .text_color(muted)
                .child("Waiting for the current automation run to finish or be cancelled"),
        );
    }
    row
}

/// Selected automation metadata, command toolbar, and controlled prompt disclosure.
pub fn automation_detail(
    d: &Definition,
    active: bool,
    busy: bool,
    prompt_open: bool,
    on_action: impl Fn(AutomationAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let on_action: Callback = Rc::new(on_action);
    let muted = cx.theme().muted_foreground;
    let content = div().flex().flex_col().w_full().min_w_0().gap_4();
    let run_id = d.id.clone();
    let pause_id = d.id.clone();
    let delete_id = d.id.clone();
    let edit = d.id.clone();
    let owner = on_action.clone();
    let enabled = d.enabled;
    let prompt_id = d.id.clone();
    let content = content.child(
        div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_2()
            .child(
                Button::new("automation-back")
                    .ghost()
                    .label("All automations")
                    .on_click(request(&on_action, AutomationAction::Back)),
            )
            .child(div().flex_1())
            .child(
                Button::new("automation-run")
                    .primary()
                    .label("Run now")
                    .tooltip(
                        "Start a fresh chat with the saved settings; leave the schedule unchanged",
                    )
                    .accessibility_label(
                        "Run now: start a fresh chat with the saved settings; leave the schedule unchanged",
                    )
                    .disabled(busy || active)
                    .on_click(request(&on_action, AutomationAction::RunNow(run_id))),
            )
            .child(
                Button::new("automation-pause")
                    .label(if enabled { "Pause" } else { "Resume" })
                    .disabled(busy)
                    .on_click(request(
                        &on_action,
                        AutomationAction::SetEnabled(pause_id, !enabled),
                    )),
            )
            .child(
                Button::new("automation-edit")
                    .label("Edit…")
                    .disabled(busy)
                    .on_click(request(&on_action, AutomationAction::Edit(Some(edit)))),
            )
            .child(
                Button::new("automation-duplicate")
                    .label("Duplicate…")
                    .disabled(busy)
                    .debug_selector(|| "automation-duplicate".into())
                    .tooltip("Create a paused copy to edit before scheduling")
                    .accessibility_label(
                        "Duplicate: create a paused copy to edit before scheduling",
                    )
                    .on_click(request(
                        &on_action,
                        AutomationAction::Duplicate(d.id.clone()),
                    )),
            )
            .child(
                Button::new("automation-more")
                    .label("More")
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        let owner = owner.clone();
                        let id = delete_id.clone();
                        menu.item(
                            PopupMenuItem::new("Delete automation")
                                .disabled(active)
                                .on_click(move |_, window, cx| {
                                    owner(AutomationAction::Delete(id.clone()), window, cx);
                                }),
                        )
                    }),
            ),
    );
    content
        .child(
            div()
                .id("automation-detail-heading")
                .debug_selector(|| "automation-detail-heading".into())
                .w_full()
                .min_w_0()
                .role(Role::Heading)
                .aria_label(d.name.clone())
                .text_xl()
                .font_semibold()
                .child(d.name.clone()),
        )
        .child(div().text_color(muted).child(format!(
            "{} · {}",
            if !d.enabled {
                "Paused"
            } else if matches!(d.schedule, threadlane_automation::Schedule::Manual) {
                "Manual"
            } else {
                "Scheduled"
            },
            d.schedule.label()
        )))
        .child(div().text_color(muted).child(format!(
            "{} · {} · {}",
            d.project.display(),
            d.model,
            if d.worktree { "Worktree" } else { "Local" }
        )))
        .child(
            Collapsible::new()
                .open(prompt_open)
                .child(
                    Button::new("automation-prompt-toggle")
                        .debug_selector(|| "automation-prompt-toggle".into())
                        .ghost()
                        .small()
                        .label(if prompt_open {
                            "Hide prompt"
                        } else {
                            "Show prompt"
                        })
                        .accessibility_label(if prompt_open {
                            "Hide automation prompt"
                        } else {
                            "Show automation prompt"
                        })
                        .on_click(request(
                            &on_action,
                            AutomationAction::ExpandPrompt((!prompt_open).then_some(prompt_id)),
                        )),
                )
                .content(
                    div()
                        .debug_selector(|| "automation-prompt-body".into())
                        .text_sm()
                        .p_3()
                        .bg(cx.theme().muted)
                        .rounded_md()
                        .child(d.prompt.clone()),
                ),
        )
        .children(
            d.paused_reason
                .clone()
                .map(|reason| div().text_color(cx.theme().warning).child(reason)),
        )
        .children(d.next_at.map(|at| {
            div().child(format!(
                "Next run: {}",
                display_time(at, d.schedule.timezone())
            ))
        }))
}
