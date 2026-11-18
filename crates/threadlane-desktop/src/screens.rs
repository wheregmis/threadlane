//! The screens, one file each, and what they share: headings and a
//! decision with its options.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, v_flex};
use gpui_kit::*;
use threadlane_desktop::Gap;

use crate::components::{button, meta};
use crate::icons::AppIcon;
use crate::tokens::{MD, TYPE_BODY, XS};
use crate::{Desktop, Screen};

mod agents;
mod decision;
mod home;
mod machines;
mod new_project;
mod project;
mod routines;
mod settings;
mod shell;
mod task;

/// What a part shows instead of its contents: that it is loading, or the
/// hub's error.
fn gap_line(gap: &Gap, what: &str, cx: &App) -> Div {
    match gap {
        Gap::Loading => meta(format!("Loading {what}…"), cx),
        Gap::Failed(why) => div()
            .text_size(TYPE_BODY)
            .text_color(cx.theme().danger)
            .child(format!("Could not load {what}: {why}")),
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
                if let Some(project) = result.project {
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
