//! A task workspace: compact header bar, scrollable primary column with docked
//! composer, and a dedicated right-hand inspector rail (collapsing to a sheet
//! on narrow windows). Tasks have no paused state in the hub, so there is no
//! Pause.

use std::time::Duration;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, InteractiveElementExt as _, Selectable as _, Sizable as _,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::client::TaskDetail;
use threadlane_core::ids::TaskId;
use threadlane_core::model::TaskStatus;
use threadlane_desktop::{
    Consequence, EmptyRegion, LatestStep, MilestoneKind, ProseLimit, Queued, TaskControl,
    TaskOutcomeTone, When, changes, milestone_summary, milestones, status_label, task_details,
    task_goal, task_header, task_outcome_banner, task_usage,
};

use super::{gap_line, menu_item};
use crate::components::{
    button, decision_scroller, icon_button, meta, section, status_badge, status_dot,
};
use crate::icons::AppIcon;
use crate::tokens::{
    BAR_HEIGHT, READING_WIDTH, Status, TABULAR, TASK_DETAIL_LABEL_WIDTH, TASK_INSPECTOR_WIDTH,
    TASK_MAIN_MIN_WIDTH, TASK_TIME_LONG_WIDTH, TASK_TIME_SHORT_WIDTH, TYPE_BODY, TYPE_META,
    TYPE_TITLE,
};
use crate::{Desktop, Screen, Tab};

/// How many milestones the live feed shows before "Load earlier".
const FEED_ROWS: usize = 12;

/// Prepared sections rendered in the task workspace's primary column.
struct TaskMainSections<'a> {
    live: Option<LatestStep>,
    feed_rows: &'a [threadlane_desktop::Milestone],
    time_width: Rems,
    timeline: Vec<threadlane_desktop::Moment>,
    pending_cards: Vec<Div>,
    past_cards: Vec<Div>,
}

/// Moves the right-hand inspector into a sheet when the remaining workspace
/// cannot comfortably fit both the primary column and the inspector rail.
fn task_inspector_in_sheet(
    viewport_width: Pixels,
    rem: Pixels,
    sidebar_collapsed: bool,
    sidebar_width: Pixels,
) -> bool {
    let left = if sidebar_collapsed {
        Pixels::ZERO
    } else {
        sidebar_width
    };
    let available = viewport_width - left;
    available < (TASK_MAIN_MIN_WIDTH + TASK_INSPECTOR_WIDTH).to_pixels(rem)
}

/// Picks one shared left-aligned tabular timestamp lane width for Activity and
/// Timeline rows so same-day times (`12:17`) have no artificial indent while
/// multi-day times (`Yesterday 12:17`) still fit on a single column.
fn task_timestamp_width<'a>(times: impl IntoIterator<Item = &'a str>) -> Rems {
    if times
        .into_iter()
        .any(|time| time.contains(char::is_whitespace))
    {
        TASK_TIME_LONG_WIDTH
    } else {
        TASK_TIME_SHORT_WIDTH
    }
}

/// Describes how `other` relates to `current` in the inspector's Related section.
fn related_task_relation(
    current: &TaskDetail,
    other_id: TaskId,
    other_parent: Option<TaskId>,
) -> &'static str {
    let task = &current.summary.task;
    if task.depends_on.contains(&other_id) {
        "Required by this task"
    } else if other_parent == Some(task.id) {
        "Subtask"
    } else if task.parent_task_id == Some(other_id) {
        "Parent task"
    } else {
        "Depends on this task"
    }
}

/// Semantic status tone for the Changes section's publish state line.
fn change_state_status(state: &str) -> Status {
    if state.starts_with("Pushed") {
        Status::Done
    } else if state.starts_with("Waiting on you") || state == "Pushing…" {
        Status::Waiting
    } else if state.starts_with("Not pushed:") {
        Status::Failed
    } else {
        Status::Idle
    }
}

/// Semantic status tone for the Changes section's merge-check line.
fn merge_line_status(line: &str) -> Status {
    if line.contains("Conflicts") {
        Status::Failed
    } else {
        Status::Done
    }
}

impl Desktop {
    pub(crate) fn task(
        &self,
        space: &str,
        task: TaskId,
        when: &When,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(detail) = self.state.task(space, task) else {
            return self.render_missing_task(space, task, when, cx);
        };
        let summary = &detail.summary;
        let archived = self.state.is_archived(space, &summary.project);
        let review_open = self.state.review(space, task).is_some();
        let inspector_in_sheet = task_inspector_in_sheet(
            window.viewport_size().width,
            window.rem_size(),
            self.sidebar_collapsed,
            self.sidebar_width,
        );
        let show_docked_inspector =
            !review_open && !inspector_in_sheet && !self.task_inspector_collapsed;

        let mut header = task_header(detail, when);
        let header_bar =
            self.render_task_header_bar(space, task, detail, header.status, inspector_in_sheet, cx);

        let mut pending_decisions: Vec<_> = self
            .state
            .visible_decisions()
            .filter(|_| !archived)
            .cloned()
            .collect();
        pending_decisions.sort_by_key(|queued| (queued.decision.created_at, queued.decision.id));
        let pending_cards: Vec<_> = pending_decisions
            .iter()
            .map(|queued| {
                self.decision_display(
                    self.state.present_decision(queued, &summary.project, when),
                    cx,
                )
            })
            .collect();

        let mut past_decisions: Vec<_> = detail
            .decisions
            .iter()
            .filter(|decision| !self.state.is_pending(space, &decision.id.to_string()))
            .map(|decision| Queued {
                space: space.to_owned(),
                decision: decision.clone(),
            })
            .collect();
        past_decisions.sort_by_key(|queued| (queued.decision.created_at, queued.decision.id));
        let past_cards: Vec<_> = past_decisions
            .iter()
            .map(|queued| {
                self.decision_display(
                    self.state.present_decision(queued, &summary.project, when),
                    cx,
                )
            })
            .collect();

        let composer = (!archived)
            .then(|| self.render_task_composer_dock(space, task, review_open, window, cx));

        let workspace_body = if review_open {
            let review_content = v_flex()
                .w_full()
                .min_w_0()
                .px_6()
                .py_5()
                .gap_6()
                .child(self.review_view(space, task, cx))
                .children(
                    (!pending_cards.is_empty())
                        .then(|| v_flex().w_full().min_w_0().gap_3().children(pending_cards)),
                )
                .children((!past_cards.is_empty()).then(|| {
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .pt_5()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .gap_3()
                        .child(section("Past decisions"))
                        .children(past_cards)
                }));
            v_flex()
                .flex_1()
                .h_full()
                .min_h_0()
                .min_w_0()
                .child(decision_scroller(
                    "task",
                    review_content,
                    &self.decision_scroll,
                ))
                .children(composer)
                .into_any_element()
        } else {
            let timeline = self.state.task_timeline(space, detail, when);
            let live = header.latest.take();
            let feed_rows = milestones(
                self.task_activity
                    .entries
                    .iter()
                    .rev()
                    .map(|entry| &entry.step),
            );
            let shown_feed = if live.is_some() {
                FEED_ROWS
            } else {
                feed_rows.len()
            };
            let feed_times: Vec<String> = feed_rows
                .iter()
                .rev()
                .take(shown_feed)
                .map(|row| when.moment(row.at))
                .collect();
            let time_width = task_timestamp_width(
                feed_times
                    .iter()
                    .map(String::as_str)
                    .chain(timeline.iter().map(|moment| moment.time.as_str())),
            );

            let main_content = self.render_task_main_content(
                space,
                task,
                detail,
                TaskMainSections {
                    live,
                    feed_rows: &feed_rows,
                    time_width,
                    timeline,
                    pending_cards,
                    past_cards,
                },
                when,
                cx,
            );

            let main_column = v_flex()
                .flex_1()
                .h_full()
                .min_h_0()
                .min_w_0()
                .child(decision_scroller(
                    "task",
                    div().w_full().child(main_content),
                    &self.decision_scroll,
                ))
                .children(composer);

            let inspector = show_docked_inspector.then(|| {
                let sections =
                    self.render_task_inspector_sections(space, task, detail, when, false, cx);
                v_flex()
                    .w(TASK_INSPECTOR_WIDTH)
                    .h_full()
                    .min_h_0()
                    .flex_none()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .child(
                                div()
                                    .id("task-inspector")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .lock_scroll_axis()
                                    .track_scroll(&self.task_inspector_scroll)
                                    .child(sections.w_full().min_h_full()),
                            )
                            .vertical_scrollbar(&self.task_inspector_scroll),
                    )
            });

            h_flex()
                .flex_1()
                .w_full()
                .min_h_0()
                .min_w_0()
                .items_stretch()
                .child(main_column)
                .children(inspector)
                .into_any_element()
        };

        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(header_bar)
            .child(workspace_body)
            .into_any_element()
    }

    fn render_missing_task(
        &self,
        space: &str,
        task: TaskId,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let gap = self
            .state
            .task_gap(space, task)
            .unwrap_or(threadlane_desktop::Gap::Loading);
        let pending: Vec<_> = self
            .state
            .visible_decisions()
            .map(|queued| self.decision(queued, when, cx))
            .collect();
        let header_bar = h_flex()
            .w_full()
            .min_h(BAR_HEIGHT)
            .flex_none()
            .px_6()
            .py_2()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(self.sidebar_collapsed.then(|| {
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(self.collapsed_nav_strip(cx))
                    .child(div().w(px(1.)).h_4().bg(cx.theme().border))
            }))
            .child(meta(space.to_owned(), cx))
            .child(meta("/", cx))
            .child(
                div()
                    .text_size(TYPE_META)
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(task.to_string()),
            );
        let content = v_flex()
            .w_full()
            .max_w(READING_WIDTH)
            .px_6()
            .py_5()
            .gap_4()
            .child(
                div()
                    .text_size(TYPE_TITLE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{space} {task}")),
            )
            .child(gap_line(&gap, "the task", space, cx))
            .children(pending);
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(header_bar)
            .child(decision_scroller(
                "task",
                div().w_full().child(content),
                &self.decision_scroll,
            ))
            .into_any_element()
    }

    fn render_task_header_bar(
        &self,
        space: &str,
        task: TaskId,
        detail: &TaskDetail,
        status: &'static str,
        inspector_in_sheet: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let archived = self.state.is_archived(space, &detail.summary.project);
        let review_open = self.state.review(space, task).is_some();
        let project_name = detail.summary.project.clone();
        let project_screen = Screen::Project {
            space: space.to_owned(),
            project: project_name.clone(),
            tab: Tab::Tasks,
        };
        let stop = self
            .state
            .task_stop(space, task)
            .filter(|_| !archived)
            .map(|target| self.stop_button(space, target, format!("Stop {task}"), cx));
        let in_space = space.to_owned();
        let retry_space = space.to_owned();
        let control = match self.state.task_control(space, task) {
            _ if archived => None,
            TaskControl::Ended => None,
            TaskControl::Retry => Some(
                button("task-retry")
                    .small()
                    .outline()
                    .icon(AppIcon::RotateCcw)
                    .label("Retry")
                    .accessibility_label(format!("Retry {task}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.retry_task(retry_space.clone(), task, cx);
                    })),
            ),
            TaskControl::Cancel | TaskControl::Confirm(_) => Some(
                button("task-cancel")
                    .small()
                    .ghost()
                    .icon(AppIcon::X)
                    .label("Cancel…")
                    .accessibility_label(format!("Cancel {task}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_action(
                            Consequence::CancelTask(in_space.clone(), task),
                            window,
                            cx,
                        );
                    })),
            ),
        };
        let inspector_toggle = (!review_open).then(|| {
            if inspector_in_sheet {
                let sheet_space = space.to_owned();
                button("task-inspector-sheet")
                    .small()
                    .outline()
                    .tab_stop(true)
                    .icon(AppIcon::PanelRightOpen)
                    .label("Details")
                    .accessibility_label(format!("Open details for {task}"))
                    .tooltip("Open task inspector")
                    .on_click(cx.listener(move |_, _, window, cx| {
                        let view = cx.entity().downgrade();
                        let space = sheet_space.clone();
                        window.open_sheet(cx, move |sheet, window, cx| {
                            let width = TASK_INSPECTOR_WIDTH
                                .to_pixels(window.rem_size())
                                .min(window.viewport_size().width);
                            let content = view.update(cx, |this, cx| {
                                let detail = this.state.task(&space, task)?;
                                Some(this.render_task_inspector_sections(
                                    &space,
                                    task,
                                    detail,
                                    &this.when(),
                                    true,
                                    cx,
                                ))
                            });
                            let view = view.clone();
                            sheet
                                .title(format!("{task} details"))
                                .size(width)
                                .resizable(false)
                                .children(content.ok().flatten())
                                .on_close(move |_, window, cx| {
                                    let _gone = view.update(cx, |this, cx| {
                                        if !this.keyboard_focus.contains_focused(window, cx) {
                                            this.keyboard_focus.focus(window, cx);
                                        }
                                    });
                                })
                        });
                    }))
                    .into_any_element()
            } else {
                let open = !self.task_inspector_collapsed;
                icon_button(
                    "toggle-task-inspector",
                    if open {
                        AppIcon::PanelRightClose
                    } else {
                        AppIcon::PanelRightOpen
                    },
                    if open {
                        "Hide inspector".to_owned()
                    } else {
                        "Show inspector".to_owned()
                    },
                )
                .tab_stop(true)
                .selected(open)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.task_inspector_collapsed = !this.task_inspector_collapsed;
                    cx.notify();
                }))
                .into_any_element()
            }
        });

        let breadcrumb_label = format!("{space} / {project_name}");
        let leading = h_flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_1()
            .children(self.sidebar_collapsed.then(|| {
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .mr_1()
                    .child(self.collapsed_nav_strip(cx))
                    .child(div().w(px(1.)).h_4().bg(cx.theme().border))
            }))
            .child(
                button("task-project")
                    .ghost()
                    .small()
                    .px_1()
                    .min_w_0()
                    .max_w(rems(20.))
                    .tab_stop(true)
                    .text_color(cx.theme().muted_foreground)
                    .accessibility_label(format!("Open {project_name} tasks in {space}"))
                    .tooltip(format!("Open {project_name} tasks"))
                    .child(div().min_w_0().truncate().child(breadcrumb_label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.go(project_screen.clone(), cx);
                    })),
            )
            .child(meta("/", cx).flex_none())
            .child(
                self.copyable("copy-task", task.to_string(), cx)
                    .flex_none()
                    .tab_stop(true)
                    .font_family(cx.theme().mono_font_family.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .ml_1()
                    .child(status_badge(status, Status::task(status), cx)),
            );

        let trailing = h_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .children(stop)
            .children(control)
            .children(inspector_toggle);

        h_flex()
            .w_full()
            .min_h(BAR_HEIGHT)
            .flex_none()
            // When the breadcrumb button leads the bar, pl_5 + the button's px_1
            // resolves to 1.5rem (px_6), locking its label onto the main column spine.
            .when(self.sidebar_collapsed, |bar| bar.px_6())
            .when(!self.sidebar_collapsed, |bar| bar.pl_5().pr_6())
            .py_2()
            .gap_3()
            .justify_between()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(leading)
            .child(trailing)
    }

    fn render_task_main_content(
        &self,
        space: &str,
        task: TaskId,
        detail: &TaskDetail,
        sections: TaskMainSections<'_>,
        when: &When,
        cx: &mut Context<Self>,
    ) -> Div {
        let TaskMainSections {
            live,
            feed_rows,
            time_width,
            timeline,
            pending_cards,
            past_cards,
        } = sections;
        let summary = &detail.summary;
        let archived = self.state.is_archived(space, &summary.project);
        let running = summary.task.status == TaskStatus::Running
            || (summary.task.status == TaskStatus::InReview && summary.running_since.is_some());

        let goal = task_goal(detail).map(|goal| {
            div()
                .w_full()
                .min_w_0()
                .text_color(cx.theme().muted_foreground)
                .child(
                    self.prose_among(
                        format!("task-goal-{space}-{task}"),
                        space,
                        Some(&summary.project),
                        Some(&summary.task.repositories),
                        goal,
                        cx,
                    )
                    .limit(ProseLimit::Question),
                )
        });

        let title_block = v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(TYPE_TITLE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .whitespace_normal()
                    .child(summary.task.title.clone()),
            )
            .children(goal)
            .children(self.pull_strip(space, task, cx));

        let waiting_banner = self
            .state
            .waiting_task(space, summary)
            .filter(|_| pending_cards.is_empty() || summary.task.status != TaskStatus::WaitingOnYou)
            .map(|why| {
                let status = Status::task(status_label(
                    summary.task.status,
                    summary.task.waiting_reason.as_deref(),
                ));
                let (ink, tint) = status.colors(cx.theme());
                let icon = match status {
                    Status::Waiting => AppIcon::Clock,
                    _ => AppIcon::Info,
                };
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(ink.opacity(0.30))
                    .bg(tint)
                    .child(
                        Icon::new(icon)
                            .small()
                            .text_color(ink)
                            .flex_none()
                            .mt(rems(0.125)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(TYPE_BODY)
                            .whitespace_normal()
                            .child(why),
                    )
            });

        let stopped_banner = (detail.stopped && !running).then(|| {
            let text = if archived {
                "This task is stopped."
            } else {
                "This task is stopped. Send a message below to resume it."
            };
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.35))
                .child(
                    Icon::new(AppIcon::Info)
                        .small()
                        .text_color(cx.theme().muted_foreground)
                        .flex_none(),
                )
                .child(meta(text, cx).flex_1())
        });

        let outcome_banner = task_outcome_banner(detail).map(|banner| {
            let (status, icon) = match banner.tone {
                TaskOutcomeTone::Done => (Status::Done, AppIcon::CircleCheck),
                TaskOutcomeTone::Failed => (Status::Failed, AppIcon::CircleAlert),
                TaskOutcomeTone::Cancelled => (Status::Idle, AppIcon::Info),
            };
            let (ink, tint) = status.colors(cx.theme());
            let retry_space = space.to_owned();
            let retry_btn = (banner.tone == TaskOutcomeTone::Failed && !archived).then(|| {
                button("task-banner-retry")
                    .small()
                    .outline()
                    .icon(AppIcon::RotateCcw)
                    .label("Retry")
                    .accessibility_label(format!("Retry {task}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.retry_task(retry_space.clone(), task, cx);
                    }))
            });
            v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .px_4()
                .py_3()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(ink.opacity(0.30))
                .bg(tint)
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .justify_between()
                        .items_center()
                        .gap_2()
                        .child(
                            h_flex()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .child(Icon::new(icon).small().text_color(ink).flex_none())
                                .child(
                                    div()
                                        .text_size(TYPE_BODY)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(banner.heading),
                                ),
                        )
                        .children(retry_btn),
                )
                .child(div().w_full().min_w_0().child(self.prose_among(
                    format!("task-outcome-{space}-{task}"),
                    space,
                    Some(&summary.project),
                    Some(&summary.task.repositories),
                    banner.body,
                    cx,
                )))
        });

        let activity = self.live_feed(space, task, live, feed_rows, (time_width, when), cx);

        let mut timeline_section = v_flex()
            .w_full()
            .min_w_0()
            .pt_5()
            .border_t_1()
            .border_color(cx.theme().border)
            .gap_3()
            .child(section("Timeline"));

        if timeline.is_empty() {
            timeline_section = timeline_section.child(meta(EmptyRegion::Timeline.sentence(), cx));
        } else {
            let mut rows = v_flex().w_full().min_w_0().gap_3();
            for moment in timeline {
                let id = format!("task-{space}-{task}-{}", moment.id);
                let row = h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .items_start()
                    .id(SharedString::from(id.clone()))
                    .child(
                        div()
                            .w(time_width)
                            .flex_none()
                            .text_left()
                            // Optical 2px alignment between 12px meta timestamp and 14px body baseline.
                            .pt(rems(0.125))
                            .text_size(TYPE_META)
                            .text_color(cx.theme().muted_foreground)
                            .font_features(TABULAR.clone())
                            .child(moment.time),
                    )
                    .child(div().flex_1().min_w_0().child(self.prose_among(
                        id,
                        space,
                        Some(&summary.project),
                        Some(&summary.task.repositories),
                        moment.text,
                        cx,
                    )));
                rows = if let Some(text) = moment.message {
                    let view = cx.entity().downgrade();
                    rows.child(row.context_menu(move |menu, _, _| {
                        let copy = text.clone();
                        let quote = text.clone();
                        let mut menu =
                            menu.item(menu_item(&view, "Copy text", move |this, _, cx| {
                                this.copy(copy.clone(), cx)
                            }));
                        if !archived {
                            menu = menu.item(menu_item(&view, "Reply", move |this, window, cx| {
                                this.reply_to(&quote, window, cx)
                            }));
                        }
                        menu
                    }))
                } else {
                    rows.child(row)
                };
            }
            timeline_section = timeline_section.child(rows);
        }

        v_flex()
            .w_full()
            .max_w(READING_WIDTH)
            .px_6()
            .py_5()
            .gap_6()
            .child(title_block)
            .children(
                (!pending_cards.is_empty())
                    .then(|| v_flex().w_full().min_w_0().gap_3().children(pending_cards)),
            )
            .children(waiting_banner)
            .children(stopped_banner)
            .children(outcome_banner)
            .child(activity)
            .child(timeline_section)
            .children((!past_cards.is_empty()).then(|| {
                v_flex()
                    .w_full()
                    .min_w_0()
                    .pt_5()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .gap_3()
                    .child(section("Past decisions"))
                    .children(past_cards)
            }))
    }

    fn render_task_composer_dock(
        &self,
        space: &str,
        task: TaskId,
        full_width: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        v_flex()
            .flex_none()
            .w_full()
            .border_t_1()
            .border_color(cx.theme().border)
            .px_6()
            .pt_3()
            .pb_4()
            .child(
                v_flex()
                    .w_full()
                    .when(!full_width, |inner| inner.max_w(READING_WIDTH))
                    .gap_2()
                    .child(self.render_task_composer("Add to this task", window, cx))
                    .child(meta(self.state.task_hint(space, task), cx)),
            )
    }

    fn render_task_inspector_sections(
        &self,
        space: &str,
        task: TaskId,
        detail: &TaskDetail,
        when: &When,
        in_sheet: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let archived = self.state.is_archived(space, &detail.summary.project);
        let details = task_details(detail, when);
        let change = changes(detail);
        let publication = self.state.task_publication(space, task);
        let usage_data = task_usage(detail, when);
        let mono = cx.theme().mono_font_family.clone();

        let section_box = |first: bool, title: &'static str, cx: &App| -> Div {
            v_flex()
                .w_full()
                .min_w_0()
                .gap_3()
                .py_4()
                .when(!in_sheet, |section| section.pl_4().pr_5())
                .when(!first, |section| {
                    section.border_t_1().border_color(cx.theme().border)
                })
                .child(
                    div()
                        .text_size(TYPE_META)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(title),
                )
        };

        // 1. Details section
        let mut details_rows = v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(inspector_kv_row("Agent", details.agent, false, false, cx))
            .child(inspector_kv_row(
                "Machine",
                details
                    .machine
                    .clone()
                    .unwrap_or_else(|| "Not assigned".to_owned()),
                details.machine.is_none(),
                false,
                cx,
            ))
            .child(inspector_kv_row(
                "Backend",
                details
                    .backend
                    .clone()
                    .unwrap_or_else(|| "Not started".to_owned()),
                details.backend.is_none(),
                false,
                cx,
            ))
            .children(details.model.map(|model| {
                inspector_kv_row(
                    "Model",
                    model,
                    details.model_is_default,
                    !details.model_is_default,
                    cx,
                )
            }))
            .child(inspector_kv_row(
                if details.repositories.len() > 1 {
                    "Repos"
                } else {
                    "Repo"
                },
                if details.repositories.is_empty() {
                    "None attached".to_owned()
                } else {
                    details.repositories.join(", ")
                },
                details.repositories.is_empty(),
                false,
                cx,
            ))
            .child(inspector_kv_row(
                "Requested by",
                details.requested_by,
                false,
                false,
                cx,
            ));
        if let Some(started) = details.started {
            details_rows =
                details_rows.child(inspector_kv_row("Started", started, false, false, cx));
        } else {
            details_rows = details_rows.child(inspector_kv_row(
                "Created",
                details.created,
                false,
                false,
                cx,
            ));
        }
        if let Some((label, value)) = details.duration {
            details_rows = details_rows.child(inspector_kv_row(label, value, false, false, cx));
        }
        if let Some(parent_id) = details.parent {
            let parent_screen = Screen::Task {
                space: space.to_owned(),
                task: parent_id,
            };
            details_rows = details_rows.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap_3()
                    .child(inspector_key("Parent", cx))
                    .child(
                        button(SharedString::from(format!("task-parent-{parent_id}")))
                            .ghost()
                            .xsmall()
                            .px_1()
                            .font_family(mono.clone())
                            .label(parent_id.to_string())
                            .accessibility_label(format!("Open parent task {parent_id}"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if in_sheet {
                                    window.close_sheet(cx);
                                }
                                this.go(parent_screen.clone(), cx);
                            })),
                    ),
            );
        }
        if !details.playbooks.is_empty() {
            let skills_list = h_flex().flex_1().min_w_0().gap_1().flex_wrap().children(
                details.playbooks.into_iter().map(|slug| {
                    let open_space = space.to_owned();
                    let open_slug = slug.clone();
                    let label = self
                        .state
                        .space_skills(space)
                        .iter()
                        .find(|skill| skill.slug == slug)
                        .map_or_else(|| slug.clone(), |skill| skill.title.clone());
                    button(SharedString::from(format!("task-skill-{slug}")))
                        .xsmall()
                        .outline()
                        .icon(AppIcon::BookOpen)
                        .label(label)
                        .tooltip(format!(
                            "Active skill: /skill:{slug} — open in Team → Skills"
                        ))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if in_sheet {
                                window.close_sheet(cx);
                            }
                            this.open_skill(open_space.clone(), Some(open_slug.clone()), cx);
                        }))
                }),
            );
            details_rows = details_rows.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap_3()
                    .child(inspector_key("Skills", cx).pt(rems(0.125)))
                    .child(skills_list),
            );
        }
        let details_section = section_box(true, "Details", cx).child(details_rows);

        // 2. Changes section
        let branch_copy = change.branch.clone();
        let branch_row = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .child(
                        Icon::new(AppIcon::GitBranch)
                            .xsmall()
                            .text_color(cx.theme().muted_foreground)
                            .flex_none(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(TYPE_META)
                            .font_family(mono.clone())
                            .child(change.branch.clone()),
                    ),
            )
            .child(
                icon_button(
                    "copy-branch",
                    AppIcon::Copy,
                    format!("Copy {}", change.branch),
                )
                .tab_stop(true)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy(branch_copy.clone(), cx);
                })),
            );

        let state_status = change_state_status(&change.state);
        let state_color = match state_status {
            Status::Idle => cx.theme().foreground,
            other => other.colors(cx.theme()).0,
        };
        let mut changes_body = v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(branch_row)
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(TYPE_META)
                    .text_color(state_color)
                    .whitespace_normal()
                    .child(change.state),
            )
            .children(publication.as_ref().map(|p| {
                let line = meta(p.line.clone(), cx).whitespace_normal();
                if p.blocked {
                    line.text_color(cx.theme().danger)
                } else {
                    line
                }
            }))
            .children(change.merge.map(|merge| {
                let (ink, _) = merge_line_status(&merge).colors(cx.theme());
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(TYPE_META)
                    .text_color(ink)
                    .whitespace_normal()
                    .child(merge)
            }));

        let attachable: Vec<String> = publication
            .into_iter()
            .filter(|_| !archived)
            .flat_map(|p| p.attachable)
            .collect();
        if !attachable.is_empty() {
            changes_body =
                changes_body.child(h_flex().w_full().min_w_0().gap_1().flex_wrap().children(
                    attachable.into_iter().map(|name| {
                        let in_space = space.to_owned();
                        button(SharedString::from(format!("attach-{name}")))
                            .small()
                            .outline()
                            .icon(AppIcon::Plus)
                            .label(format!("Attach {name}"))
                            .accessibility_label(format!(
                                "Attach repository {name} to {task} so its work can be published"
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.attach_repository(in_space.clone(), task, name.clone(), cx);
                            }))
                    }),
                ));
        }

        if let Some(url) = change.pull_request {
            let copy_url = url.clone();
            let open_url = url.clone();
            changes_body = changes_body.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(TYPE_META)
                            .font_family(mono.clone())
                            .text_color(cx.theme().muted_foreground)
                            .child(url.clone()),
                    )
                    .child(
                        icon_button("copy-pull-request", AppIcon::Copy, format!("Copy {url}"))
                            .tab_stop(true)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy(copy_url.clone(), cx);
                            })),
                    )
                    .child(
                        button("open-pull-request")
                            .xsmall()
                            .outline()
                            .tab_stop(true)
                            .icon(AppIcon::ExternalLink)
                            .label("Open")
                            .accessibility_label("Open the pull request in your browser")
                            .on_click(move |_, _, cx| cx.open_url(&open_url)),
                    ),
            );
        }

        if !change.diffs.is_empty() || !self.state.task_pulls(space, task).is_empty() {
            let in_space = space.to_owned();
            let (insertions, deletions) = change.diffs.iter().fold((0, 0), |(i, d), diff| {
                (i + diff.insertions, d + diff.deletions)
            });
            let stats = if change.diffs.is_empty() {
                String::new()
            } else {
                format!(" (+{insertions} -{deletions})")
            };
            changes_body = changes_body.child(
                button("task-review")
                    .small()
                    .outline()
                    .self_start()
                    .tab_stop(true)
                    .icon(AppIcon::FileText)
                    .label(format!("Review changes{stats}"))
                    .accessibility_label(format!("Review {task}'s changed files full width"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if in_sheet {
                            window.close_sheet(cx);
                        }
                        this.open_review(in_space.clone(), task, cx);
                    })),
            );
        }
        let changes_section = section_box(false, "Changes", cx).child(changes_body);

        // 3. Related section
        let mut related_section = section_box(false, "Related", cx);
        if detail.related.is_empty() {
            related_section = related_section
                .child(meta(EmptyRegion::Related.sentence(), cx).whitespace_normal());
        } else {
            let mut list = v_flex().w_full().min_w_0().gap_2();
            for other in &detail.related {
                let relation =
                    related_task_relation(detail, other.task.id, other.task.parent_task_id);
                let screen = Screen::Task {
                    space: space.to_owned(),
                    task: other.task.id,
                };
                let status = status_label(other.task.status, other.task.waiting_reason.as_deref());
                let elapsed = threadlane_desktop::work_elapsed(other, when);
                list = list.child(
                    button(SharedString::from(format!("related-{}", other.task.id)))
                        .ghost()
                        .w_full()
                        .h_auto()
                        .px_2()
                        .py_2()
                        .bg(cx.theme().muted.opacity(0.25))
                        .rounded(cx.theme().radius)
                        .tab_stop(true)
                        .accessibility_label(format!(
                            "{} {}, {status}. {relation}.",
                            other.task.id, other.task.title
                        ))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if in_sheet {
                                window.close_sheet(cx);
                            }
                            this.go(screen.clone(), cx);
                        }))
                        .child(
                            v_flex()
                                .w_full()
                                .min_w_0()
                                .items_start()
                                .gap_1()
                                .child(
                                    h_flex()
                                        .w_full()
                                        .min_w_0()
                                        .justify_between()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_size(TYPE_META)
                                                .font_family(mono.clone())
                                                .text_color(cx.theme().muted_foreground)
                                                .child(other.task.id.to_string()),
                                        )
                                        .child(status_badge(status, Status::task(status), cx)),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .text_left()
                                        .text_size(TYPE_META)
                                        .text_color(cx.theme().foreground)
                                        .whitespace_normal()
                                        .child(other.task.title.clone()),
                                )
                                .child(
                                    h_flex()
                                        .w_full()
                                        .min_w_0()
                                        .justify_between()
                                        .items_center()
                                        .gap_2()
                                        .child(meta(relation, cx))
                                        .children(elapsed.map(|value| {
                                            meta(value, cx).font_features(TABULAR.clone())
                                        })),
                                ),
                        ),
                );
            }
            related_section = related_section.child(list);
        }

        // 4. Usage section
        let mut usage_section = section_box(false, "Usage", cx);
        if usage_data.runs == 0 {
            usage_section = usage_section.child(meta("No runs yet.", cx));
        } else {
            let mut usage_rows = v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(inspector_kv_row(
                    "Runs",
                    usage_data.runs.to_string(),
                    false,
                    false,
                    cx,
                ))
                .children(
                    usage_data
                        .total_time
                        .map(|value| inspector_kv_row("Total time", value, false, false, cx)),
                )
                .children(
                    usage_data
                        .input_tokens
                        .map(|value| inspector_kv_row("Input", value, false, false, cx)),
                )
                .children(
                    usage_data
                        .output_tokens
                        .map(|value| inspector_kv_row("Output", value, false, false, cx)),
                )
                .children(
                    usage_data
                        .cost
                        .map(|value| inspector_kv_row("Cost", value, false, false, cx)),
                );
            if let Some(note) = usage_data.note {
                usage_rows = usage_rows.child(meta(note, cx).whitespace_normal());
            }
            usage_section = usage_section.child(usage_rows);
        }

        v_flex()
            .w_full()
            .min_w_0()
            .child(details_section)
            .child(changes_section)
            .child(related_section)
            .child(usage_section)
    }

    /// The Activity feed: while the task runs, its files, commands and tests
    /// as they happen under a live pulse, amber once the run goes quiet;
    /// afterwards one summary line that opens the same feed.
    fn live_feed(
        &self,
        space: &str,
        task: TaskId,
        live: Option<LatestStep>,
        rows: &[threadlane_desktop::Milestone],
        (time_width, when): (Rems, &When),
        cx: &mut Context<Self>,
    ) -> Div {
        let data = &self.task_activity;
        let expanded = live.is_some() || self.state.activity_open(space, task);
        let heading = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(section("Activity"));
        let heading = match &live {
            Some(latest) => {
                let status = if latest.quiet {
                    Status::Waiting
                } else {
                    Status::Running
                };
                let dot = status_dot(status, cx);
                let dot = if latest.quiet {
                    dot.into_any_element()
                } else {
                    dot.with_animation(
                        "task-feed-pulse",
                        Animation::new(Duration::from_millis(1600))
                            .repeat()
                            .with_easing(pulsating_between(0.35, 1.0)),
                        |dot, delta| dot.opacity(delta),
                    )
                    .into_any_element()
                };
                let line = meta(latest.text.clone(), cx);
                heading
                    .child(dot)
                    .child(if latest.quiet {
                        line.text_color(Status::Waiting.colors(cx.theme()).0)
                    } else {
                        line
                    })
                    .into_any_element()
            }
            None => {
                let in_space = space.to_owned();
                heading
                    .children(data.loaded.then(|| meta(milestone_summary(rows), cx)))
                    .child(
                        button("task-activity")
                            .small()
                            .ghost()
                            .tab_stop(true)
                            .label(if expanded { "Hide steps" } else { "Show steps" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.toggle_activity(&in_space, task);
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
        };
        let mut feed = v_flex().w_full().min_w_0().gap_2().child(heading);
        if !expanded {
            return feed;
        }
        if data.loading && !data.loaded {
            feed = feed.child(meta("Loading activity…", cx));
        } else if data.is_empty() {
            feed = feed.child(meta(EmptyRegion::Activity.sentence(), cx));
        } else if rows.is_empty() && data.error.is_none() {
            feed = feed.child(meta("No files or commands yet.", cx));
        }
        if !data.loading && data.newest.is_some() {
            feed = feed.child(
                button("task-activity-newest")
                    .small()
                    .ghost()
                    .self_start()
                    .tab_stop(true)
                    .label("Show newest activity")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.task_activity.show_newest();
                        cx.notify();
                    })),
            );
        }
        let mono = cx.theme().mono_font_family.clone();
        let shown = if live.is_some() {
            FEED_ROWS
        } else {
            rows.len()
        };
        if !rows.is_empty() {
            let mut list = v_flex().w_full().min_w_0().gap_1();
            for row in rows.iter().rev().take(shown) {
                let (icon, status, verb) = match row.kind {
                    MilestoneKind::File => (AppIcon::FileText, Status::Idle, "Changed"),
                    MilestoneKind::Command => (AppIcon::ChevronRight, Status::Idle, "Ran"),
                    MilestoneKind::TestsPassed => (AppIcon::Check, Status::Done, "Tests passed"),
                    MilestoneKind::TestsFailed => (AppIcon::X, Status::Failed, "Tests failed"),
                    MilestoneKind::Failed => (AppIcon::X, Status::Failed, "Failed"),
                };
                let ink = status.colors(cx.theme()).0;
                list = list.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_3()
                        .items_center()
                        .py(rems(0.125))
                        .child(
                            div()
                                .w(time_width)
                                .flex_none()
                                .text_left()
                                .text_size(TYPE_META)
                                .text_color(cx.theme().muted_foreground)
                                .font_features(TABULAR.clone())
                                .child(when.moment(row.at)),
                        )
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .child(
                                    h_flex()
                                        .w_4()
                                        .flex_none()
                                        .justify_center()
                                        .child(Icon::new(icon).xsmall().text_color(ink)),
                                )
                                .child(meta(verb, cx).flex_none())
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(TYPE_META)
                                        .font_family(mono.clone())
                                        .child(row.text.clone()),
                                ),
                        ),
                );
            }
            feed = feed.child(list);
        }
        if let Some((_, error)) = &data.error {
            feed = feed.child(meta(error.clone(), cx).text_color(cx.theme().danger));
        }
        if !data.loading && (data.next.is_some() || data.error.is_some()) {
            let in_space = space.to_owned();
            let before = data.error.as_ref().map_or(data.next, |(before, _)| *before);
            feed = feed.child(
                button("task-activity-earlier")
                    .small()
                    .ghost()
                    .self_start()
                    .tab_stop(true)
                    .label(if data.error.is_some() {
                        "Retry"
                    } else {
                        "Load earlier activity"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.load_task_activity(in_space.clone(), task, before, cx);
                        cx.notify();
                    })),
            );
        }
        feed
    }
}

fn inspector_key(label: &'static str, cx: &App) -> Div {
    div()
        .w(TASK_DETAIL_LABEL_WIDTH)
        .flex_none()
        .text_size(TYPE_META)
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

fn inspector_kv_row(label: &'static str, value: String, muted: bool, mono: bool, cx: &App) -> Div {
    let mut val = div()
        .flex_1()
        .min_w_0()
        .text_size(TYPE_META)
        .text_color(if muted {
            cx.theme().muted_foreground
        } else {
            cx.theme().foreground
        })
        .font_features(TABULAR.clone())
        .whitespace_normal();
    if mono {
        val = val.font_family(cx.theme().mono_font_family.clone());
    }
    h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap_3()
        .child(inspector_key(label, cx))
        .child(val.child(value))
}

#[cfg(test)]
mod tests {
    use gpui_kit::px;
    use threadlane_core::client::{TaskDetail, TaskSummary};
    use threadlane_core::ids::{AgentId, LaneId, ProjectId, TaskId};
    use threadlane_core::model::{Task, TaskOrigin, TaskStatus};
    use threadlane_core::time::Timestamp;

    use super::{
        change_state_status, merge_line_status, related_task_relation, task_inspector_in_sheet,
        task_timestamp_width,
    };
    use crate::tokens::{SIDEBAR_WIDTH, Status, TASK_TIME_LONG_WIDTH, TASK_TIME_SHORT_WIDTH};

    fn dummy_detail(
        id: i64,
        depends_on: Vec<TaskId>,
        parent_task_id: Option<TaskId>,
    ) -> TaskDetail {
        TaskDetail {
            summary: TaskSummary {
                task: Task {
                    id: TaskId(id),
                    project_id: ProjectId(1),
                    lane_id: LaneId(1),
                    agent_id: AgentId(1),
                    title: "Example".into(),
                    goal: "Example".into(),
                    repositories: Vec::new(),
                    parent_task_id,
                    depends_on,
                    origin: TaskOrigin::User,
                    status: TaskStatus::Running,
                    priority: 0,
                    created_by_call: None,
                    last_machine_id: None,
                    waiting_reason: None,
                    result: None,
                    created_at: Timestamp::from_millis(0),
                    publication_blocker: None,
                    playbooks: Vec::new(),
                },
                project: "app".into(),
                agent: "Lead".into(),
                machine: None,
                active_machine_id: None,
                running_since: None,
                latest_step: None,
                completed_at: None,
            },
            branch: format!("threadlane/T-{id:04}"),
            repositories: Vec::new(),
            runs: Vec::new(),
            reviews: Vec::new(),
            decisions: Vec::new(),
            related: Vec::new(),
            messages: Vec::new(),
            proposed_by: None,
            stopped: false,
        }
    }

    #[test]
    fn inspector_docks_on_wide_windows_and_moves_to_sheet_when_narrow_or_zoomed() {
        // Default 1180px window with 220px sidebar at 16px base fits docked inspector.
        assert!(!task_inspector_in_sheet(
            px(1180.),
            px(16.),
            false,
            SIDEBAR_WIDTH
        ));
        // 1024px window with sidebar also fits (804px >= 46.5 * 16 = 744px).
        assert!(!task_inspector_in_sheet(
            px(1024.),
            px(16.),
            false,
            SIDEBAR_WIDTH
        ));
        // Narrow 900px window with sidebar moves inspector to a sheet (680px < 744px).
        assert!(task_inspector_in_sheet(
            px(900.),
            px(16.),
            false,
            SIDEBAR_WIDTH
        ));
        // Collapsing the sidebar frees enough room at 900px to dock the inspector again.
        assert!(!task_inspector_in_sheet(
            px(900.),
            px(16.),
            true,
            SIDEBAR_WIDTH
        ));
        // Minimum window width (720px) always uses the sheet even with sidebar collapsed.
        assert!(task_inspector_in_sheet(
            px(720.),
            px(16.),
            true,
            SIDEBAR_WIDTH
        ));
        // Larger base-font zoom (22px rem) switches to the sheet before crushing the main column.
        assert!(task_inspector_in_sheet(
            px(1180.),
            px(22.),
            false,
            SIDEBAR_WIDTH
        ));
    }

    #[test]
    fn timestamp_width_uses_compact_lane_for_same_day_and_expands_for_dates() {
        assert_eq!(
            task_timestamp_width(std::iter::empty()),
            TASK_TIME_SHORT_WIDTH
        );
        assert_eq!(
            task_timestamp_width(["12:16", "12:17"]),
            TASK_TIME_SHORT_WIDTH
        );
        assert_eq!(
            task_timestamp_width(["Yesterday 12:16", "12:17"]),
            TASK_TIME_LONG_WIDTH
        );
        assert_eq!(
            task_timestamp_width(["2026-11-20 09:05"]),
            TASK_TIME_LONG_WIDTH
        );
    }

    #[test]
    fn related_task_labels_distinguish_dependencies_dependents_parent_and_subtasks() {
        let detail = dummy_detail(10, vec![TaskId(2)], Some(TaskId(1)));
        assert_eq!(
            related_task_relation(&detail, TaskId(2), None),
            "Required by this task"
        );
        assert_eq!(
            related_task_relation(&detail, TaskId(1), None),
            "Parent task"
        );
        assert_eq!(
            related_task_relation(&detail, TaskId(11), Some(TaskId(10))),
            "Subtask"
        );
        assert_eq!(
            related_task_relation(&detail, TaskId(12), None),
            "Depends on this task"
        );
    }

    #[test]
    fn changes_and_merge_status_map_to_semantic_tones() {
        assert_eq!(change_state_status("Pushed"), Status::Done);
        assert_eq!(
            change_state_status("Pushed · pull request opened"),
            Status::Done
        );
        assert_eq!(
            change_state_status("Waiting on you to push (D-0012)"),
            Status::Waiting
        );
        assert_eq!(change_state_status("Pushing…"), Status::Waiting);
        assert_eq!(
            change_state_status("Not pushed: the push failed"),
            Status::Failed
        );
        assert_eq!(change_state_status("Not pushed"), Status::Idle);

        assert_eq!(
            merge_line_status("Merges cleanly into main at 1a2b3c4"),
            Status::Done
        );
        assert_eq!(
            merge_line_status("Conflicts with main: src/a.rs"),
            Status::Failed
        );
    }
}
