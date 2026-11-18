//! A task (wireframe 5): who took it, on which machine, what happened, the
//! decision it waits on, the tasks around it, a composer to its lane, and
//! Cancel. Tasks have no paused state in the hub, so there is no Pause.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::ids::TaskId;
use threadlane_desktop::{
    Queued, TaskControl, When, changes, chips, status_label, timeline, usage,
};

use super::{gap_line, link};
use crate::components::{
    button, card, composer, decision_scroller, empty_state, field, heading, meta, page_header,
    section, status_badge,
};
use crate::icons::AppIcon;
use crate::tokens::{
    BADGE_RADIUS, LG, MD, PANEL_WIDTH, READING_WIDTH, SM, Status, TASK_TIMELINE_BASIS,
    TIMESTAMP_WIDTH, TYPE_BODY, TYPE_META, XL, XS,
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
        let muted = cx.theme().muted_foreground;
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
                .child(gap_line(&gap, "this task", cx))
                .children(pending);
            return decision_scroller("task", content, &self.decision_scroll)
                .w_full()
                .max_w(READING_WIDTH)
                .self_center()
                .into_any_element();
        };
        let summary = &detail.summary;
        let project = Screen::Project {
            space: space.to_owned(),
            project: summary.project.clone(),
            tab: Tab::Tasks,
        };

        let mut moments = v_flex().gap(SM).child(section("Timeline"));
        for moment in timeline(detail, when) {
            moments = moments.child(
                h_flex()
                    .gap(MD)
                    .items_start()
                    .child(
                        meta(moment.time, cx)
                            .w(TIMESTAMP_WIDTH)
                            .flex_none()
                            .text_right(),
                    )
                    .child(div().text_size(TYPE_BODY).min_w_0().child(moment.text)),
            );
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
        decisions.extend(self.state.visible_decisions().cloned());
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
        let mut published = card(cx)
            .p(SM)
            .child(self.copyable("copy-branch", change.branch, cx))
            .child(change.state);
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
            related = related.child(empty_state("None.", None, cx));
        }
        for other in &detail.related {
            let relation = if summary.task.depends_on.contains(&other.task.id) {
                "it waits for this"
            } else {
                "waits for it"
            };
            let screen = Screen::Task {
                space: space.to_owned(),
                task: other.task.id,
            };
            let status = status_label(other.task.status, other.task.waiting_reason.as_deref());
            related = related.child(
                v_flex()
                    .gap(XS)
                    .child(
                        link(
                            format!("related-{}", other.task.id),
                            format!("{} {}", other.task.id, other.task.title),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
                    )
                    .child(
                        h_flex()
                            .gap(XS)
                            .flex_wrap()
                            .child(status_badge(status, Status::task(status), cx))
                            .child(meta(relation, cx)),
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
        let control =
            match self.state.task_control(space, task) {
                TaskControl::Ended => None,
                TaskControl::Cancel => {
                    let in_space = space.to_owned();
                    Some(
                        h_flex().child(
                            button("task-cancel")
                                .small()
                                .ghost()
                                .icon(AppIcon::X)
                                .label("Cancel")
                                .accessibility_label(format!("Cancel {task}"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.ask_cancel_task(&in_space, task);
                                    cx.notify();
                                })),
                        ),
                    )
                }
                TaskControl::Confirm(text) => {
                    let in_space = space.to_owned();
                    Some(
                        h_flex()
                            .gap(SM)
                            .flex_wrap()
                            .child(div().text_size(TYPE_BODY).child(text))
                            .child(
                                button("task-cancel-confirm")
                                    .small()
                                    .danger()
                                    .icon(AppIcon::X)
                                    .label(format!("Cancel {task}"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.cancel_task(in_space.clone(), task, cx);
                                    })),
                            )
                            .child(button("task-keep").small().label("Keep it").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.state.keep_task();
                                    cx.notify();
                                }),
                            )),
                    )
                }
            };
        let mut chips = chips(detail, when);
        let status_chip = chips.pop().map(|label| {
            let status = Status::task(&label);
            status_badge(label, status, cx)
        });
        let actions = h_flex()
            .gap(SM)
            .flex_wrap()
            .children(stop)
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
            .child(page_header(
                summary.task.title.clone(),
                Some(
                    h_flex()
                        .gap(XS)
                        .flex_wrap()
                        .child(self.copyable("copy-task", task.to_string(), cx))
                        .children(status_chip),
                ),
                Some(actions),
            ))
            .child(
                h_flex()
                    .gap(SM)
                    .flex_wrap()
                    .children(chips.into_iter().map(|chip| {
                        Tag::secondary()
                            .small()
                            .rounded(BADGE_RADIUS)
                            .text_size(TYPE_META)
                            .child(chip)
                    })),
            )
            .children(
                summary
                    .task
                    .waiting_reason
                    .clone()
                    .map(|why| div().text_size(TYPE_BODY).text_color(muted).child(why)),
            )
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
            )
            .child(
                v_flex()
                    .gap(XS)
                    .child(
                        composer(cx)
                            .child(div().flex_1().min_w_0().child(field(&self.task_composer)))
                            .child(
                                button("send-task")
                                    .primary()
                                    .icon(AppIcon::Send)
                                    .label("Send")
                                    .tooltip(crate::keyboard::send_hint(cx))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.send_to_task(window, cx)
                                    })),
                            ),
                    )
                    .child(meta(self.state.task_hint(space, task), cx)),
            );
        decision_scroller("task", content, &self.decision_scroll)
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .into_any_element()
    }
}
