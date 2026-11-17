//! The screens, one file each, and what they share: headings and a
//! decision with its options.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_desktop::Queued;

use crate::Desktop;

mod agents;
mod home;
mod machines;
mod project;
mod shell;
mod task;

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
    /// A decision waiting on you: where it comes from, its question and
    /// context, and one button per option, the recommended one marked
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
            .child(div().text_sm().text_color(muted).child(format!(
                "{id} · {} / {project}{task}{proposal}",
                queued.space
            )))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(decision.question.clone()),
            )
            .children((!decision.context.is_empty()).then(|| {
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(decision.context.clone())
            }))
            .child(h_flex().gap_2().flex_wrap().children(options))
    }

    /// The note sent with the next answer.
    fn note_field(&self) -> Input {
        Input::new(&self.note)
    }
}
