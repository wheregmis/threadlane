//! The screens, one file each, and what they share: headings and a
//! decision with its options.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_desktop::{AppState, Gap, Queued};

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
        Gap::Loading => div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(format!("Loading {what}…")),
        Gap::Failed(why) => div()
            .text_sm()
            .text_color(cx.theme().danger)
            .child(format!("Could not load {what}: {why}")),
    }
}

/// A row that opens something: a full-width text button named by its
/// label, whose text may wrap.
fn link(id: impl Into<SharedString>, label: String) -> Button {
    Button::new(id.into())
        .text()
        .w_full()
        .accessibility_label(label.clone())
        .child(div().flex_1().min_w_0().whitespace_normal().child(label))
}

/// A screen's title.
fn heading(text: impl Into<SharedString>) -> Div {
    div()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

/// The title of a part of a screen.
fn section(text: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
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
        Button::new("stop-run")
            .small()
            .outline()
            .tab_stop(true)
            .disabled(pending)
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
            .gap_1()
            .items_start()
            .child(
                Button::new(SharedString::from(format!("digest-{space}-{seq}")))
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
                let mut row = v_flex().gap_1().pl_3().min_w_0().items_start();
                if let Some(project) = result.project {
                    let screen = Screen::Project {
                        space: space.clone(),
                        project: project.clone(),
                        tab: crate::Tab::Conversation,
                    };
                    row = row.child(
                        Button::new(SharedString::from(format!(
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
                    row = row.child(div().text_sm().child(space.clone()));
                }
                row.child(div().text_sm().child(result.text))
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
        Button::new(id.into())
            .ghost()
            .xsmall()
            .label(text.clone())
            .accessibility_label(format!("Copy {text}"))
            .on_click(cx.listener(move |this, _, _, cx| this.copy(text.clone(), cx)))
    }

    /// A decision waiting on you: where it comes from, its question, why it
    /// is asked, the call's arguments, and one button per option, the recommended one marked
    /// (Plan.md §9.4). A click answers with that option and the note.
    fn decision(&self, queued: &Queued, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let decision = &queued.decision;
        let project = self
            .state
            .project_name(&queued.space, decision.project_id)
            .unwrap_or("?");
        let task = decision
            .task_id
            .map_or_else(String::new, |task| format!(" · {task}"));
        let card = AppState::decision_card(queued);
        let id = decision.id.to_string();
        let note = window.use_keyed_state(
            SharedString::from(format!("note-{}-{id}", queued.space)),
            cx,
            InputState::new,
        );
        let copy_id = self.copyable(format!("copy-{}-{id}", queued.space), id.clone(), cx);
        let options = decision.options.iter().enumerate().map(|(index, option)| {
            let (space, id, label) = (queued.space.clone(), id.clone(), option.label.clone());
            let button = Button::new(SharedString::from(format!("answer-{space}-{id}-{index}")));
            let button = if card.recommended == Some(index) {
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
        v_flex()
            .gap_1()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .when(card.dashed, |card| card.border_dashed())
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(copy_id)
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(format!("· {} / {project}{task}", queued.space)),
                    )
                    .children(card.chip.map(|chip| Tag::new().outline().child(chip))),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(decision.question.clone()),
            )
            .children(
                card.context
                    .map(|line| div().text_sm().text_color(muted).child(line)),
            )
            .children(
                card.details
                    .into_iter()
                    .map(|line| div().text_sm().child(line)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .py_1()
                    .child(div().text_sm().child(card.note_label))
                    .child(Input::new(&note).aria_label(card.note_label)),
            )
            .child(h_flex().gap_2().flex_wrap().children(options))
            .children(
                card.would_run_as
                    .map(|line| div().text_sm().text_color(muted).child(line)),
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
