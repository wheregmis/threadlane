//! One decision card for Home, project conversation, and task screens.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::ContextMenuExt as _;

use super::menu_item;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_desktop::{DecisionChoice, DecisionDisplay, ProseLimit, Queued, When};

use crate::Desktop;
use crate::components::{
    button, count_badge, decision_option, decision_ring, decision_surface, decision_title, field,
    meta, status_badge,
};
use crate::keyboard::DecisionScroll;
use crate::menus::{DecisionShortcut, shortcut_hint};
use crate::tokens::{Status, XS};
use threadlane_desktop::DecisionCommand;

impl Desktop {
    pub(super) fn decision(&self, queued: &Queued, when: &When, cx: &mut Context<Self>) -> Div {
        let project = self
            .state
            .project_name(&queued.space, queued.decision.project_id)
            .unwrap_or("?");
        self.decision_display(self.state.present_decision(queued, project, when), cx)
    }

    pub(super) fn decision_display(&self, display: DecisionDisplay, cx: &mut Context<Self>) -> Div {
        let DecisionDisplay {
            key,
            question,
            body,
            options,
            origin,
            asked,
            asked_at,
            contents,
            record,
            current,
            show_note,
            always,
            answering,
        } = display;
        let menu_id = key.id;
        let menu_view = cx.entity().downgrade();
        let prefix = format!("{}-{}", key.space, key.id);
        let copy_id = self
            .copyable(format!("copy-{prefix}"), key.id.to_string(), cx)
            .font_family(cx.theme().mono_font_family.clone())
            .tab_stop(true);
        let option_rows = options
            .into_iter()
            .enumerate()
            .map(|(index, option)| {
                let choice = DecisionChoice {
                    key: key.clone(),
                    option: option.clone(),
                };
                let chosen = record
                    .as_ref()
                    .is_some_and(|record| record.chosen == Some(index));
                let hint = record
                    .is_none()
                    .then(|| shortcut_hint(cx, &DecisionShortcut(DecisionCommand::Pick(index))))
                    .flatten();
                decision_option(
                    SharedString::from(format!("option-{prefix}-{index}")),
                    option,
                    contents.recommended == Some(index),
                    chosen,
                    record.is_some() && !chosen,
                    hint.clone(),
                    cx,
                )
                .disabled(record.is_some() || answering)
                .when_some(hint, |button, hint| {
                    button.tooltip(format!(
                        "Choose this option when the card is current  {hint}"
                    ))
                })
                .on_click(
                    cx.listener(move |this, _, _, cx| this.submit_decision(choice.clone(), cx)),
                )
            })
            .collect::<Vec<_>>();
        let mut card = decision_surface(contents.dashed, cx)
            .child(
                h_flex()
                    .gap(XS)
                    .flex_wrap()
                    .children(
                        record
                            .is_none()
                            .then(|| status_badge("Needs you", Status::Waiting, cx)),
                    )
                    .child(copy_id)
                    .child(meta(origin, cx))
                    .children(contents.chip.map(|chip| count_badge(chip, cx)))
                    .child(asked_at.map_or_else(
                        || meta(asked, cx).into_any_element(),
                        |at| {
                            crate::components::Elapsed::new(format!("asked-{prefix}"), at)
                                .asked()
                                .into_any_element()
                        },
                    )),
            )
            .child(decision_title(question))
            .children(body.map(|body| {
                self.prose(format!("question-{prefix}"), &key.space, body, cx)
                    .limit(ProseLimit::Question)
            }))
            .children(contents.context.map(|line| {
                self.decision_context(format!("context-{prefix}"), &key.space, line, cx)
            }))
            .children(contents.details.into_iter().map(|line| div().child(line)))
            .children(
                contents
                    .diff
                    .as_ref()
                    .map(|diff| self.diff_preview(format!("decision-{prefix}"), diff, cx)),
            )
            .children(contents.would_run_as.map(|line| {
                self.decision_context(format!("would-run-{prefix}"), &key.space, line, cx)
            }))
            .child(v_flex().gap(XS).children(option_rows));
        if let Some(record) = record {
            card = card.child(meta(record.status, cx)).children(
                record
                    .note
                    .map(|note| self.prose(format!("note-{prefix}"), &key.space, note, cx)),
            );
        } else if show_note {
            if let Some((note, _)) = self.decision_inputs.get(&key) {
                card = card.child(v_flex().gap(XS).child(contents.note_label).child(
                    self.reveal_decision_part(
                        field(note).aria_label(contents.note_label),
                        current,
                        DecisionScroll::Note,
                    ),
                ));
            }
        } else {
            let reply = key.clone();
            card = card.child(
                button(SharedString::from(format!("instructions-{prefix}")))
                    .ghost()
                    .small()
                    .tab_stop(true)
                    .self_start()
                    .label("Other instructions…")
                    .when_some(
                        shortcut_hint(cx, &DecisionShortcut(DecisionCommand::Reply)),
                        |button, hint| button.tooltip(format!("Other instructions…  {hint}")),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.state.reveal_decision_note(reply.clone());
                        this.focus_decision_note(&reply, window, cx);
                    })),
            );
        }
        let linked = self.reveal_linked_decision.as_ref() == Some(&key);
        if let Some((label, checked)) = always {
            card = card.child(
                Checkbox::new(SharedString::from(format!("always-{prefix}")))
                    .checked(checked)
                    .label(label.clone())
                    .accessibility_label(label)
                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                        this.state.toggle_always(&key.space, key.id);
                        cx.notify();
                    })),
            );
        }
        self.reveal_decision_part(
            decision_ring(card, current, cx)
                .id(SharedString::from(format!("decision-menu-{prefix}")))
                .context_menu(move |menu, _, _| {
                    menu.item(menu_item(&menu_view, "Copy id", move |this, _, cx| {
                        this.copy(menu_id.to_string(), cx)
                    }))
                }),
            linked || (current && self.reveal_linked_decision.is_none()),
            DecisionScroll::Card,
        )
    }

    fn reveal_decision_part(
        &self,
        element: impl IntoElement,
        current: bool,
        part: DecisionScroll,
    ) -> Div {
        let scroll = self.decision_scroll.clone();
        let request = self.scroll_to_decision.clone();
        div()
            .w_full()
            .flex_none()
            .child(element)
            .when(current, |target| {
                target.on_children_prepainted(move |children, window, _| {
                    if request.get() != part {
                        return;
                    }
                    request.set(DecisionScroll::None);
                    let Some(bounds) = children.first().copied() else {
                        return;
                    };
                    let viewport = scroll.bounds();
                    let delta = if bounds.top() < viewport.top()
                        || bounds.size.height > viewport.size.height
                    {
                        viewport.top() - bounds.top()
                    } else {
                        (viewport.bottom() - bounds.bottom()).min(Pixels::ZERO)
                    };
                    if delta != Pixels::ZERO {
                        let scroll = scroll.clone();
                        window.on_next_frame(move |window, _| {
                            let offset = scroll.offset();
                            let y = (offset.y + delta).clamp(-scroll.max_offset().y, Pixels::ZERO);
                            scroll.set_offset(point(offset.x, y));
                            window.refresh();
                        });
                    }
                })
            })
    }

    fn decision_context(&self, id: String, space: &str, line: String, cx: &Context<Self>) -> Div {
        div()
            .text_color(cx.theme().muted_foreground)
            .child(self.prose(id, space, line, cx))
    }
}
