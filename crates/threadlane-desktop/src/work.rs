//! Focused views reached from a project, a routine, or the space sidebar.
use crate::{
    Desktop, Screen, Tab,
    components::{button, field, meta, page_header, section},
    describe, local_zone,
    tokens::{LG, MD, READING_WIDTH, SM},
};
use gpui_kit::component::{
    Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputState, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit::*;
use std::sync::Arc;
use threadlane_client::ClientCore;
use threadlane_core::{
    client::{OverviewReply, OverviewRequest, TaskReply, TaskRequest},
    ids::{EventId, TaskId, TimerId},
    model::{Recurrence, Rule, Weekday},
    work::{Occurrence, OutputSource, ReviewRecipe},
};
use threadlane_desktop::{Activity, When};

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
    Workflow(String),
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
            Self::Workflow(_) => OverviewRequest::ReviewRecipe,
        }
    }
    fn title(&self) -> &'static str {
        match self {
            Self::History { .. } => "Run history",
            Self::Agenda | Self::Tomorrow(_) => "Routine agenda",
            Self::Usage(_) => "Space usage",
            Self::Outputs { .. } => "Project outputs",
            Self::Image { .. } => "Attachment",
            Self::Workflow(_) => "Review workflow",
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
    retry_workflow: Option<(TaskRequest, String)>,
    implementation: Entity<TextareaState>,
    review: Entity<TextareaState>,
    engineer: Entity<InputState>,
    reviewer: Entity<InputState>,
    goal: Entity<InputState>,
    repository: Entity<InputState>,
    search: Entity<InputState>,
    zone: String,
    manual: std::collections::HashMap<(String, TimerId), (String, bool)>,
}

impl WorkView {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let mut input =
            |value: &str| cx.new(|cx| InputState::new(window, cx).default_value(value.to_owned()));
        Self {
            data: None,
            loading: false,
            generation: 0,
            saving: false,
            manual: Default::default(),
            loaded_recipe: None,
            loaded_search: false,
            retry_workflow: None,
            engineer: input("Engineer"),
            reviewer: input("Reviewer"),
            goal: input(""),
            repository: input(""),
            search: input(""),
            implementation: cx.new(|cx| TextareaState::new(window, cx).rows(3)),
            review: cx.new(|cx| TextareaState::new(window, cx).rows(3)),
            zone: local_zone().unwrap_or_else(|| "UTC".into()),
        }
    }
    pub(super) fn leave(&mut self) {
        self.generation += 1;
        self.data = None;
        self.loading = false;
        self.loaded_recipe = None;
        self.loaded_search = false;
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
                for (input, value) in [
                    (&self.work.repository, repository),
                    (&self.work.engineer, "Engineer"),
                    (&self.work.reviewer, "Reviewer"),
                    (&self.work.goal, ""),
                ] {
                    input.update(cx, |input, cx| {
                        input.set_value(value.to_owned(), window, cx)
                    });
                }
            }
            self.work.implementation.update(cx, |f, cx| {
                f.set_value(recipe.implementation.clone(), window, cx)
            });
            self.work
                .review
                .update(cx, |f, cx| f.set_value(recipe.review.clone(), window, cx));
            self.work.loaded_recipe = Some(recipe.version);
        }
        let version = recipe.version;
        let (s, p) = (space.to_owned(), project.to_owned());
        let save = button("save-recipe")
            .small()
            .label("Save steps for this space")
            .disabled(self.work.saving)
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
            .disabled(self.work.saving)
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
                    engineer: this.work.engineer.read(cx).value().to_string(),
                    reviewer: this.work.reviewer.read(cx).value().to_string(),
                    repository: this.work.repository.read(cx).value().to_string(),
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
            .disabled(self.work.saving)
            .on_click(cx.listener(move |this, _, window, cx| {
                let engineer = this.work.engineer.read(cx).value().trim().to_owned();
                let reviewer = this.work.reviewer.read(cx).value().trim().to_owned();
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
        v_flex()
            .gap(MD)
            .child(section("Reusable steps"))
            .child("Implementation")
            .child(
                Textarea::new(&self.work.implementation).aria_label("Implementation instructions"),
            )
            .child("Review")
            .child(Textarea::new(&self.work.review).aria_label("Review instructions"))
            .child(meta(
                "The owner accepts the review or requests changes.",
                cx,
            ))
            .child(save)
            .child(section("Project team"))
            .child("Engineer")
            .child(field(&self.work.engineer).aria_label("Engineer name"))
            .child("Reviewer")
            .child(field(&self.work.reviewer).aria_label("Reviewer name"))
            .child(preset)
            .child(section("Start a review workflow"))
            .child("Repository")
            .child(field(&self.work.repository).aria_label("Registered repository name"))
            .child("Goal")
            .child(field(&self.work.goal).aria_label("Workflow goal"))
            .child(start)
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
            WorkPage::Workflow(p)
            | WorkPage::Outputs { project: p, .. }
            | WorkPage::Image { project: p, .. } => format!("{space} / {p}"),
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
            (Some(Ok(OverviewReply::SpaceUsage { groups })), WorkPage::Usage(days)) => {
                body = body.child(h_flex().gap(SM).children([1, 7, 30, 90].map(|d| {
                    self.work_link(
                        format!("range-{d}"),
                        format!("{d} days"),
                        space,
                        WorkPage::Usage(d),
                        cx,
                    )
                    .disabled(d == *days)
                })));
                if groups.is_empty() {
                    body = body.child("No runs during this period.");
                }
                let runs: u64 = groups.iter().map(|g| g.runs).sum();
                let cost: f64 = groups.iter().map(|g| g.cost).sum();
                let reports: u64 = groups.iter().map(|g| g.cost_reports).sum();
                body = body
                    .child(section(format!("Past {days} days · {runs} runs")))
                    .child(meta(
                        format!("USD {cost:.4} reported · {reports} of {runs} runs supplied cost"),
                        cx,
                    ));
                for g in groups {
                    let heading = format!("{} / {} / {}", g.project, g.agent, g.backend);
                    let totals = format!(
                        "{} runs · {} min · {} tokens ({} complete reports) · USD {:.4} ({} cost reports)",
                        g.runs,
                        g.duration_ms / 60000,
                        g.tokens,
                        g.token_reports,
                        g.cost,
                        g.cost_reports
                    );
                    let info = meta(totals, cx);
                    let tasks = g.tasks.iter().map(|id| {
                        self.source_task(space, *id, format!("usage-{heading}-{id}"), cx)
                    });
                    body = body.child(
                        v_flex()
                            .gap(SM)
                            .child(section(heading.clone()))
                            .child(info)
                            .child(h_flex().gap(SM).flex_wrap().children(tasks)),
                    );
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
            (Some(Ok(OverviewReply::ReviewRecipe { recipe })), WorkPage::Workflow(project)) => {
                let recipe = recipe.clone();
                body = body.child(self.review_form(space, project, &recipe, window, cx));
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
