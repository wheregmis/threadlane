//! A task (wireframe 5): who took it, on which machine, what happened, the
//! decision it waits on, the tasks around it, a composer to its lane, and
//! Cancel. Tasks have no paused state in the hub, so there is no Pause.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Duration;
use threadlane_core::ids::TaskId;

use gpui_kit::component::Icon;
use threadlane_desktop::{
    Consequence, EmptyRegion, LatestStep, MilestoneKind, Queued, TaskControl, When, changes,
    milestone_summary, milestones, status_label, task_header, usage,
};

use super::{empty, gap_line, link, menu_item};
use crate::components::{
    button, card, decision_scroller, heading, meta, section, status_badge, status_dot,
};
use crate::icons::AppIcon;
use crate::tokens::{
    BADGE_RADIUS, LG, MD, PANEL_WIDTH, READING_WIDTH, SM, Status, TASK_TIMELINE_BASIS,
    TIMESTAMP_WIDTH, TYPE_META, XL, XS,
};
use crate::{Desktop, Screen, Tab};

impl Desktop {
    pub(crate) fn task(
        &self,
        space: &str,
        task: TaskId,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(detail) = self.state.task(space, task) else {
            let gap = self
                .state
                .task_gap(space, task)
                .unwrap_or(threadlane_desktop::Gap::Loading);
            let pending: Vec<_> = self
                .state
                .visible_decisions()
                .map(|queued| self.decision(queued, when, cx))
                .collect();
            let content = v_flex()
                .gap(MD)
                .child(heading(format!("{space} {task}")))
                .child(gap_line(&gap, "the task", space, cx))
                .children(pending);
            return decision_scroller("task", content, &self.decision_scroll)
                .w_full()
                .max_w(READING_WIDTH)
                .self_center()
                .into_any_element();
        };
        let summary = &detail.summary;
        let archived = self.state.is_archived(space, &summary.project);
        let project = Screen::Project {
            space: space.to_owned(),
            project: summary.project.clone(),
            tab: Tab::Tasks,
        };

        let mut moments = v_flex().gap(SM).child(section("Timeline"));
        let timeline = self.state.task_timeline(space, detail, when);
        if timeline.is_empty() {
            moments = moments.child(empty(EmptyRegion::Timeline, cx));
        }
        for moment in timeline {
            let id = format!("task-{space}-{task}-{}", moment.id);
            let row = h_flex()
                .gap(MD)
                .items_start()
                .id(SharedString::from(id.clone()))
                .child(
                    meta(moment.time, cx)
                        .w(TIMESTAMP_WIDTH)
                        .flex_none()
                        .text_right(),
                )
                .child(div().flex_1().min_w_0().child(self.prose_among(
                    id,
                    space,
                    Some(&summary.project),
                    Some(&summary.task.repositories),
                    moment.text,
                    cx,
                )));
            moments = if let Some(text) = moment.message {
                let view = cx.entity().downgrade();
                moments.child(row.context_menu(move |menu, _, _| {
                    let copy = text.clone();
                    let quote = text.clone();
                    let mut menu = menu.item(menu_item(&view, "Copy text", move |this, _, cx| {
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
                moments.child(row)
            };
        }
        let mut decisions: Vec<_> = detail
            .decisions
            .iter()
            .filter(|decision| !self.state.is_pending(space, &decision.id.to_string()))
            .map(|decision| Queued {
                space: space.to_owned(),
                decision: decision.clone(),
            })
            .collect();
        decisions.extend(
            self.state
                .visible_decisions()
                .filter(|_| !archived)
                .cloned(),
        );
        decisions.sort_by_key(|queued| (queued.decision.created_at, queued.decision.id));
        let decision_cards: Vec<_> = decisions
            .iter()
            .map(|queued| {
                self.decision_display(
                    self.state.present_decision(queued, &summary.project, when),
                    cx,
                )
            })
            .collect();

        let change = changes(detail);
        let publication = self.state.task_publication(space, task);
        let mut published = card(cx)
            .p(SM)
            .child(self.copyable("copy-branch", change.branch, cx))
            .child(change.state)
            .children(publication.as_ref().map(|p| {
                let line = meta(p.line.clone(), cx);
                if p.blocked {
                    line.text_color(cx.theme().danger)
                } else {
                    line
                }
            }))
            .children(
                publication
                    .into_iter()
                    .filter(|_| !archived)
                    .flat_map(|p| p.attachable)
                    .map(|name| {
                        let in_space = space.to_owned();
                        button(SharedString::from(format!("attach-{name}")))
                            .small()
                            .ghost()
                            .icon(AppIcon::Plus)
                            .label(format!("Attach {name}"))
                            .accessibility_label(format!(
                                "Attach repository {name} to {task} so its work can be published"
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.attach_repository(in_space.clone(), task, name.clone(), cx);
                            }))
                    }),
            )
            .children(change.merge)
            .children(
                (!change.diffs.is_empty() || !self.state.task_pulls(space, task).is_empty()).then(
                    || {
                        let in_space = space.to_owned();
                        let (insertions, deletions) =
                            change.diffs.iter().fold((0, 0), |(i, d), diff| {
                                (i + diff.insertions, d + diff.deletions)
                            });
                        let stats = if change.diffs.is_empty() {
                            String::new()
                        } else {
                            format!(" (+{insertions} -{deletions})")
                        };
                        button("task-review")
                            .small()
                            .outline()
                            .icon(AppIcon::FileText)
                            .label(format!("Review changes{stats}"))
                            .accessibility_label(format!(
                                "Review {task}'s changed files full width"
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_review(in_space.clone(), task, cx);
                            }))
                    },
                ),
            );
        if let Some(url) = change.pull_request {
            let open = url.clone();
            published = published.child(
                h_flex()
                    .gap(XS)
                    .min_w_0()
                    .flex_wrap()
                    .child(self.copyable("copy-pull-request", url, cx))
                    .child(
                        button("open-pull-request")
                            .small()
                            .icon(AppIcon::ExternalLink)
                            .label("Open")
                            .accessibility_label("Open the pull request in your browser")
                            .on_click(move |_, _, cx| cx.open_url(&open)),
                    ),
            );
        }
        let mut related = v_flex()
            .w(PANEL_WIDTH)
            .max_w_full()
            .flex_none()
            .gap(SM)
            .child(section("Changes"))
            .child(published)
            .child(section("Related"));
        if detail.related.is_empty() {
            related = related.child(empty(EmptyRegion::Related, cx));
        }
        for other in &detail.related {
            let relation = if summary.task.depends_on.contains(&other.task.id) {
                "Required by this task"
            } else {
                "Depends on this task"
            };
            let screen = Screen::Task {
                space: space.to_owned(),
                task: other.task.id,
            };
            let status = status_label(other.task.status, other.task.waiting_reason.as_deref());
            let label = format!("{} {}", other.task.id, other.task.title);
            related = related.child(
                v_flex()
                    .gap(XS)
                    .child(
                        link(format!("related-{}", other.task.id), label.clone())
                            .accessibility_label(format!("{label}, {status}. {relation}."))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(XS)
                            .flex_wrap()
                            .child(status_badge(status, Status::task(status), cx))
                            .child(meta(relation, cx).flex_1())
                            .children(
                                threadlane_desktop::work_elapsed(other, when)
                                    .map(|elapsed| meta(elapsed, cx).text_right()),
                            ),
                    ),
            );
        }

        related = related.child(section("Usage")).child(
            card(cx).p(SM).children(
                usage(detail, when)
                    .into_iter()
                    .map(|line| meta(line, cx).text_right()),
            ),
        );

        let stop = self
            .state
            .task_stop(space, task)
            .map(|target| self.stop_button(space, target, format!("Stop {task}"), cx));
        let in_space = space.to_owned();
        let retry_space = space.to_owned();
        let control = match self.state.task_control(space, task) {
            _ if archived => None,
            TaskControl::Ended => None,
            TaskControl::Retry => Some(
                button("task-retry")
                    .small()
                    .ghost()
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
        let mut header = task_header(detail, when);
        let activity = self.live_feed(space, task, header.latest.take(), cx);
        let actions = h_flex()
            .gap(SM)
            .flex_wrap()
            .children(stop.filter(|_| !archived))
            .children(control);
        let content = v_flex()
            .gap(XL)
            .child(
                link(
                    "task-project",
                    format!("{space} / {} / {task}", summary.project),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.go(project.clone(), cx))),
            )
            .child(
                h_flex()
                    .justify_between()
                    .items_start()
                    .gap(SM)
                    .flex_wrap()
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap(XS)
                            .child(
                                h_flex()
                                    .gap(XS)
                                    .child(status_badge(
                                        header.status,
                                        Status::task(header.status),
                                        cx,
                                    ))
                                    .child(
                                        self.copyable("copy-task", task.to_string(), cx)
                                            .font_family(cx.theme().mono_font_family.clone()),
                                    ),
                            )
                            .child(heading(summary.task.title.clone()))
                            .child(meta(header.meta, cx))
                            .children(self.pull_strip(space, task, cx)),
                    )
                    .child(actions),
            )
            .children(
                self.state
                    .waiting_task(space, summary)
                    .map(|why| meta(why, cx)),
            )
            .child(
                h_flex()
                    .gap(SM)
                    .flex_wrap()
                    .children(header.context.into_iter().map(|chip| {
                        Tag::secondary()
                            .small()
                            .rounded(BADGE_RADIUS)
                            .text_size(TYPE_META)
                            .child(chip)
                    })),
            )
            .child(activity)
            .child(if self.state.review(space, task).is_some() {
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(LG)
                    .child(self.review_view(space, task, cx))
                    .children(decision_cards)
                    .into_any_element()
            } else {
                h_flex()
                    .items_start()
                    .flex_wrap()
                    .gap(LG)
                    .child(
                        v_flex()
                            .flex_1()
                            .flex_basis(TASK_TIMELINE_BASIS)
                            .min_w_0()
                            .gap(LG)
                            .child(moments)
                            .children(decision_cards),
                    )
                    .child(related)
                    .into_any_element()
            });
        let composer = (!archived).then(|| {
            v_flex()
                .flex_none()
                .gap(XS)
                .child(self.render_composer(
                    threadlane_desktop::Draft::Task,
                    "Add to this task",
                    None,
                    cx,
                ))
                .child(meta(self.state.task_hint(space, task), cx))
        });
        // The task view scrolls above a composer pinned at the bottom, as in project chat and Home.
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            // The open review uses the window's full width.
            .when(self.state.review(space, task).is_none(), |view| {
                view.max_w(READING_WIDTH)
            })
            .self_center()
            .gap(SM)
            .child(decision_scroller("task", content, &self.decision_scroll))
            .children(composer)
            .into_any_element()
    }
}

/// How many milestones the live feed shows before "Load earlier".
const FEED_ROWS: usize = 12;

impl Desktop {
    /// The Activity feed: while the task runs, its files, commands and tests
    /// as they happen under a live pulse, amber once the run goes quiet;
    /// afterwards one summary line that opens the same feed.
    fn live_feed(
        &self,
        space: &str,
        task: TaskId,
        live: Option<LatestStep>,
        cx: &mut Context<Self>,
    ) -> Div {
        let data = &self.task_activity;
        let rows = milestones(data.entries.iter().rev().map(|entry| &entry.step));
        let expanded = live.is_some() || self.state.activity_open(space, task);
        let heading = h_flex()
            .gap(SM)
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
                    .children(data.loaded.then(|| meta(milestone_summary(&rows), cx)))
                    .child(
                        button("task-activity")
                            .small()
                            .ghost()
                            .label(if expanded { "Hide steps" } else { "Show steps" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.toggle_activity(&in_space, task);
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
        };
        let mut feed = v_flex().gap(XS).items_start().child(heading);
        if !expanded {
            return feed;
        }
        if data.loading && !data.loaded {
            feed = feed.child(meta("Loading activity…", cx));
        } else if data.is_empty() {
            feed = feed.child(empty(EmptyRegion::Activity, cx));
        } else if rows.is_empty() && data.error.is_none() {
            feed = feed.child(meta("No files or commands yet.", cx));
        }
        if !data.loading && data.newest.is_some() {
            feed = feed.child(
                button("task-activity-newest")
                    .small()
                    .ghost()
                    .label("Show newest activity")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.task_activity.show_newest();
                        cx.notify();
                    })),
            );
        }
        let when = self.when();
        let mono = cx.theme().mono_font_family.clone();
        let shown = if live.is_some() {
            FEED_ROWS
        } else {
            rows.len()
        };
        feed = feed.children(rows.iter().rev().take(shown).map(|row| {
            let (icon, status, verb) = match row.kind {
                MilestoneKind::File => (AppIcon::FileText, Status::Idle, "Changed"),
                MilestoneKind::Command => (AppIcon::ChevronRight, Status::Idle, "Ran"),
                MilestoneKind::TestsPassed => (AppIcon::Check, Status::Done, "Tests passed"),
                MilestoneKind::TestsFailed => (AppIcon::X, Status::Failed, "Tests failed"),
                MilestoneKind::Failed => (AppIcon::X, Status::Failed, "Failed"),
            };
            let ink = status.colors(cx.theme()).0;
            h_flex()
                .gap(SM)
                .items_center()
                .min_w_0()
                .child(
                    meta(when.moment(row.at), cx)
                        .w(TIMESTAMP_WIDTH)
                        .flex_none()
                        .text_right(),
                )
                .child(Icon::new(icon).small().text_color(ink).flex_none())
                .child(meta(verb, cx).flex_none())
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(TYPE_META)
                        .font_family(mono.clone())
                        .child(row.text.clone()),
                )
        }));
        if let Some((_, error)) = &data.error {
            feed = feed.child(meta(error.clone(), cx));
        }
        if !data.loading && (data.next.is_some() || data.error.is_some()) {
            let in_space = space.to_owned();
            let before = data.error.as_ref().map_or(data.next, |(before, _)| *before);
            feed = feed.child(
                button("task-activity-earlier")
                    .small()
                    .ghost()
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
