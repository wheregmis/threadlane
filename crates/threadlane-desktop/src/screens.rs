//! The screens, one file each, and what they share: headings and a
//! decision with its options.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::group_box::GroupBox;
use gpui_kit::component::input::InputState;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_desktop::{AppState, Gap, Queued};

use crate::components::{button, card, field, meta};
use crate::icons::AppIcon;
use crate::tokens::{MD, SM, TYPE_BODY, TYPE_EMPHASIS, XS};
use crate::{Desktop, Screen};

mod agents;
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
                row.child(div().text_size(TYPE_BODY).child(result.text))
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

    /// A decision waiting on you: where it comes from, its question, why it
    /// is asked, the call's arguments, and one button per option, the recommended one marked
    /// (Plan.md §9.4). A click answers with that option and the note.
    fn decision(&self, queued: &Queued, window: &mut Window, cx: &mut Context<Self>) -> GroupBox {
        let muted = cx.theme().muted_foreground;
        let decision = &queued.decision;
        let project = self
            .state
            .project_name(&queued.space, decision.project_id)
            .unwrap_or("?");
        let task = decision
            .task_id
            .map_or_else(String::new, |task| format!(" · {task}"));
        let decision_card = AppState::decision_card(queued);
        let id = decision.id.to_string();
        let note = window.use_keyed_state(
            SharedString::from(format!("note-{}-{id}", queued.space)),
            cx,
            InputState::new,
        );
        let copy_id = self.copyable(format!("copy-{}-{id}", queued.space), id.clone(), cx);
        let options = decision.options.iter().enumerate().map(|(index, option)| {
            let (space, id, label) = (queued.space.clone(), id.clone(), option.label.clone());
            let button = button(SharedString::from(format!("answer-{space}-{id}-{index}")));
            let button = if decision_card.recommended == Some(index) {
                button.primary().label(format!("{label} (recommended)"))
            } else {
                button.label(label.clone())
            };
            let note = note.clone();
            button
                .tab_stop(true)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let text = note.read(cx).value().trim().to_owned();
                    this.answer(
                        space.clone(),
                        id.clone(),
                        label.clone(),
                        (!text.is_empty()).then_some(text),
                        cx,
                    );
                }))
        });
        card(cx)
            .when(decision_card.dashed, |card| {
                card.border_dashed()
                    .border_1()
                    .border_color(cx.theme().border)
            })
            .child(
                h_flex()
                    .gap(XS)
                    .flex_wrap()
                    .child(copy_id)
                    .child(
                        div()
                            .text_size(TYPE_BODY)
                            .text_color(muted)
                            .child(format!("· {} / {project}{task}", queued.space)),
                    )
                    .children(
                        decision_card
                            .chip
                            .map(|chip| Tag::new().outline().child(chip)),
                    ),
            )
            .child(
                div()
                    .text_size(TYPE_EMPHASIS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(decision.question.clone()),
            )
            .children(
                decision_card
                    .context
                    .map(|line| div().text_size(TYPE_BODY).text_color(muted).child(line)),
            )
            .children(
                decision_card
                    .details
                    .into_iter()
                    .map(|line| div().text_size(TYPE_BODY).child(line)),
            )
            .child(
                v_flex()
                    .gap(XS)
                    .py(XS)
                    .child(div().text_size(TYPE_BODY).child(decision_card.note_label))
                    .child(field(&note).aria_label(decision_card.note_label)),
            )
            .child(h_flex().gap(SM).flex_wrap().children(options))
            .children(
                decision_card
                    .would_run_as
                    .map(|line| div().text_size(TYPE_BODY).text_color(muted).child(line)),
            )
            .children(self.state.always_offer(queued).map(|(label, ticked)| {
                let (space, id) = (queued.space.clone(), decision.id);
                Checkbox::new(SharedString::from(format!("always-{space}-{id}")))
                    .checked(ticked)
                    .label(label.clone())
                    .accessibility_label(label)
                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                        this.state.toggle_always(&space, id);
                        cx.notify();
                    }))
            }))
    }
}
