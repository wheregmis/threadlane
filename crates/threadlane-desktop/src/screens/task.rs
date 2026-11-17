//! A task (wireframe 5): who took it, on which machine, what happened, the
//! decision it waits on, the tasks around it, a composer to its lane, and
//! Cancel. Tasks have no paused state in the hub, so there is no Pause.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::ids::TaskId;
use threadlane_desktop::{TaskControl, When, changes, chips, status_label, timeline, usage};

use super::{gap_line, heading, link, section};
use crate::{Desktop, Screen, Tab};

impl Desktop {
    pub(crate) fn task(
        &self,
        space: &str,
        task: TaskId,
        when: &When,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let Some(detail) = self.state.task(space, task) else {
            let gap = self
                .state
                .task_gap(space, task)
                .unwrap_or(threadlane_desktop::Gap::Loading);
            return v_flex()
                .gap_3()
                .child(heading(format!("{space} {task}")))
                .child(gap_line(&gap, "this task", cx))
                .into_any_element();
        };
        let summary = &detail.summary;
        let project = Screen::Project {
            space: space.to_owned(),
            project: summary.project.clone(),
            tab: Tab::Tasks,
        };

        let mut moments = v_flex().gap_2().child(section("Timeline"));
        for moment in timeline(detail, when) {
            moments = moments.child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .text_sm()
                            .text_color(muted)
                            .child(moment.time),
                    )
                    .child(div().text_sm().min_w_0().child(moment.text)),
            );
        }
        let open = self
            .state
            .open_for(space, task)
            .map(|queued| self.decision(queued, window, cx));

        let change = changes(detail);
        let mut published = v_flex()
            .gap_1()
            .p_2()
            .text_sm()
            .bg(cx.theme().muted)
            .rounded(cx.theme().radius)
            .child(self.copyable("copy-branch", change.branch, cx))
            .child(change.state);
        if let Some(url) = change.pull_request {
            let open = url.clone();
            published = published.child(
                h_flex()
                    .gap_1()
                    .min_w_0()
                    .child(self.copyable("copy-pull-request", url, cx))
                    .child(
                        Button::new("open-pull-request")
                            .xsmall()
                            .label("Open")
                            .accessibility_label("Open the pull request in your browser")
                            .on_click(move |_, _, cx| cx.open_url(&open)),
                    ),
            );
        }
        let mut related = v_flex()
            .w(px(250.))
            .flex_none()
            .gap_2()
            .child(section("Changes"))
            .child(published)
            .child(section("Related"));
        if detail.related.is_empty() {
            related = related.child(div().text_sm().text_color(muted).child("None."));
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
            related = related.child(
                v_flex()
                    .child(
                        link(
                            format!("related-{}", other.task.id),
                            format!("{} {}", other.task.id, other.task.title),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
                    )
                    .child(div().text_xs().text_color(muted).child(format!(
                        "{} · {relation}",
                        status_label(other.task.status, other.task.waiting_reason.as_deref())
                    ))),
            );
        }

        related = related.child(section("Usage")).child(
            v_flex()
                .gap_1()
                .p_2()
                .text_sm()
                .bg(cx.theme().muted)
                .rounded(cx.theme().radius)
                .children(usage(detail, when)),
        );

        let stop = self
            .state
            .task_stop(space, task)
            .map(|target| self.stop_button(space, target, format!("Stop {task}"), cx));
        let control = match self.state.task_control(space, task) {
            TaskControl::Ended => None,
            TaskControl::Cancel => {
                let in_space = space.to_owned();
                Some(
                    h_flex().child(
                        Button::new("task-cancel")
                            .small()
                            .ghost()
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
                        .gap_2()
                        .flex_wrap()
                        .child(div().text_sm().child(text))
                        .child(
                            Button::new("task-cancel-confirm")
                                .small()
                                .danger()
                                .label(format!("Cancel {task}"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.cancel_task(in_space.clone(), task, cx);
                                })),
                        )
                        .child(Button::new("task-keep").small().label("Keep it").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.state.keep_task();
                                cx.notify();
                            }),
                        )),
                )
            }
        };
        v_flex()
            .id("task")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .child(
                link(
                    "task-project",
                    format!("{space} / {} / {task}", summary.project),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.go(project.clone(), cx))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(self.copyable("copy-task", task.to_string(), cx))
                    .child(heading(summary.task.title.clone())),
            )
            .child(
                h_flex().gap_2().flex_wrap().children(
                    chips(detail, when)
                        .into_iter()
                        .map(|chip| Tag::secondary().child(chip)),
                ),
            )
            .children(
                summary
                    .task
                    .waiting_reason
                    .clone()
                    .map(|why| div().text_sm().text_color(muted).child(why)),
            )
            .children(stop)
            .children(control)
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_4()
                            .child(moments)
                            .children(open),
                    )
                    .child(related),
            )
            .child(
                v_flex()
                    .gap_1()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.task_composer)))
                            .child(Button::new("send-task").primary().label("Send").on_click(
                                cx.listener(|this, _, window, cx| this.send_to_task(window, cx)),
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(self.state.task_hint(space, task)),
                    ),
            )
            .into_any_element()
    }
}
