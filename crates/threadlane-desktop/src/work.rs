//! Focused views reached from a project, a routine, or the space sidebar.
use crate::{
    Desktop, Screen, Tab,
    components::{button, field, meta, page_header, section},
    describe,
    icons::AppIcon,
    local_zone,
    tokens::{LG, MD, READING_WIDTH, SCROLLBAR_GUTTER, SM},
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    h_flex,
    input::InputState,
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashSet;
use std::sync::Arc;
use threadlane_client::ClientCore;
use threadlane_core::{
    client::{OverviewReply, OverviewRequest},
    ids::{EventId, TaskId, TimerId},
    work::{Occurrence, OutputSource, UsageGroup},
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
        }
    }
    fn title(&self) -> &'static str {
        match self {
            Self::History { .. } => "Run history",
            Self::Agenda | Self::Tomorrow(_) => "Routine agenda",
            Self::Usage(_) => "Space usage",
            Self::Outputs { .. } => "Project outputs",
            Self::Image { .. } => "Attachment",
        }
    }
}

pub(super) struct WorkView {
    data: Option<Result<OverviewReply, String>>,
    loading: bool,
    generation: u64,
    loaded_search: bool,
    expanded_usage: HashSet<(String, String, String)>,
    search: Entity<InputState>,
    zone: String,
    manual: std::collections::HashMap<(String, TimerId), (String, bool)>,
}

impl WorkView {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search outputs…"));
        Self {
            data: None,
            loading: false,
            generation: 0,
            manual: Default::default(),
            loaded_search: false,
            expanded_usage: HashSet::new(),
            search,
            zone: local_zone().unwrap_or_else(|| "UTC".into()),
        }
    }
    pub(super) fn leave(&mut self) {
        self.generation += 1;
        self.data = None;
        self.loading = false;
        self.loaded_search = false;
        self.expanded_usage.clear();
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
        let compact = usage_is_compact(
            window.viewport_size().width,
            window.rem_size(),
            self.sidebar_collapsed,
            self.sidebar_width,
        );
        let border = cx.theme().border;
        let mut body = v_flex().w_full().max_w(READING_WIDTH).mx_auto();
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
            _ => body = body.child(meta("The hub returned an unexpected response.", cx)),
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_4()
            .child(
                div().flex_none().w_full().pr(SCROLLBAR_GUTTER).child(
                    v_flex()
                        .w_full()
                        .max_w(READING_WIDTH)
                        .mx_auto()
                        .gap_2()
                        .child(page_header(
                            "Space usage",
                            Some(meta(space.to_owned(), cx)),
                            Some(h_flex().child(refresh.tab_stop(true))),
                        ))
                        .child(range),
                ),
            )
            .child(
                v_flex()
                    .id("work-content")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(div().w_full().pr(SCROLLBAR_GUTTER).pb_4().child(body)),
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
            .outline()
            .tab_stop(true)
            .icon(AppIcon::RotateCcw)
            .label("Refresh")
            .disabled(self.work.loading)
            .on_click(cx.listener(|this, _, _, cx| {
                this.work.data = None;
                this.load_screen(cx);
            }));
        if let WorkPage::Usage(days) = page {
            return self.usage_screen(space, *days, refresh, window, cx);
        }
        match (&self.work.data, page) {
            (None, _) => body = body.child(meta("Loading…", cx)),
            (Some(Err(error)), _) => {
                body = body.child(meta(error.clone(), cx).text_color(cx.theme().danger));
            }
            (
                Some(Ok(OverviewReply::RoutineHistory { rows })),
                WorkPage::History { timer, before },
            ) => {
                if rows.is_empty() {
                    body = body.child(meta("No occurrences on this page.", cx));
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
                    ButtonGroup::new("agenda-range")
                        .child(
                            self.work_link("week", "Next 7 days", space, WorkPage::Agenda, cx)
                                .outline()
                                .tab_stop(true)
                                .selected(!tomorrow),
                        )
                        .child(
                            self.work_link(
                                "tomorrow",
                                "Tomorrow",
                                space,
                                WorkPage::Tomorrow(self.work.zone.clone()),
                                cx,
                            )
                            .outline()
                            .tab_stop(true)
                            .selected(tomorrow),
                        ),
                );
                body = body.child(meta(
                    format!("{} · First 100 occurrences", self.work.zone),
                    cx,
                ));
                if upcoming.is_empty() {
                    body = body.child(meta("No routines due.", cx));
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
                    body = body.child(meta("No recorded runs.", cx));
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
                    body = body.child(meta("No matching outputs.", cx));
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
            _ => body = body.child(meta("The hub returned an unexpected response.", cx)),
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap(LG)
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .pr(SCROLLBAR_GUTTER)
                    .child(page_header(
                        page.title(),
                        Some(meta(header, cx)),
                        Some(h_flex().child(refresh)),
                    )),
            )
            .child(
                v_flex()
                    .id("work-content")
                    .min_h_0()
                    .flex_1()
                    .w_full()
                    .overflow_y_scrollbar()
                    .child(
                        div()
                            .w_full()
                            .pr(SCROLLBAR_GUTTER)
                            .pb_4()
                            .child(body.w_full().max_w(READING_WIDTH)),
                    ),
            )
            .into_any_element()
    }
}

/// Switches the usage breakdown to compact stacked rows when the content area
/// cannot hold the agent name and numeric columns comfortably at the current
/// zoom and sidebar width.
fn usage_is_compact(
    viewport_width: Pixels,
    rem: Pixels,
    sidebar_collapsed: bool,
    sidebar_width: Pixels,
) -> bool {
    let sidebar = if sidebar_collapsed {
        Pixels::ZERO
    } else {
        sidebar_width
    };
    let content_width =
        (viewport_width - sidebar - LG * 2. - SCROLLBAR_GUTTER).min(READING_WIDTH.to_pixels(rem));
    content_width < rem * 40.
}

#[cfg(test)]
mod usage_tests {
    use gpui_kit::px;

    use super::{reported_cost, usage_is_compact};

    #[test]
    fn missing_cost_differs_from_reported_zero() {
        assert_eq!(reported_cost(0.0, 0), "—");
        assert_eq!(reported_cost(0.0, 2), "USD 0.0000");
        assert_eq!(reported_cost(1.2345, 1), "USD 1.2345");
    }

    #[test]
    fn usage_compact_breakpoint_tracks_sidebar_width_and_zoom() {
        assert!(!usage_is_compact(px(1180.), px(16.), false, px(220.)));
        assert!(usage_is_compact(px(880.), px(16.), false, px(220.)));
        assert!(!usage_is_compact(px(880.), px(16.), true, px(220.)));
        assert!(usage_is_compact(px(1040.), px(16.), false, px(400.)));
        assert!(usage_is_compact(px(1180.), px(24.), false, px(220.)));
    }
}
