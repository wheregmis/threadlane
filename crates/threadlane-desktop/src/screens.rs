//! The screens, one file each, and what they share: headings and a
//! decision with its options.

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{Sizable as _, WindowExt as _, v_flex};
use gpui_kit::*;
use threadlane_desktop::{Consequence, EmptyRegion, Gap};

use crate::components::{button, empty_state};
use crate::icons::AppIcon;
use crate::tokens::{MD, TYPE_BODY, XS};
use crate::{Desktop, Screen};

mod agent_edit;
mod agents;
mod decision;
mod delete_project;
mod home;
pub(crate) use home::Inbox;
pub(crate) mod machines;
mod new_project;
mod project;
mod routines;
mod settings;
pub(crate) mod shell;
mod task;

/// What a part shows instead of its contents: that it is loading, or the
/// hub's error.
fn gap_line(gap: &Gap, what: &str, space: &str, cx: &mut Context<Desktop>) -> Div {
    let sentence = gap.sentence(what, space);
    let retry = sentence.detail.as_ref().map(|_| {
        button(SharedString::from(format!("retry-{space}-{what}")))
            .small()
            .outline()
            .label("Retry")
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
    });
    empty_state(sentence.text, sentence.detail.as_deref(), cx)
        .items_start()
        .children(retry)
}

fn empty(region: EmptyRegion, cx: &App) -> Div {
    empty_state(region.sentence(), None, cx)
}

fn menu_item(
    view: &WeakEntity<Desktop>,
    label: impl Into<SharedString>,
    action: impl Fn(&mut Desktop, &mut Window, &mut Context<Desktop>) + 'static,
) -> PopupMenuItem {
    let view = view.clone();
    PopupMenuItem::new(label).on_click(move |_, window, cx| {
        let _gone = view.update(cx, |this, cx| action(this, window, cx));
    })
}

impl Desktop {
    pub(crate) fn confirm_action(
        &mut self,
        action: Consequence,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let Some(copy) = self.state.confirmation(&action) else {
            self.perform_consequence(action, cx);
            return;
        };
        let destructive = matches!(
            action,
            Consequence::CancelTask(..)
                | Consequence::CancelRoutine(..)
                | Consequence::RevokeRule(..)
                | Consequence::Unpair(..)
                | Consequence::DeleteMemory(..)
        );
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let confirm = view.clone();
            let action = action.clone();
            dialog
                .confirm()
                .title(copy.title.clone())
                .description(copy.sentence.clone())
                .ok_text(copy.button)
                .ok_variant(if destructive {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Primary
                })
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let _gone =
                        confirm.update(cx, |this, cx| this.perform_consequence(action.clone(), cx));
                    true
                })
        });
    }

    fn perform_consequence(&mut self, action: Consequence, cx: &mut Context<Self>) {
        match action {
            Consequence::CancelTask(space, task) => self.cancel_task(space, task, cx),
            Consequence::CancelRoutine(space, project, timer) => {
                self.change_timer(space, project, timer, crate::TimerChange::Cancel, cx)
            }
            Consequence::RevokeRule(space, rule) => self.revoke_rule(space, rule, cx),
            Consequence::Unpair(space) => self.unpair_space(space, cx),
            Consequence::DeleteMemory(space, project, path) => {
                self.write_memory(space, project, path, None, cx)
            }
            Consequence::SetCeiling(space, Some(project), mode) => {
                self.set_ceiling(space, project, mode, cx)
            }
            Consequence::SetCeiling(space, None, mode) => self.set_space_ceiling(space, mode, cx),
            Consequence::ArchiveProject(space, project) => {
                self.set_archived(space, project, true, cx)
            }
        }
    }
}

/// A row that opens something: a full-width text button named by its
/// label, whose text may wrap.
fn link(id: impl Into<SharedString>, label: String) -> Button {
    button(id.into())
        .text()
        .w_full()
        .accessibility_label(label.clone())
        .child(div().flex_1().min_w_0().whitespace_normal().child(label))
}

impl Desktop {
    fn stop_button(
        &self,
        space: &str,
        target: threadlane_core::client::LaneTarget,
        label: String,
        cx: &mut Context<Self>,
    ) -> Button {
        use gpui_kit::component::Disableable as _;
        let pending = self.state.stopping(space, &target);
        let space = space.to_owned();
        button("stop-run")
            .small()
            .outline()
            .tab_stop(true)
            .disabled(pending)
            .icon(AppIcon::Square)
            .label(if pending {
                "Requesting stop…"
            } else {
                "Stop"
            })
            .accessibility_label(label)
            .on_click(
                cx.listener(move |this, _, _, cx| {
                    this.stop_lane(space.clone(), target.clone(), cx)
                }),
            )
    }

    fn digest_entry(
        &self,
        space: &str,
        digest: threadlane_desktop::Digest,
        cx: &mut Context<Self>,
    ) -> Div {
        let seq = digest.seq;
        let space = space.to_owned();
        let in_space = space.clone();
        v_flex()
            .w_full()
            .gap(XS)
            .items_start()
            .child(
                button(SharedString::from(format!("digest-{space}-{seq}")))
                    .small()
                    .ghost()
                    .tab_stop(true)
                    .label(digest.toggle)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.toggle_result(&in_space, seq);
                        cx.notify();
                    })),
            )
            .children(digest.results.into_iter().map(|result| {
                let mut row = v_flex().gap(XS).pl(MD).min_w_0().items_start();
                if let Some(project) = &result.project {
                    let screen = Screen::Project {
                        space: space.clone(),
                        project: project.clone(),
                        tab: crate::Tab::Conversation,
                    };
                    row = row.child(
                        button(SharedString::from(format!(
                            "digest-{space}-{seq}-{}",
                            result.id
                        )))
                        .small()
                        .ghost()
                        .tab_stop(true)
                        .label(project.clone())
                        .accessibility_label(format!("Open {project} in {space}"))
                        .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx))),
                    );
                } else {
                    row = row.child(div().text_size(TYPE_BODY).child(space.clone()));
                }
                row.w_full().child(self.prose(
                    format!("digest-body-{space}-{seq}-{}", result.id),
                    &space,
                    result.project.as_deref(),
                    result.text,
                    cx,
                ))
            }))
    }
}

impl Desktop {
    /// An id, branch or link that copies itself when clicked.
    fn copyable(
        &self,
        id: impl Into<SharedString>,
        text: String,
        cx: &mut Context<Self>,
    ) -> Button {
        button(id.into())
            .ghost()
            .small()
            .icon(AppIcon::Copy)
            .max_w_full()
            .child(div().min_w_0().truncate().child(text.clone()))
            .tooltip(format!("Copy {text}"))
            .accessibility_label(format!("Copy {text}"))
            .on_click(cx.listener(move |this, _, _, cx| this.copy(text.clone(), cx)))
    }
}
