//! A task (wireframe 5): who took it, on which machine, what happened, the
//! decision it waits on, the tasks around it, a composer to its lane, and
//! Cancel. Tasks have no paused state in the hub, so there is no Pause.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::ids::TaskId;
use threadlane_desktop::{
    Consequence, EmptyRegion, Queued, TaskControl, When, changes, status_label, task_header, usage,
};

use super::{empty, gap_line, link, menu_item};
use crate::components::{button, card, decision_scroller, heading, meta, section, status_badge};
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
            .children(change.diffs.iter().enumerate().map(|(index, diff)| {
                self.diff_preview(format!("task-{space}-{task}-{index}"), diff, cx)
            }));
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
        let header = task_header(detail, when);
        let in_space = space.to_owned();
        let expanded = self.state.activity_open(space, task);
        let mut activity = v_flex().gap(XS).items_start().child(
            button("task-activity")
                .small()
                .ghost()
                .label(if expanded {
                    "Hide activity"
                } else {
                    "Activity"
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.toggle_activity(&in_space, task);
                    if this.state.activity_open(&in_space, task) {
                        this.load_task_activity(in_space.clone(), task, None, cx);
                    }
                    cx.notify();
                })),
        );
        if expanded {
            let data = &self.task_activity;
            if data.loading && !data.loaded {
                activity = activity.child(meta("Loading activity…", cx));
            } else if data.is_empty() {
                activity = activity.child(empty(EmptyRegion::Activity, cx));
            }
            if !data.loading && data.newest.is_some() {
                activity = activity.child(
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
            for group in data
                .entries
                .chunk_by(|a, b| a.position.run == b.position.run)
            {
                let run = group[0].position.run;
                let mut title = format!("Run {run}");
                if let Some(summary) = detail.runs.iter().find(|summary| summary.run.id == run) {
                    title.push_str(&format!(" · {}", when.moment(summary.run.started_at)));
                }
                activity = activity
                    .child(section(title))
                    .children(group.iter().rev().map(|entry| {
                        meta(
                            format!(
                                "{} · {}",
                                when.moment(entry.step.at),
                                entry.step.description
                            ),
                            cx,
                        )
                    }));
            }
            if let Some((_, error)) = &data.error {
                activity = activity.child(meta(error.clone(), cx));
            }
            if !data.loading && (data.next.is_some() || data.error.is_some()) {
                let in_space = space.to_owned();
                let before = data.error.as_ref().map_or(data.next, |(before, _)| *before);
                activity = activity.child(
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
        }
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
                            .child(meta(header.meta, cx)),
                    )
                    .child(actions),
            )
            .children(
                header
                    .latest
                    .map(|latest| crate::components::live_step(latest, cx)),
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
            .child(
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
                    .child(related),
            );
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
            .max_w(READING_WIDTH)
            .self_center()
            .gap(SM)
            .child(decision_scroller("task", content, &self.decision_scroll))
            .children(composer)
            .into_any_element()
    }
}
