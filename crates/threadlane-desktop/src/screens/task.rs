//! A task (wireframe 5): who took it, on which machine, what happened, the
//! decision it waits on, and the tasks around it.

use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::ids::TaskId;
use threadlane_desktop::{When, chips, status_label, timeline};

use super::{gap_line, heading, link, section};
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
        let open = self.state.open_for(space, task).map(|queued| {
            v_flex()
                .gap_2()
                .child(self.note_field())
                .child(self.decision(queued, cx))
        });

        let mut related = v_flex()
            .w(px(250.))
            .flex_none()
            .gap_2()
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
            .child(heading(format!("{task} {}", summary.task.title)))
            .child(
                h_flex().gap_2().flex_wrap().children(
                    chips(detail)
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
            .into_any_element()
    }
}
