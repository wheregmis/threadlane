//! Captured-session navigation: all chrome comes from the shared toolkit.
use super::SessionPreview;
use gpui::{prelude::*, *};
use gpui_component::input::{MoveDown, MoveUp};
use gpui_component::WindowExt;
use threadlane_ui_kit::{
    self as kit,
    transcript::{PromptLandmark, PromptRecallStep, TranscriptRow},
};

impl SessionPreview {
    pub(super) fn clear_prompt_navigation(&mut self) {
        self.outline_open = false;
        self.outline_focus_id = None;
        self.outline_selected_id = None;
        self.prompt_recall = None;
    }
    pub(super) fn render_prompt_rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.landmarks.is_empty() {
            return Empty.into_any_element();
        }
        if self.prompt_rail.item_count() != self.landmarks.len() {
            self.prompt_rail.reset(self.landmarks.len());
        }
        if let Some(index) = kit::active_prompt_landmark(&self.landmarks, &self.transcript.list) {
            let id = &self.landmarks[index].message_id;
            if self.prompt_rail_active_id.as_ref() != Some(id) {
                self.prompt_rail.scroll_to(ListOffset {
                    item_ix: index,
                    offset_in_item: px(0.),
                });
                self.prompt_rail_active_id = Some(id.clone());
            }
        }
        let owner = cx.weak_entity();
        let content_owner = owner.clone();
        let outline = kit::conversation_outline_popover(self.outline_open, &self.outline_focus)
            .on_open_change(move |open, window, cx| {
                let _ = owner.update(cx, |host, cx| {
                    if *open {
                        host.open_outline(window, cx);
                    } else {
                        host.outline_open = false;
                        cx.notify();
                    }
                });
            })
            .content(move |_, _, cx| {
                content_owner
                    .update(cx, |host, cx| host.render_outline(cx))
                    .unwrap_or_else(|_| Empty.into_any_element())
            });
        kit::prompt_navigation_rail(
            list(
                self.prompt_rail.clone(),
                cx.processor(Self::render_prompt_tick),
            ),
            outline,
            self.landmarks.len(),
        )
        .into_any_element()
    }
    fn render_prompt_tick(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(landmark) = self.landmarks.get(index) else {
            return Empty.into_any_element();
        };
        let id = landmark.message_id.clone();
        kit::prompt_rail_tick(
            landmark,
            kit::active_prompt_landmark(&self.landmarks, &self.transcript.list) == Some(index),
            cx,
        )
        .on_click(cx.listener(move |host, _, _, cx| {
            host.clear_conversation_find();
            host.outline_focus_id = Some(id.clone());
            host.activate_outline(cx);
        }))
        .into_any_element()
    }
    fn open_outline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.outline_open || !self.chat_is_active() || window.has_active_dialog(cx) {
            return;
        }
        self.clear_conversation_find();
        self.prompt_recall = None;
        self.outline_open = true;
        if self.outline_list.item_count() != self.landmarks.len() {
            self.outline_list.reset(self.landmarks.len());
        }
        self.outline_focus_id = self
            .outline_selected_id
            .as_ref()
            .filter(|id| self.landmarks.iter().any(|entry| &entry.message_id == *id))
            .cloned()
            .or_else(|| self.landmarks.last().map(|entry| entry.message_id.clone()));
        if let Some(index) = self.outline_focus_index() {
            self.outline_list.scroll_to_reveal_item(index);
        }
        window.focus(&self.outline_focus, cx);
        cx.on_next_frame(window, |host, window, cx| {
            if host.outline_open {
                window.focus(&host.outline_focus, cx);
            }
        });
        cx.notify();
    }
    fn outline_focus_index(&self) -> Option<usize> {
        self.landmarks
            .iter()
            .position(|entry| Some(&entry.message_id) == self.outline_focus_id.as_ref())
    }
    fn handle_outline_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            cx.propagate();
            return;
        }
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => {
                self.outline_open = false;
                cx.notify();
                cx.stop_propagation();
            }
            "up" | "down" | "home" | "end" if !event.keystroke.modifiers.modified() => {
                if let Some(index) = kit::transcript::step_prompt_focus(
                    self.outline_focus_index(),
                    self.landmarks.len(),
                    key,
                ) {
                    self.outline_focus_id = Some(self.landmarks[index].message_id.clone());
                    self.outline_list.scroll_to_reveal_item(index);
                    cx.notify();
                }
                cx.stop_propagation();
            }
            "enter" | "space" if !event.keystroke.modifiers.modified() => {
                self.activate_outline(cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }
    fn activate_outline(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.outline_focus_index() else {
            return;
        };
        let landmark = &self.landmarks[index];
        if !matches!(self.transcript.rows.get(landmark.row_index), Some(TranscriptRow::Message(index))
            if self.messages.get(*index).is_some_and(|message| message.id == landmark.message_id))
        {
            return;
        }
        self.outline_selected_id = Some(landmark.message_id.clone());
        self.transcript.list.pause_following_tail();
        self.transcript.list.scroll_to(ListOffset {
            item_ix: landmark.row_index,
            offset_in_item: px(0.),
        });
        self.outline_open = false;
        cx.notify();
    }
    fn render_outline_row(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(landmark) = self.landmarks.get(index) else {
            return Empty.into_any_element();
        };
        let id = landmark.message_id.clone();
        kit::conversation_outline_row(
            landmark,
            self.outline_focus_id.as_ref() == Some(&id),
            self.outline_selected_id.as_ref() == Some(&id),
            cx,
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |host, _, _, cx| {
                host.outline_focus_id = Some(id.clone());
                host.activate_outline(cx);
            }),
        )
        .into_any_element()
    }
    fn render_outline(&mut self, cx: &mut Context<Self>) -> AnyElement {
        kit::conversation_outline_content(
            &self.outline_focus,
            self.landmarks.len(),
            self.landmarks.is_empty().then(|| "No prompts yet".into()),
            list(
                self.outline_list.clone(),
                cx.processor(Self::render_outline_row),
            )
            .w_full()
            .h_full(),
            cx,
        )
        .on_key_down(cx.listener(Self::handle_outline_key))
        .on_action(
            cx.listener(|host, _: &gpui_component::dialog::Confirm, _, cx| {
                host.activate_outline(cx);
                cx.stop_propagation();
            }),
        )
        .into_any_element()
    }
    fn recallable_prompts(&self) -> Vec<&PromptLandmark> {
        self.landmarks
            .iter()
            .filter(|entry| !entry.pending_echo && !entry.text.trim().is_empty())
            .collect()
    }
    pub(super) fn recall_unavailable_reason(&self, cx: &App) -> Option<&'static str> {
        if !self.chat_is_active() {
            Some("Switch to Chat to recall a prompt")
        } else if self.prompt_recall.is_none() && !self.input.read(cx).value().is_empty() {
            Some("Clear the composer to recall a prompt")
        } else if self.recallable_prompts().is_empty() {
            Some("No earlier prompts yet")
        } else {
            None
        }
    }
    pub(super) fn step_prompt_recall(
        &mut self,
        older: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let entries = self.recallable_prompts();
        let position = self
            .prompt_recall
            .as_ref()
            .and_then(|(id, _)| entries.iter().position(|entry| &entry.message_id == id));
        match kit::transcript::prompt_recall_step(position, entries.len(), older) {
            PromptRecallStep::Load(index) => {
                let entry = entries[index];
                let text = entry.text.clone();
                let id = entry.message_id.clone();
                let len = text.len();
                self.prompt_recall = Some((id, text.clone()));
                self.input.update(cx, |input, cx| {
                    input.set_value(text, window, cx);
                    let caret = if older { 0 } else { len };
                    input.set_selected_range(caret..caret, cx);
                    input.focus(window, cx);
                });
            }
            PromptRecallStep::Clear => {
                self.prompt_recall = None;
                self.input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
            }
            PromptRecallStep::PassThrough => return false,
        }
        cx.notify();
        true
    }
    fn recall_arrow(&mut self, older: bool, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.read(cx);
        let value = input.value();
        let focused = input.focus_handle(cx).is_focused(window);
        let eligible = if let Some((_, text)) = &self.prompt_recall {
            value.as_str() == text
                && input.selected_range().is_empty()
                && input.cursor() == if older { 0 } else { value.len() }
        } else {
            older && value.is_empty()
        };
        if focused
            && eligible
            && self.chat_is_active()
            && !window.has_active_dialog(cx)
            && self.step_prompt_recall(older, window, cx)
        {
            return;
        }
        let focus = self.input.read(cx).focus_handle(cx);
        if older {
            focus.dispatch_action(&MoveUp, window, cx);
        } else {
            focus.dispatch_action(&MoveDown, window, cx);
        }
    }
    pub(super) fn recall_older_prompt_action(
        &mut self,
        _: &kit::RecallOlderPrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.recall_arrow(true, window, cx);
    }
    pub(super) fn recall_newer_prompt_action(
        &mut self,
        _: &kit::RecallNewerPrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.recall_arrow(false, window, cx);
    }
    pub(super) fn render_prompt_recall_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, _) = self.prompt_recall.as_ref()?;
        let entries = self.recallable_prompts();
        let position = entries.iter().position(|entry| &entry.message_id == id)?;
        Some(
            kit::prompt_recall_strip(
                position,
                entries.len(),
                kit::recall_older_button(position == 0).on_click(cx.listener(
                    |host, _, window, cx| {
                        host.step_prompt_recall(true, window, cx);
                    },
                )),
                kit::recall_newer_button().on_click(cx.listener(|host, _, window, cx| {
                    host.step_prompt_recall(false, window, cx);
                })),
                cx,
            )
            .into_any_element(),
        )
    }
}
