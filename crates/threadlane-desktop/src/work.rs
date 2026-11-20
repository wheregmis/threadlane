//! Focused views reached from a project, a routine, or the space sidebar.
use crate::{
    Desktop, Screen, Tab,
    components::{button, field, meta, page_header, section},
    describe,
    icons::AppIcon,
    local_zone,
    tokens::{LG, MD, READING_WIDTH, SIDEBAR_WIDTH, SM},
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashSet;
use std::sync::Arc;
use threadlane_client::ClientCore;
use threadlane_core::{
    client::{OverviewReply, OverviewRequest, TaskReply, TaskRequest},
    ids::{EventId, TaskId, TimerId},
    model::{Mode, Recurrence, Rule, Weekday},
    work::{Occurrence, OutputSource, ReviewRecipe, UsageGroup},
};
use threadlane_desktop::{Activity, When, count_form, format_count};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkPage {
    History {
        timer: TimerId,
        before: Option<EventId>,
    },
    Agenda,
    Tomorrow(String),
    Usage(u32),
    Outputs {
        project: String,
        search: String,
    },
    Image {
        project: String,
        event: EventId,
        index: u32,
    },
    Workflow,
}

impl WorkPage {
    fn query(&self) -> OverviewRequest {
        match self {
            Self::History { timer, before } => OverviewRequest::RoutineHistory {
                timer: *timer,
                before: *before,
            },
            Self::Agenda => OverviewRequest::Agenda { tomorrow_in: None },
            Self::Tomorrow(zone) => OverviewRequest::Agenda {
                tomorrow_in: Some(zone.clone()),
            },
            Self::Usage(days) => OverviewRequest::SpaceUsage { days: *days },
            Self::Outputs { project, search } => OverviewRequest::Outputs {
                project: project.clone(),
                search: search.clone(),
            },
            Self::Image {
                project,
                event,
                index,
            } => OverviewRequest::OutputImage {
                project: project.clone(),
                event: *event,
                index: *index,
            },
            Self::Workflow => OverviewRequest::ReviewRecipe,
        }
    }
    fn title(&self) -> &'static str {
        match self {
            Self::History { .. } => "Run history",
            Self::Agenda | Self::Tomorrow(_) => "Routine agenda",
            Self::Usage(_) => "Space usage",
            Self::Outputs { .. } => "Project outputs",
            Self::Image { .. } => "Attachment",
            Self::Workflow => "Review workflow",
        }
    }
}

pub(super) struct WorkView {
    data: Option<Result<OverviewReply, String>>,
    loading: bool,
    generation: u64,
    saving: bool,
    loaded_recipe: Option<i64>,
    loaded_search: bool,
    expanded_usage: HashSet<(String, String, String)>,
    retry_workflow: Option<(TaskRequest, String)>,
    implementation: Entity<TextareaState>,
    review: Entity<TextareaState>,
    engineer: String,
    reviewer: String,
    goal: Entity<InputState>,
    repository: String,
    search: Entity<InputState>,
    zone: String,
    manual: std::collections::HashMap<(String, TimerId), (String, bool)>,
}

impl WorkView {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let mut input =
            |value: &str| cx.new(|cx| InputState::new(window, cx).default_value(value.to_owned()));
        let goal = input("");
        let search = input("");
        let implementation = cx.new(|cx| TextareaState::new(window, cx).rows(3));
        let review = cx.new(|cx| TextareaState::new(window, cx).rows(3));
        cx.observe(&goal, |_, _, cx| cx.notify()).detach();
        cx.observe(&implementation, |_, _, cx| cx.notify()).detach();
        cx.observe(&review, |_, _, cx| cx.notify()).detach();
        Self {
            data: None,
            loading: false,
            generation: 0,
            saving: false,
            manual: Default::default(),
            loaded_recipe: None,
            loaded_search: false,
            expanded_usage: HashSet::new(),
            retry_workflow: None,
            engineer: "Engineer".into(),
            reviewer: "Reviewer".into(),
            goal,
            repository: String::new(),
            search,
            implementation,
            review,
            zone: local_zone().unwrap_or_else(|| "UTC".into()),
        }
    }
    pub(super) fn leave(&mut self) {
        self.generation += 1;
        self.data = None;
        self.loading = false;
        self.loaded_recipe = None;
        self.loaded_search = false;
        self.expanded_usage.clear();
        self.retry_workflow = None;
        self.saving = false;
    }
    fn moment(&self, at: threadlane_core::time::Timestamp, when: &When) -> String {
        jiff::Timestamp::from_millisecond(at.as_millis())
            .ok()
            .and_then(|ts| ts.in_tz(&self.zone).ok())
            .map_or_else(
                || when.moment(at),
                |time| time.strftime("%a %b %d %H:%M %Z").to_string(),
            )
    }
}

#[derive(Clone, Copy)]
enum WorkflowChoice {
    Repository,
    Engineer,
    Reviewer,
}

impl WorkflowChoice {
    fn label(self) -> &'static str {
        match self {
            Self::Repository => "Repository",
            Self::Engineer => "Engineer",
            Self::Reviewer => "Reviewer",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Repository => "workflow-repository",
            Self::Engineer => "workflow-engineer",
            Self::Reviewer => "workflow-reviewer",
        }
    }

    fn field(self, work: &mut WorkView) -> &mut String {
        match self {
            Self::Repository => &mut work.repository,
            Self::Engineer => &mut work.engineer,
            Self::Reviewer => &mut work.reviewer,
        }
    }
}

/// Pair fields only when both can usefully share the actual form width.
fn workflow_pair(first: Div, second: Div) -> Div {
    let column = |field: Div| field.flex_1().min_w_0().flex_basis(rems(18.));
    h_flex()
        .w_full()
        .min_w_0()
        .flex_wrap()
        .items_start()
        .gap(MD)
        .child(column(first))
        .child(column(second))
}

fn workflow_start_blocker(
    dirty: bool,
    project_off: bool,
    roles_ready: bool,
    repository_ready: bool,
    goal: &str,
) -> Option<&'static str> {
    if dirty {
        Some("Save the edited steps before starting.")
    } else if project_off {
        Some("Turn on this project before starting.")
    } else if !roles_ready {
        Some("Choose two enabled specialists on the project team, or install a review team.")
    } else if !repository_ready {
        Some("Choose a repository registered in this project.")
    } else if goal.trim().is_empty() {
        Some("Add a goal to start the workflow.")
    } else {
        None
    }
}

struct ViewWorkflowRuns<'a> {
    implementations: Vec<&'a threadlane_core::client::TaskSummary>,
    reviews: std::collections::HashMap<TaskId, &'a threadlane_core::client::TaskSummary>,
}

impl<'a> ViewWorkflowRuns<'a> {
    fn from_tasks(tasks: &'a [threadlane_core::client::TaskSummary]) -> Self {
        let mut runs = Self {
            implementations: Vec::new(),
            reviews: Default::default(),
        };
        for row in tasks {
            let Some(call) = row.task.created_by_call.as_deref() else {
                continue;
            };
            if call.starts_with("workflow-implement:") {
                runs.implementations.push(row);
            } else if call.starts_with("workflow-review:")
                && let [first] = row.task.depends_on.as_slice()
            {
                runs.reviews.insert(*first, row);
            }
        }
        runs
    }
}

fn reported_cost(cost: f64, reports: u128) -> String {
    if reports == 0 {
        "—".to_owned()
    } else {
        format!("USD {cost:.4}")
    }
}

fn usage_metrics(group: Option<&UsageGroup>, compact: bool, cx: &App) -> Div {
    let count = |n| format_count(u128::from(n));
    let cells = [
        ("Runs", 4., group.map(|g| (count(g.runs), None))),
        (
            "Run time",
            5.,
            group.map(|g| (format!("{} min", count(g.duration_ms / 60_000)), None)),
        ),
        (
            "Reported tokens",
            8.,
            group.map(|g| {
                (
                    count(g.tokens),
                    Some(format!(
                        "{} complete {}",
                        count(g.token_reports),
                        count_form(g.token_reports, "report", "reports")
                    )),
                )
            }),
        ),
        (
            "Reported cost",
            8.,
            group.map(|g| {
                (
                    reported_cost(g.cost, u128::from(g.cost_reports)),
                    Some(format!(
                        "{} cost {}",
                        count(g.cost_reports),
                        count_form(g.cost_reports, "report", "reports")
                    )),
                )
            }),
        ),
    ];
    h_flex()
        .items_start()
        .flex_wrap()
        .gap_x_3()
        .gap_y_2()
        .children(cells.map(|(label, width, value)| {
            v_flex()
                .w(rems(width))
                .max_w_full()
                .gap_1()
                .when(!compact, |cell| cell.text_right())
                .children((compact || group.is_none()).then(|| meta(label, cx)))
                .children(value.map(|(value, detail)| {
                    v_flex()
                        .gap_1()
                        .child(div().child(value))
                        .children(detail.map(|detail| meta(detail, cx)))
                }))
        }))
}

impl Desktop {
    pub(super) fn queuing_routine(&self, space: &str, timer: TimerId) -> bool {
        self.work
            .manual
            .get(&(space.to_owned(), timer))
            .is_some_and(|(_, pending)| *pending)
    }

    pub(super) fn queue_routine(
        &mut self,
        space: String,
        project: String,
        timer: TimerId,
        cx: &mut Context<Self>,
    ) {
        let key = (space.clone(), timer);
        let (request, pending) = self
            .work
            .manual
            .entry(key.clone())
            .or_insert_with(|| (ClientCore::request_id(), false));
        if *pending {
            return;
        }
        *pending = true;
        let request = request.clone();
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let result = core
                    .read()
                    .await
                    .run_routine_once(Some(&space), timer, request)
                    .await;
                (space, result)
            },
            move |this, (space, result), _| match result {
                Ok(timer) => {
                    this.work.manual.remove(&key);
                    this.state.report(Activity::Done(format!(
                        "Queued routine {} in {project}.",
                        timer.id
                    )));
                    this.state.timer_changed(&space, &project, timer);
                }
                Err(error) => {
                    if let Some((_, pending)) = this.work.manual.get_mut(&key) {
                        *pending = false;
                    }
                    this.state.report(Activity::Failed(describe(&error)));
                }
            },
            cx,
        );
        cx.notify();
    }

    pub(super) fn work_link(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        space: &str,
        page: WorkPage,
        cx: &mut Context<Self>,
    ) -> Button {
        let screen = Screen::Work {
            space: space.to_owned(),
            page,
        };
        button(id.into())
            .small()
            .ghost()
            .label(label.into())
            .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
    }
    pub(super) fn load_work(&mut self, space: String, page: WorkPage, cx: &mut Context<Self>) {
        if self.work.loading || self.work.data.is_some() {
            return;
        }
        self.work.loading = true;
        let generation = self.work.generation;
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                core.read()
                    .await
                    .overview(Some(&space), page.query())
                    .await
                    .map(|(_, reply)| reply)
                    .map_err(|e| describe(&e))
            },
            move |this, result, _| {
                if this.work.generation == generation {
                    this.work.loading = false;
                    this.work.data = Some(result);
                }
            },
            cx,
        );
    }
    fn source_task(&self, space: &str, id: TaskId, key: String, cx: &mut Context<Self>) -> Button {
        let screen = Screen::Task {
            space: space.to_owned(),
            task: id,
        };
        button(SharedString::from(key))
            .small()
            .ghost()
            .label(id.to_string())
            .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
    }

    fn usage_screen(
        &self,
        space: &str,
        days: u32,
        refresh: Button,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let range = h_flex()
            .gap_1()
            .flex_wrap()
            .children([1, 7, 30, 90].map(|d| {
                let choice = self
                    .work_link(
                        format!("range-{d}"),
                        format!("{d} {}", count_form(d, "day", "days")),
                        space,
                        WorkPage::Usage(d),
                        cx,
                    )
                    .tab_stop(true)
                    .selected(d == days);
                if d == days {
                    choice.secondary().outline()
                } else {
                    choice
                }
            }));
        let sidebar = if self.sidebar_collapsed {
            Pixels::ZERO
        } else {
            SIDEBAR_WIDTH
        };
        let content_width =
            (window.viewport_size().width - sidebar - LG * 2. - SM).min(READING_WIDTH);
        // Leave room for the numeric columns and a readable agent column.
        let compact = content_width < window.rem_size() * 40.;
        let border = cx.theme().border;
        let mut body = v_flex()
            .w_full()
            .max_w(READING_WIDTH)
            .mx_auto()
            .pr_2()
            .pb_2();
        match &self.work.data {
            Some(Ok(OverviewReply::SpaceUsage { groups })) => {
                let runs = groups.iter().map(|g| u128::from(g.runs)).sum();
                let reports = groups.iter().map(|g| u128::from(g.cost_reports)).sum();
                let cost: f64 = groups.iter().map(|g| g.cost).sum();
                let runs = format_count(runs);
                let cost = reported_cost(cost, reports);
                body = body.child(
                    h_flex()
                        .items_start()
                        .flex_wrap()
                        .gap_x_8()
                        .gap_y_4()
                        .p_4()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().muted)
                        .child(
                            v_flex()
                                .min_w(rems(4.))
                                .gap_1()
                                .child(meta("Runs", cx))
                                .child(section(runs.clone()).text_xl()),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .flex_basis(rems(14.))
                                .min_w_0()
                                .gap_1()
                                .child(meta("Reported cost", cx))
                                .child(section(cost).text_xl())
                                .child(meta(
                                    format!(
                                        "{} of {runs} runs reported cost",
                                        format_count(reports)
                                    ),
                                    cx,
                                )),
                        ),
                );
                if groups.is_empty() {
                    body =
                        body.child(meta("No runs in this period. Try a longer range.", cx).py_4());
                } else {
                    body = body.child(section("Usage breakdown").pt_6().pb_3());
                    if !compact {
                        body = body.child(
                            h_flex()
                                .justify_between()
                                .px_3()
                                .pb_2()
                                .child(meta("Agent", cx))
                                .child(usage_metrics(None, false, cx)),
                        );
                    }
                    body = body.children(
                        groups
                            .iter()
                            .map(|g| self.usage_group(space, g, compact, border, cx)),
                    );
                }
            }
            None => {
                body = body
                    .child(meta("Loading usage…", cx).py_3())
                    .children((0..3).map(|_| {
                        div()
                            .h_8()
                            .mb_2()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().skeleton)
                    }));
            }
            Some(Err(error)) => {
                body = body
                    .gap_1()
                    .py_3()
                    .child(section("Couldn’t load usage"))
                    .child(meta(error.clone(), cx))
                    .child(meta("Refresh to try again, or choose another period.", cx));
            }
            _ => body = body.child("The hub returned an unexpected response."),
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_4()
            .child(
                v_flex()
                    .w_full()
                    .max_w(READING_WIDTH)
                    .mx_auto()
                    .pr_2()
                    .gap_2()
                    .child(page_header(
                        "Space usage",
                        Some(meta(space.to_owned(), cx)),
                        Some(h_flex().child(refresh.tab_stop(true))),
                    ))
                    .child(range),
            )
            .child(
                v_flex()
                    .id("work-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .into_any_element()
    }

    fn usage_group(
        &self,
        space: &str,
        group: &UsageGroup,
        compact: bool,
        border: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        let key = (
            group.project.clone(),
            group.agent.clone(),
            group.backend.clone(),
        );
        let expanded = self.work.expanded_usage.contains(&key);
        let heading = format!("{} / {} / {}", group.project, group.agent, group.backend);
        let disclosure = (!group.tasks.is_empty()).then(|| {
            let n = group.tasks.len();
            button(format!("usage-tasks-{heading}"))
                .small()
                .ghost()
                .self_start()
                .tab_stop(true)
                .icon(if expanded {
                    AppIcon::ChevronDown
                } else {
                    AppIcon::ChevronRight
                })
                .label(format!(
                    "{} {}",
                    format_count(n as u128),
                    count_form(n, "task", "tasks")
                ))
                .accessibility_label(format!(
                    "{} tasks for {heading}",
                    if expanded { "Hide" } else { "Show" }
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.work.expanded_usage.remove(&key) {
                        this.work.expanded_usage.insert(key.clone());
                    }
                    cx.notify();
                }))
        });
        v_flex()
            .gap_2()
            .px_3()
            .py_3()
            .border_t_1()
            .border_color(border)
            .child(
                h_flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .when(compact, |row| row.flex_col().justify_start())
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .when(compact, |identity| identity.w_full())
                            .when(!compact, |identity| identity.flex_1())
                            .child(
                                div()
                                    .whitespace_normal()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(group.agent.clone()),
                            )
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap_x_2()
                                    .child(meta(group.project.clone(), cx).whitespace_normal())
                                    .child(meta(group.backend.clone(), cx).whitespace_normal()),
                            )
                            .children(disclosure),
                    )
                    .child(
                        usage_metrics(Some(group), compact, cx)
                            .max_w_full()
                            .flex_none(),
                    ),
            )
            .children(expanded.then(|| {
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(group.tasks.iter().map(|id| {
                        self.source_task(space, *id, format!("usage-{heading}-{id}"), cx)
                            .tab_stop(true)
                    }))
            }))
    }

    fn workflow_action(
        &mut self,
        space: String,
        project: String,
        request: TaskRequest,
        cx: &mut Context<Self>,
    ) {
        if self.work.saving {
            return;
        }
        self.work.saving = true;
        let generation = self.work.generation;
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let result = core.read().await.workflow(&space, request).await;
                (space, project, result)
            },
            move |this, (space, project, result), cx| {
                let visible = this.work.generation == generation;
                if visible {
                    this.work.saving = false;
                }
                match result {
                    Ok(TaskReply::Task { task }) => {
                        this.state.report(Activity::Done(format!(
                            "Started review workflow at {} in {project}.",
                            task.id
                        )));
                        if visible {
                            this.go(
                                Screen::Task {
                                    space,
                                    task: task.id,
                                },
                                cx,
                            );
                        }
                    }
                    Ok(TaskReply::ReviewTeam { timer }) => {
                        this.state.report(Activity::Done(format!(
                            "PR review routine #{} is {}.",
                            timer.id, timer.status
                        )));
                        if visible {
                            this.go(
                                Screen::Project {
                                    space,
                                    project,
                                    tab: Tab::Routines,
                                },
                                cx,
                            );
                        }
                    }
                    Ok(TaskReply::ReviewRecipe { recipe }) => {
                        if visible {
                            this.work.data = Some(Ok(OverviewReply::ReviewRecipe { recipe }));
                        }
                        this.state.report(Activity::Done(format!(
                            "Saved the review workflow in {space}."
                        )));
                    }
                    Ok(_) => this
                        .state
                        .report(Activity::Failed("Unexpected workflow response.".into())),
                    Err(error) => this.state.report(Activity::Failed(describe(&error))),
                }
            },
            cx,
        );
    }

    fn workflow_picker(
        &self,
        choice: WorkflowChoice,
        selected: &str,
        options: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let view = cx.entity().downgrade();
        let generation = self.work.generation;
        let label = choice.label();
        let action = format!("Choose {}", label.to_lowercase());
        let value = if selected.is_empty() {
            action.clone()
        } else {
            selected.to_owned()
        };
        let accessible = if selected.is_empty() {
            action
        } else {
            format!("{action}, current selection: {selected}")
        };
        v_flex().min_w_0().gap(SM).child(meta(label, cx)).child(
            button(choice.id())
                .small()
                .outline()
                .w_full()
                .min_w_0()
                .tab_stop(true)
                .icon(AppIcon::ChevronDown)
                .label(value.clone())
                .accessibility_label(accessible)
                .tooltip(value)
                .disabled(self.work.saving || options.is_empty())
                .dropdown_menu(move |mut menu, _, _| {
                    for option in &options {
                        let (picked, view) = (option.clone(), view.clone());
                        menu = menu.item(PopupMenuItem::new(option.clone()).on_click(
                            move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    if this.work.generation == generation && !this.work.saving {
                                        *choice.field(&mut this.work) = picked.clone();
                                        cx.notify();
                                    }
                                });
                            },
                        ));
                    }
                    menu
                }),
        )
    }

    pub(super) fn workflow_tab(
        &mut self,
        space: &str,
        project: &str,
        archived: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body = v_flex().gap(MD).w_full().min_w_0();
        if archived {
            body = body.child(meta(
                "Unarchive this project to configure or start a workflow.",
                cx,
            ));
        } else {
            match self.work.data.as_ref() {
                None => body = body.child(meta("Loading review workflow…", cx)),
                Some(Ok(OverviewReply::ReviewRecipe { recipe })) => {
                    let recipe = recipe.clone();
                    body = body.child(self.review_form(space, project, &recipe, window, cx));
                }
                Some(result) => {
                    let message = result.as_ref().err().map_or(
                        "The hub returned an unexpected workflow response.",
                        String::as_str,
                    );
                    body = body.child(meta(message.to_owned(), cx)).child(
                        button("retry-workflow-view")
                            .small()
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.work.data = None;
                                this.load_screen(cx);
                            })),
                    );
                }
            }
        }
        v_flex()
            .id("project-workflow-panel")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .overflow_y_scrollbar()
            .child(body)
            .into_any_element()
    }

    fn review_form(
        &mut self,
        space: &str,
        project: &str,
        recipe: &ReviewRecipe,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        use gpui_kit::component::WindowExt as _;
        if self.work.loaded_recipe != Some(recipe.version) {
            if self.work.loaded_recipe.is_none() {
                let repos = self.state.task_repositories(space, project);
                let repository = if repos.len() == 1 {
                    repos[0].as_str()
                } else {
                    ""
                };
                self.work.repository = repository.to_owned();
                self.work.engineer = "Engineer".into();
                self.work.reviewer = "Reviewer".into();
                self.work
                    .goal
                    .update(cx, |goal, cx| goal.set_value("", window, cx));
            }
            self.work.implementation.update(cx, |f, cx| {
                f.set_value(recipe.implementation.clone(), window, cx)
            });
            self.work
                .review
                .update(cx, |f, cx| f.set_value(recipe.review.clone(), window, cx));
            self.work.loaded_recipe = Some(recipe.version);
        }
        let repositories = self.state.task_repositories(space, project);
        if self.work.repository.is_empty() && repositories.len() == 1 {
            self.work.repository.clone_from(&repositories[0]);
        }
        let bound = self
            .state
            .project(space, project)
            .map(|project| {
                project
                    .bindings
                    .iter()
                    .filter_map(|binding| {
                        (!binding.is_lead && binding.mode != Mode::Off)
                            .then_some(binding.agent.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut specialists = bound.clone();
        if let Some(space) = self.state.space(space) {
            specialists.extend(space.agents.iter().map(|agent| agent.name.clone()));
        }
        specialists.extend(["Engineer".into(), "Reviewer".into()]);
        specialists.sort_unstable();
        specialists.dedup();
        let roles_ready = self.work.engineer != self.work.reviewer
            && bound.contains(&self.work.engineer)
            && bound.contains(&self.work.reviewer);
        let dirty = self.work.implementation.read(cx).value().as_str() != recipe.implementation
            || self.work.review.read(cx).value().as_str() != recipe.review;
        let blocker = workflow_start_blocker(
            dirty,
            self.state.project_ceiling(space, project) == Mode::Off,
            roles_ready,
            repositories.contains(&self.work.repository),
            self.work.goal.read(cx).value().as_str(),
        );
        let version = recipe.version;
        let (s, p) = (space.to_owned(), project.to_owned());
        let save = button("save-recipe")
            .small()
            .label("Save steps for this space")
            .tab_stop(true)
            .self_start()
            .max_w_full()
            .disabled(self.work.saving || !dirty)
            .on_click(cx.listener(move |this, _, _, cx| {
                let recipe = ReviewRecipe {
                    version,
                    implementation: this.work.implementation.read(cx).value().to_string(),
                    review: this.work.review.read(cx).value().to_string(),
                };
                this.workflow_action(
                    s.clone(),
                    p.clone(),
                    TaskRequest::SaveReviewRecipe { recipe },
                    cx,
                );
            }));
        let (s, p) = (space.to_owned(), project.to_owned());
        let saved = recipe.clone();
        let start = button("run-review")
            .primary()
            .label("Start workflow")
            .tab_stop(true)
            .self_start()
            .max_w_full()
            .disabled(self.work.saving || blocker.is_some())
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.work.implementation.read(cx).value().as_str() != saved.implementation
                    || this.work.review.read(cx).value().as_str() != saved.review
                {
                    this.state.report(Activity::Failed(
                        "Save the edited workflow steps before starting.".into(),
                    ));
                    cx.notify();
                    return;
                }
                let mut request = TaskRequest::StartReviewWorkflow {
                    project: p.clone(),
                    version,
                    engineer: this.work.engineer.clone(),
                    reviewer: this.work.reviewer.clone(),
                    repository: this.work.repository.clone(),
                    goal: this.work.goal.read(cx).value().to_string(),
                    client_request_id: String::new(),
                };
                if this
                    .work
                    .retry_workflow
                    .as_ref()
                    .is_none_or(|(draft, _)| draft != &request)
                {
                    this.work.retry_workflow = Some((request.clone(), ClientCore::request_id()));
                }
                if let (
                    TaskRequest::StartReviewWorkflow {
                        client_request_id, ..
                    },
                    Some((_, id)),
                ) = (&mut request, &this.work.retry_workflow)
                {
                    client_request_id.clone_from(id);
                }
                this.workflow_action(s.clone(), p.clone(), request, cx);
            }));
        let (s, p) = (space.to_owned(), project.to_owned());
        let preset = button("preview-team")
            .small()
            .label("Preview PR review team…")
            .tab_stop(true)
            .self_start()
            .max_w_full()
            .disabled(self.work.saving || self.work.engineer == self.work.reviewer)
            .on_click(cx.listener(move |this, _, window, cx| {
                let engineer = this.work.engineer.clone();
                let reviewer = this.work.reviewer.clone();
                let recurrence = Recurrence {
                    zone: this.work.zone.clone(),
                    rule: Rule::Weekdays {
                        days: vec![
                            Weekday::Mon,
                            Weekday::Tue,
                            Weekday::Wed,
                            Weekday::Thu,
                            Weekday::Fri,
                        ],
                        hour: 9,
                        minute: 0,
                    },
                };
                let summary = threadlane_core::work::review_routine(&engineer, &reviewer);
                let mut roles = Vec::new();
                for (role, name) in [("Engineer", &engineer), ("Reviewer", &reviewer)] {
                    let agent = this
                        .state
                        .space(&s)
                        .and_then(|space| space.agents.iter().find(|a| &a.name == name));
                    let detail = agent.map_or_else(
                        || format!("Create from the {role} template"),
                        |agent| {
                            let skills = agent
                                .skills
                                .iter()
                                .map(|binding| binding.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            if skills.is_empty() {
                                "Reuse existing agent".into()
                            } else {
                                format!("Reuse existing agent · {skills}")
                            }
                        },
                    );
                    roles.push(format!("{role} · {name} · {detail}"));
                }
                let request = TaskRequest::InstallReviewTeam {
                    project: p.clone(),
                    engineer,
                    reviewer,
                    recurrence: recurrence.clone(),
                };
                let (s, p, view) = (s.clone(), p.clone(), cx.entity().downgrade());
                window.open_dialog(cx, move |dialog, _, cx| {
                    let (s, p, view, request) =
                        (s.clone(), p.clone(), view.clone(), request.clone());
                    dialog
                        .title(format!("PR review team for {p}"))
                        .child(
                            v_flex()
                                .gap(SM)
                                .children(roles.clone())
                                .child(meta(
                                    "New bindings use On request. No additional skills required.",
                                    cx,
                                ))
                                .child(section("Proposed routine"))
                                .child(summary.clone())
                                .child(format!("{recurrence} · installed paused")),
                        )
                        .footer(
                            h_flex()
                                .gap(SM)
                                .child(
                                    button("cancel-team")
                                        .label("Cancel")
                                        .on_click(|_, w, cx| w.close_dialog(cx)),
                                )
                                .child(
                                    button("install-team")
                                        .primary()
                                        .label("Install team")
                                        .on_click(move |_, w, cx| {
                                            w.close_dialog(cx);
                                            let _ = view.update(cx, |this, cx| {
                                                this.workflow_action(
                                                    s.clone(),
                                                    p.clone(),
                                                    request.clone(),
                                                    cx,
                                                )
                                            });
                                        }),
                                ),
                        )
                });
            }));
        let implementation = v_flex().gap(SM).child("1. Implementation").child(
            Textarea::new(&self.work.implementation)
                .w_full()
                .min_w_0()
                .aria_label("Implementation instructions"),
        );
        let review = v_flex().gap(SM).child("2. Review").child(
            Textarea::new(&self.work.review)
                .w_full()
                .min_w_0()
                .aria_label("Review instructions"),
        );
        let engineer = self.workflow_picker(
            WorkflowChoice::Engineer,
            &self.work.engineer,
            specialists.clone(),
            cx,
        );
        let reviewer = self.workflow_picker(
            WorkflowChoice::Reviewer,
            &self.work.reviewer,
            specialists,
            cx,
        );

        v_flex()
            .w_full()
            .min_w_0()
            .gap(LG)
            .child(
                v_flex().gap(SM).child(section("Review workflow")).child(
                    meta(
                        "An engineer implements the goal, a reviewer checks the result, and you decide whether to accept it.",
                        cx,
                    )
                    .max_w(READING_WIDTH)
                    .whitespace_normal(),
                ),
            )
            .child(
                v_flex()
                    .gap(MD)
                    .child(section("Reusable steps"))
                    .child(workflow_pair(implementation, review))
                    .child(save),
            )
            .child(
                v_flex()
                    .gap(MD)
                    .child(section("Project team"))
                    .child(workflow_pair(engineer, reviewer))
                    .child(preset),
            )
            .child(
                v_flex()
                    .gap(MD)
                    .child(section("Start a review workflow"))
                    .child(self.workflow_picker(WorkflowChoice::Repository, &self.work.repository, repositories, cx))
                    .child(
                        v_flex()
                            .gap(SM)
                            .child("Goal")
                            .child(field(&self.work.goal).w_full().min_w_0().aria_label("Workflow goal")),
                    )
                    .children(blocker.map(|reason| meta(reason, cx).whitespace_normal()))
                    .child(start),
            )
            .child(self.workflow_runs(space, project, cx))
    }

    fn workflow_runs(&self, space: &str, project: &str, cx: &mut Context<Self>) -> Div {
        let mut panel = v_flex().gap(SM).child(section("Recent workflow runs"));
        let Some(view) = self.state.project(space, project) else {
            return panel.child(meta("Loading tasks…", cx));
        };
        let runs = ViewWorkflowRuns::from_tasks(&view.tasks);
        if runs.implementations.is_empty() {
            return panel.child(meta("No review workflows started in this project yet.", cx));
        }
        for first in runs.implementations.into_iter().rev().take(5) {
            let review = runs.reviews.get(&first.task.id);
            let label = |step: &str, row: &threadlane_core::client::TaskSummary| {
                format!(
                    "{step} · {} · {} · {}",
                    row.task.id, row.agent, row.task.status
                )
            };
            panel = panel.child(
                v_flex()
                    .gap(SM)
                    .child(meta(first.task.title.clone(), cx))
                    .child(self.source_task_label(
                        space,
                        first.task.id,
                        label("Implement", first),
                        cx,
                    ))
                    .children(review.map(|row| {
                        self.source_task_label(space, row.task.id, label("Review", row), cx)
                    })),
            );
        }
        panel
    }

    fn source_task_label(
        &self,
        space: &str,
        id: TaskId,
        label: String,
        cx: &mut Context<Self>,
    ) -> Button {
        let screen = Screen::Task {
            space: space.to_owned(),
            task: id,
        };
        button(SharedString::from(format!("workflow-task-{id}")))
            .small()
            .outline()
            .label(label)
            .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
    }

    fn occurrence_row(
        &self,
        space: &str,
        row: &Occurrence,
        when: &When,
        cx: &mut Context<Self>,
    ) -> Div {
        let origin = if row.manual {
            "Run once by owner"
        } else {
            "Scheduled"
        };
        let mut line = v_flex()
            .gap(SM)
            .child(section(format!(
                "{} · {}",
                self.work.moment(row.due, when),
                row.status
            )))
            .child(meta(
                format!("{origin} · {} · Routine #{}", row.project, row.timer),
                cx,
            ));
        if let Some(start) = row.started {
            line = line.child(meta(
                format!("Started {}", self.work.moment(start, when)),
                cx,
            ));
        }
        if row.input_count > 1 {
            line = line.child(meta(
                format!("Shared run · {} input events", row.input_count),
                cx,
            ));
        }
        if let Some(result) = &row.result
            && !result.trim().is_empty()
        {
            line = line.child(self.prose(
                format!("occurrence-{space}-{}", row.event),
                space,
                Some(&row.project),
                result.clone(),
                cx,
            ));
        }
        line.child(
            h_flex().flex_wrap().gap(SM).children(
                row.tasks.iter().map(|id| {
                    self.source_task(space, *id, format!("result-{}-{id}", row.event), cx)
                }),
            ),
        )
    }
    pub(super) fn work_screen(
        &mut self,
        space: &str,
        page: &WorkPage,
        when: &When,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body = v_flex().gap(MD);
        let header = match page {
            WorkPage::Outputs { project: p, .. } | WorkPage::Image { project: p, .. } => {
                format!("{space} / {p}")
            }
            WorkPage::History { timer, .. } => format!("{space} / Routine #{timer}"),
            _ => space.to_owned(),
        };
        let refresh = button("refresh-work")
            .small()
            .label("Refresh")
            .disabled(self.work.loading || self.work.saving)
            .on_click(cx.listener(|this, _, _, cx| {
                this.work.data = None;
                this.load_screen(cx);
            }));
        if let WorkPage::Usage(days) = page {
            return self.usage_screen(space, *days, refresh, window, cx);
        }
        match (&self.work.data, page) {
            (None, _) => body = body.child(meta("Loading…", cx)),
            (Some(Err(error)), _) => body = body.child(error.clone()),
            (
                Some(Ok(OverviewReply::RoutineHistory { rows })),
                WorkPage::History { timer, before },
            ) => {
                if rows.is_empty() {
                    body = body.child("No occurrences on this page.");
                }
                for row in rows {
                    body = body.child(self.occurrence_row(space, row, when, cx));
                }
                if before.is_some() {
                    body = body.child(self.work_link(
                        "latest",
                        "Latest occurrences",
                        space,
                        WorkPage::History {
                            timer: *timer,
                            before: None,
                        },
                        cx,
                    ));
                }
                if let Some(last) = rows.last().filter(|_| rows.len() == 50) {
                    body = body.child(self.work_link(
                        "older",
                        "Older occurrences",
                        space,
                        WorkPage::History {
                            timer: *timer,
                            before: Some(last.event),
                        },
                        cx,
                    ));
                }
            }
            (Some(Ok(OverviewReply::Agenda { recent, upcoming })), _) => {
                let tomorrow = matches!(page, WorkPage::Tomorrow(_));
                body = body.child(
                    h_flex()
                        .gap(SM)
                        .child(
                            self.work_link("week", "Next 7 days", space, WorkPage::Agenda, cx)
                                .disabled(!tomorrow),
                        )
                        .child(
                            self.work_link(
                                "tomorrow",
                                "Tomorrow",
                                space,
                                WorkPage::Tomorrow(self.work.zone.clone()),
                                cx,
                            )
                            .disabled(tomorrow),
                        ),
                );
                body = body.child(meta(
                    format!("{} · First 100 occurrences", self.work.zone),
                    cx,
                ));
                if upcoming.is_empty() {
                    body = body.child("No routines due.");
                }
                for (index, row) in upcoming.iter().enumerate() {
                    let overlap = index
                        .checked_sub(1)
                        .and_then(|i| upcoming.get(i))
                        .is_some_and(|r| r.due == row.due)
                        || upcoming.get(index + 1).is_some_and(|r| r.due == row.due);
                    body = body.child(
                        h_flex()
                            .gap(SM)
                            .flex_wrap()
                            .child(meta(self.work.moment(row.due, when), cx))
                            .children(overlap.then(|| meta("Same start time", cx)))
                            .child(self.work_link(
                                format!("due-{}-{}", row.timer, row.due.as_millis()),
                                format!(
                                    "{} · {}",
                                    row.project,
                                    row.instruction.lines().next().unwrap_or("Routine")
                                ),
                                space,
                                WorkPage::History {
                                    timer: row.timer,
                                    before: None,
                                },
                                cx,
                            )),
                    );
                }
                body = body.child(section("Recent runs"));
                if recent.is_empty() {
                    body = body.child("No recorded runs.");
                }
                for row in recent {
                    body = body.child(self.work_link(
                        format!("event-{}", row.event),
                        format!(
                            "{} · {} · #{} · {}",
                            self.work.moment(row.due, when),
                            row.project,
                            row.timer,
                            row.status
                        ),
                        space,
                        WorkPage::History {
                            timer: row.timer,
                            before: Some(EventId(row.event.get() + 1)),
                        },
                        cx,
                    ));
                }
            }
            (Some(Ok(OverviewReply::Outputs { rows })), WorkPage::Outputs { project, search }) => {
                if !self.work.loaded_search {
                    self.work
                        .search
                        .update(cx, |input, cx| input.set_value(search.clone(), window, cx));
                    self.work.loaded_search = true;
                }
                let target = project.clone();
                let named = space.to_owned();
                body = body.child(
                    h_flex()
                        .gap(SM)
                        .child(field(&self.work.search).aria_label("Search project outputs"))
                        .child(
                            button("search-outputs")
                                .label("Search")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let search = this.work.search.read(cx).value().to_string();
                                    this.go(
                                        Screen::Work {
                                            space: named.clone(),
                                            page: WorkPage::Outputs {
                                                project: target.clone(),
                                                search,
                                            },
                                        },
                                        cx,
                                    );
                                })),
                        ),
                );
                if rows.is_empty() {
                    body = body.child("No matching outputs.");
                } else {
                    body = body.child(meta("Most recent 100 matches", cx));
                }
                for row in rows {
                    let mut line = h_flex()
                        .gap(SM)
                        .flex_wrap()
                        .child(meta(self.work.moment(row.at, when), cx));
                    let key = match &row.source {
                        OutputSource::Attachment { event, index } => {
                            format!("attachment-{event}-{index}")
                        }
                        OutputSource::Report(path) => format!("report-{path}"),
                        OutputSource::PullRequest(url) => {
                            format!("pr-{url}-{:?}-{}", row.task, row.at.as_millis())
                        }
                    };
                    let task_key = format!("{key}-task");
                    let link = match &row.source {
                        OutputSource::Attachment { event, index } => self.work_link(
                            key,
                            row.name.clone(),
                            space,
                            WorkPage::Image {
                                project: project.clone(),
                                event: *event,
                                index: *index,
                            },
                            cx,
                        ),
                        OutputSource::Report(path) => {
                            let (s, p, path) = (space.to_owned(), project.clone(), path.clone());
                            button(SharedString::from(key))
                                .small()
                                .label(row.name.clone())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.go(
                                        Screen::Project {
                                            space: s.clone(),
                                            project: p.clone(),
                                            tab: Tab::Memory,
                                        },
                                        cx,
                                    );
                                    this.select_memory(&s, &p, &path, cx);
                                }))
                        }
                        OutputSource::PullRequest(url) => {
                            let target = url.clone();
                            button(SharedString::from(key))
                                .small()
                                .label(row.name.clone())
                                .tooltip(target.clone())
                                .disabled(!target.starts_with("https://"))
                                .on_click(move |_, _, cx| {
                                    if target.starts_with("https://") {
                                        cx.open_url(&target);
                                    }
                                })
                        }
                    };
                    line = line.child(link);
                    if let Some(id) = row.task {
                        line = line.child(self.source_task(space, id, task_key, cx));
                    } else if let OutputSource::Attachment { event, .. } = &row.source {
                        line =
                            line.child(meta(format!("Project conversation · message {event}"), cx));
                    } else if let Some(run) = row.run {
                        line = line.child(meta(format!("Run {run}"), cx));
                    }
                    body = body.child(line);
                }
            }
            (Some(Ok(OverviewReply::OutputImage { image })), WorkPage::Image { project, .. }) => {
                body = body
                    .child(self.work_link(
                        "all-outputs",
                        "All outputs",
                        space,
                        WorkPage::Outputs {
                            project: project.clone(),
                            search: String::new(),
                        },
                        cx,
                    ))
                    .children(crate::components::chat_images::previews(
                        std::slice::from_ref(image),
                        cx,
                    ));
            }
            _ => body = body.child("The hub returned an unexpected response."),
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .gap(LG)
            .child(page_header(
                page.title(),
                Some(meta(header, cx)),
                Some(h_flex().child(refresh)),
            ))
            .child(
                v_flex()
                    .id("work-content")
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .child(body.max_w(READING_WIDTH)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod usage_tests {
    use super::{reported_cost, workflow_start_blocker};

    #[test]
    fn workflow_start_explains_each_missing_requirement() {
        assert!(
            workflow_start_blocker(true, false, true, true, "Goal")
                .unwrap()
                .contains("Save")
        );
        assert!(
            workflow_start_blocker(false, true, true, true, "Goal")
                .unwrap()
                .contains("Turn on")
        );
        assert!(
            workflow_start_blocker(false, false, false, true, "Goal")
                .unwrap()
                .contains("specialists")
        );
        assert!(
            workflow_start_blocker(false, false, true, false, "Goal")
                .unwrap()
                .contains("repository")
        );
        assert!(
            workflow_start_blocker(false, false, true, true, "  ")
                .unwrap()
                .contains("goal")
        );
        assert_eq!(
            workflow_start_blocker(false, false, true, true, "Goal"),
            None
        );
    }

    #[test]
    fn missing_cost_differs_from_reported_zero() {
        assert_eq!(reported_cost(0.0, 0), "—");
        assert_eq!(reported_cost(0.0, 2), "USD 0.0000");
        assert_eq!(reported_cost(1.2345, 1), "USD 1.2345");
    }
}
