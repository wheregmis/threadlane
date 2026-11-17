//! The screens, one file each, and what they share: headings and a
//! decision with its options.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_desktop::{Gap, Queued};

use crate::Desktop;

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
        .child(div().w_full().whitespace_normal().child(label))
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
    fn decision(&self, queued: &Queued, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let decision = &queued.decision;
        let project = self
            .state
            .project_name(&queued.space, decision.project_id)
            .unwrap_or("?");
        let task = decision
            .task_id
            .map_or_else(String::new, |task| format!(" · {task}"));
        let proposal = if decision.kind == threadlane_core::model::DecisionKind::Proposal {
            " · Proposal"
        } else {
            ""
        };
        let id = decision.id.to_string();
        let recommended = decision
            .recommended
            .and_then(|index| usize::try_from(index).ok());
        let copy_id = self.copyable(format!("copy-{}-{id}", queued.space), id.clone(), cx);
        let options = decision.options.iter().enumerate().map(|(index, option)| {
            let (space, id, label) = (queued.space.clone(), id.clone(), option.label.clone());
            let button = Button::new(SharedString::from(format!("answer-{space}-{id}-{index}")));
            let button = if recommended == Some(index) {
                button.primary().label(format!("{label} (recommended)"))
            } else {
                button.label(label.clone())
            };
            button.on_click(cx.listener(move |this, _, _, cx| {
                this.answer(space.clone(), id.clone(), label.clone(), cx);
            }))
        });
        v_flex()
            .gap_1()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(
                h_flex().gap_1().child(copy_id).child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(format!("· {} / {project}{task}{proposal}", queued.space)),
                ),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(decision.question.clone()),
            )
            .children(
                threadlane_desktop::context_line(decision)
                    .map(|line| div().text_sm().text_color(muted).child(line.to_owned())),
            )
            .children(
                threadlane_desktop::arguments(decision)
                    .into_iter()
                    .map(|line| div().text_sm().child(line)),
            )
            .child(h_flex().gap_2().flex_wrap().children(options))
    }

    /// The note sent with the next answer.
    fn note_field(&self) -> Input {
        Input::new(&self.note)
    }
}
