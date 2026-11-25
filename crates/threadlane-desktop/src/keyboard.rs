//! Window focus and GPUI actions for navigation and decisions.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Enter, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::ids::{EventId, RunId};
use threadlane_desktop::{
    DecisionChoice, DecisionCommand, DecisionEffect, DecisionKey, DecisionScope, Line,
};

use crate::components::{key_hint, section};
use crate::menus;
use crate::tokens::{FORM_WIDTH, MD, SCROLLBAR_GUTTER, SHORTCUTS_HEIGHT_FRACTION, SM, XS};
use crate::{Desktop, Screen, Tab};

/// Which part of the current card should be revealed after layout.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum DecisionScroll {
    #[default]
    None,
    Card,
    Note,
}

/// Stable message keys for holding the visible row still when prepending history.
#[derive(Clone, PartialEq, Eq)]
enum HistoryRow {
    Event(EventId),
    Feed(i64),
    Run(RunId),
    Decision(String),
}

impl HistoryRow {
    fn of(line: &Line) -> Option<Self> {
        match line {
            Line::You { id, .. }
            | Line::SpaceLead { id, .. }
            | Line::ProjectReply { id, .. }
            | Line::DeliveryFailure { id, .. } => Some(Self::Event(*id)),
            Line::Lead { seq, .. } | Line::Notice { seq, .. } => Some(Self::Feed(*seq)),
            Line::Digest { digest, .. } => Some(Self::Feed(digest.seq)),
            Line::Quiet { id, .. } => Some(Self::Run(*id)),
            Line::Decision { card, .. } => Some(Self::Decision(card.id.clone())),
            Line::Day(_) | Line::New | Line::Sending { .. } | Line::Stopped { .. } => None,
        }
    }
}

#[derive(Default)]
pub(super) struct HistoryScroll {
    visible: Option<(HistoryRow, Pixels)>,
    restore: Option<(HistoryRow, Pixels)>,
}

impl HistoryScroll {
    pub(super) fn preserve(&mut self) {
        self.restore = self.visible.clone();
    }

    fn measure(
        &mut self,
        rows: &[Option<HistoryRow>],
        bounds: &[Bounds<Pixels>],
        viewport: Bounds<Pixels>,
    ) -> Option<Pixels> {
        let rows = || {
            rows.iter()
                .zip(bounds)
                .filter_map(|(row, bound)| Some((row.as_ref()?, bound)))
        };
        let delta = self.restore.take().and_then(|(anchor, prior_top)| {
            rows()
                .find(|(row, _)| **row == anchor)
                .map(|(_, bound)| prior_top - (bound.top() - viewport.top()))
        });
        self.visible = rows()
            .find(|(_, bound)| bound.bottom() > viewport.top() && bound.top() < viewport.bottom())
            .map(|(row, bound)| (row.clone(), bound.top() - viewport.top()));
        delta
    }
}

impl Screen {
    pub(super) fn decision_scope(&self) -> DecisionScope {
        match self {
            Self::Home => DecisionScope::Home,
            Self::Project {
                space,
                project,
                tab: Tab::Conversation | Tab::Tasks,
            } => DecisionScope::Project(space.clone(), project.clone()),
            Self::Task { space, task } => DecisionScope::Task(space.clone(), *task),
            _ => DecisionScope::None,
        }
    }
}

impl Desktop {
    /// Keeps one input per decision so navigation preserves the draft.
    pub(super) fn prepare_decision_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.decision_inputs
            .retain(|key, _| self.state.is_pending(&key.space, &key.id.to_string()));
        if self.state.first_run() {
            return;
        }
        for queued in self.state.visible_decisions() {
            let key = DecisionKey::from(queued);
            self.decision_inputs.entry(key.clone()).or_insert_with(|| {
                let input = cx.new(|cx| InputState::new(window, cx));
                let subscription =
                    cx.subscribe_in(&input, window, move |this, input, event, window, cx| {
                        if matches!(
                            event,
                            InputEvent::PressEnter {
                                secondary: false,
                                shift: false
                            }
                        ) {
                            let note = input.read(cx).value();
                            if let Some(choice) = this.state.decision_note_enter(&key, &note) {
                                this.keyboard_focus.focus(window, cx);
                                this.submit_decision(choice, cx);
                            }
                        }
                    });
                (input, subscription)
            });
        }
    }

    pub(super) fn focus_decision_note(
        &self,
        key: &DecisionKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((input, _)) = self.decision_inputs.get(key) {
            let focus = input.focus_handle(cx);
            self.scroll_to_decision.set(DecisionScroll::Note);
            window.on_next_frame(move |window, cx| focus.focus(window, cx));
            cx.notify();
        }
    }

    fn decision_note(&self, key: &DecisionKey, cx: &App) -> Option<String> {
        let (note, _) = self.decision_inputs.get(key)?;
        let text = note.read(cx).value().trim().to_owned();
        (!text.is_empty()).then_some(text)
    }

    pub(super) fn submit_decision(&mut self, choice: DecisionChoice, cx: &mut Context<Self>) {
        let note = self.decision_note(&choice.key, cx);
        self.answer(
            choice.key.space,
            choice.key.id.to_string(),
            choice.option,
            note,
            cx,
        );
    }

    pub(super) fn decision_shortcut(
        &mut self,
        command: DecisionCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.first_run() || window.has_active_dialog(cx) {
            return;
        }
        match self
            .state
            .decision_command(command, window.has_focused_input(cx))
        {
            Some(DecisionEffect::Scroll) => {
                self.reveal_linked_decision = None;
                self.conversation_follow.set(false);
                self.scroll_to_decision.set(DecisionScroll::Card);
                cx.notify();
            }
            Some(DecisionEffect::FocusNote(key)) => self.focus_decision_note(&key, window, cx),
            Some(DecisionEffect::Answer(choice)) => self.submit_decision(choice, cx),
            Some(DecisionEffect::Confirm(choice)) => {
                self.confirm_decision_dialog(choice, window, cx)
            }
            None => {}
        }
    }

    fn confirm_decision_dialog(
        &self,
        choice: DecisionChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(queued) = self.state.current_decision() else {
            return;
        };
        let question = queued.decision.question.clone();
        let view = cx.entity().downgrade();
        let ok_label = if choice.option.chars().count() <= 24 {
            choice.option.clone()
        } else {
            "Choose option".to_owned()
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let confirm = view.clone();
            let selection = choice.clone();
            let cancel = view.clone();
            alert
                .confirm()
                .title(format!("Answer {} in {}?", choice.key.id, choice.key.space))
                .description(
                    v_flex()
                        .gap(XS)
                        .child(question.clone())
                        .child(format!("Selected option · {}", choice.option)),
                )
                .ok_text(ok_label.clone())
                .ok_variant(ButtonVariant::Primary)
                .cancel_text("Cancel")
                .on_ok(move |_, _, cx| {
                    let _gone = confirm.update(cx, |this, cx| {
                        if let Some(choice) = this.state.confirm_decision(&selection) {
                            this.submit_decision(choice, cx);
                        }
                    });
                    true
                })
                .on_cancel(move |_, _, cx| {
                    let _gone =
                        cancel.update(cx, |this, _| this.state.cancel_decision_confirmation());
                    true
                })
        });
    }

    pub(super) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.first_run() || window.has_active_dialog(cx) {
            return;
        }
        let field = match &self.screen {
            Screen::Home => return self.open_inbox_composer(window, cx),
            Screen::Task { space, task } if self.state.task(space, *task).is_some() => {
                self.decision_scroll.scroll_to_bottom();
                &self.task_composer
            }
            Screen::Project {
                space,
                project,
                tab: Tab::Tasks,
            } if !self.state.is_archived(space, project) => {
                let target = Screen::Project {
                    space: space.clone(),
                    project: project.clone(),
                    tab: Tab::Conversation,
                };
                self.go(target.clone(), cx);
                let desktop = cx.entity().downgrade();
                window.on_next_frame(move |window, cx| {
                    let _gone = desktop.update(cx, |this, cx| {
                        if this.screen == target {
                            this.focus_composer(window, cx);
                        }
                    });
                });
                return;
            }
            Screen::Project {
                space,
                project,
                tab: Tab::Conversation,
            } if !self.state.is_archived(space, project) => &self.composer,
            _ => return,
        };
        self.scroll_to_decision.set(DecisionScroll::None);
        field.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(crate) fn jump_to_latest(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.screen,
            Screen::Project {
                tab: Tab::Conversation,
                ..
            }
        ) {
            self.reveal_linked_decision = None;
            self.scroll_to_decision.set(DecisionScroll::None);
            self.conversation_follow.set(true);
            cx.notify();
        }
    }

    pub(crate) fn conversation_scroller(
        &self,
        space: &str,
        project: &str,
        lines: Div,
        conversation: &[Line],
        has_header: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let rows: Vec<_> = conversation.iter().map(HistoryRow::of).collect();
        let history = self.history_scroll.clone();
        let scroll = self.decision_scroll.clone();
        let previous_max = scroll.max_offset().y;
        let was_at_bottom = self.conversation_at_bottom;
        let follow_growth = self.reveal_linked_decision.is_none()
            && previous_max + scroll.offset().y <= crate::tokens::SCROLL_END_TOLERANCE;
        let unread = self.state.conversation_has_newer(space, project, false);
        let follow = self.conversation_follow.clone();
        let current_visit = follow.clone();
        let desktop = cx.entity().downgrade();
        let (in_space, named) = (space.to_owned(), project.to_owned());
        let lines = lines.on_children_prepainted(move |bounds, window, _| {
            // When present, the first child is the history control/status.
            let viewport = scroll.bounds();
            if let Some(delta) = history.borrow_mut().measure(
                &rows,
                bounds.get(usize::from(has_header)..).unwrap_or_default(),
                viewport,
            ) {
                follow.set(false);
                let scroll = scroll.clone();
                let desktop = desktop.clone();
                let visit = current_visit.clone();
                let measured = scroll.offset();
                window.on_next_frame(move |window, cx| {
                    let _ = desktop.update(cx, |this, _| {
                        if std::rc::Rc::ptr_eq(&this.conversation_follow, &visit)
                            && scroll.offset() == measured
                        {
                            let y =
                                (measured.y + delta).clamp(-scroll.max_offset().y, Pixels::ZERO);
                            scroll.set_offset(point(measured.x, y));
                            window.refresh();
                        }
                    });
                });
                return;
            }
            let max = scroll.max_offset().y;
            let at_bottom = max + scroll.offset().y <= crate::tokens::SCROLL_END_TOLERANCE;
            let jump = threadlane_desktop::follow_latest(
                follow.replace(false),
                follow_growth,
                max > previous_max,
            );
            if jump && !at_bottom || at_bottom != was_at_bottom || at_bottom && unread {
                let scroll = scroll.clone();
                let desktop = desktop.clone();
                let visit = current_visit.clone();
                let (space, project) = (in_space.clone(), named.clone());
                window.on_next_frame(move |window, cx| {
                    let _gone = desktop.update(cx, |this, cx| {
                        if !std::rc::Rc::ptr_eq(&this.conversation_follow, &visit) {
                            return;
                        }
                        if jump && !at_bottom {
                            scroll.scroll_to_bottom();
                            window.refresh();
                        }
                        if !jump && this.conversation_at_bottom != at_bottom {
                            this.conversation_at_bottom = at_bottom;
                            cx.notify();
                        }
                        if at_bottom {
                            this.state.saw_conversation_latest(&space, &project);
                        }
                    });
                });
            }
        });
        let show_jump =
            self.state
                .conversation_has_newer(space, project, self.conversation_at_bottom);
        crate::components::decision_scroller(
            "conversation",
            div().w_full().child(lines),
            &self.decision_scroll,
        )
        .when(show_jump, |viewport| {
            viewport.child(
                div()
                    .absolute()
                    .bottom(MD)
                    .w_full()
                    .pr(SCROLLBAR_GUTTER)
                    .flex()
                    .justify_center()
                    .child(
                        crate::components::button("jump-latest")
                            .small()
                            .rounded_full()
                            .tab_stop(true)
                            .icon(crate::icons::AppIcon::ArrowDown)
                            .label("Jump to latest")
                            .on_click(cx.listener(|this, _, _, cx| this.jump_to_latest(cx))),
                    ),
            )
        })
    }

    pub(super) fn show_shortcuts(&self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        window.open_dialog(cx, |dialog, window, cx| {
            let mut list = v_flex().gap(SM);
            let mut current_group = "";
            for (group, label, keys) in menus::shortcut_rows(cx) {
                if current_group != group {
                    list = list.child(section(group).pt(MD));
                    current_group = group;
                }
                list = list.child(
                    h_flex()
                        .justify_between()
                        .gap(MD)
                        .child(div().min_w_0().child(label))
                        .child(
                            h_flex()
                                .flex_wrap()
                                .justify_end()
                                .gap(XS)
                                .children(keys.into_iter().map(|key| key_hint(key, cx))),
                        ),
                );
            }
            dialog.title("Keyboard shortcuts").width(FORM_WIDTH).child(
                list.max_h(window.viewport_size().height * SHORTCUTS_HEIGHT_FRACTION)
                    .pr(SCROLLBAR_GUTTER)
                    .overflow_y_scrollbar()
                    .id("shortcut-list"),
            )
        });
    }

    pub(super) fn dismiss_keyboard_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        if self.screen == Screen::Home && self.inbox.composing() {
            self.close_inbox_composer(window, cx);
            return;
        }
        if window.has_focused_input(cx) {
            self.keyboard_focus.focus(window, cx);
            self.scroll_to_decision.set(DecisionScroll::Card);
        } else if let Screen::Task { space, task } = &self.screen
            && self.state.review(space, *task).is_some()
        {
            self.state.close_review();
        } else if let Screen::Project { space, project, .. } = self.screen.clone()
            && self.state.task_form(&space, &project).is_some()
        {
            self.hide_new_task_form(&space, &project, window, cx);
            return;
        } else {
            self.scroll_to_decision.set(DecisionScroll::None);
            self.state.clear_current_decision();
        }
        cx.notify();
    }
}

/// Uses the kit's registered Enter action for the composers' existing send gesture.
pub(super) fn send_hint(cx: &App) -> String {
    menus::shortcut_hint(
        cx,
        &Enter {
            secondary: false,
            shift: false,
        },
    )
    .map_or_else(|| "Send".to_owned(), |key| format!("Send  {key}"))
}

#[cfg(test)]
mod history_scroll_tests {
    use gpui_kit::{Bounds, point, px, size};

    use super::{HistoryRow, HistoryScroll};

    #[test]
    fn reopening_earlier_history_keeps_the_same_visible_row_and_pixel_position() {
        let rect = |top, height| Bounds::new(point(px(0.), px(top)), size(px(300.), px(height)));
        let mut scroll = HistoryScroll::default();
        let viewport = rect(100., 200.);
        let shown = [None, Some(HistoryRow::Feed(2)), Some(HistoryRow::Feed(3))];
        let bounds = [rect(74., 10.), rect(90., 96.), rect(210., 96.)];
        assert_eq!(scroll.measure(&shown, &bounds, viewport), None);
        scroll.preserve();

        let prepended = [
            Some(HistoryRow::Feed(1)),
            None,
            shown[1].clone(),
            shown[2].clone(),
        ];
        let bounds = [
            rect(68., 70.),
            rect(145., 10.),
            rect(161., 96.),
            rect(281., 96.),
        ];
        assert_eq!(
            scroll.measure(&prepended, &bounds, viewport),
            Some(px(-71.))
        );
        assert_eq!(
            scroll.measure(&prepended, &bounds, viewport),
            None,
            "restore is one-shot"
        );
    }
}
