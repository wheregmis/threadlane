//! Read-only search adapter for the immutable captured conversation.
use super::SessionPreview;
use gpui::{prelude::*, *};
use gpui_component::WindowExt;
use threadlane_ui_kit::{self as kit, transcript::TranscriptRow};

impl SessionPreview {
    pub(super) fn chat_is_active(&self) -> bool {
        !self.editor_open
            && !self.gallery_open
            && !self.settings_open
            && self.github_open.is_none()
            && !self.automations_open
    }
    pub(super) fn clear_conversation_find(&mut self) {
        self.find_open = false;
        self.find_query.clear();
        self.find_results.clear();
        self.find_selected = None;
        self.find_previous_focus = None;
    }
    pub(super) fn open_conversation_find(
        &mut self,
        _: &kit::FindInConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.chat_is_active() || self.palette_open || window.has_active_dialog(cx) {
            cx.propagate();
            return;
        }
        self.outline_open = false;
        self.outline_selected_id = None;
        if !self.find_open {
            self.find_previous_focus = window.focused(cx);
            self.find_open = true;
            self.find_query.clear();
            self.find_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.refresh_conversation_find(cx);
        }
        self.find_input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn close_conversation_find(
        &mut self,
        _: &kit::CloseConversationFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.find_open || window.has_active_dialog(cx) {
            cx.propagate();
            return;
        }
        let focus = self.find_previous_focus.take();
        self.clear_conversation_find();
        if let Some(focus) = focus {
            focus.focus(window, cx);
        } else {
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn refresh_conversation_find(&mut self, cx: &mut Context<Self>) {
        self.find_results =
            kit::transcript::find_conversation_messages(&self.messages, false, &self.find_query);
        self.find_selected = None;
        self.navigate_conversation_find(false, cx);
        cx.notify();
    }
    pub(super) fn navigate_conversation_find(&mut self, previous: bool, cx: &mut Context<Self>) {
        if !self.find_open || !self.chat_is_active() {
            return;
        }
        let selected = self
            .find_results
            .iter()
            .position(|hit| Some(&hit.message_id) == self.find_selected.as_ref());
        if let Some(index) =
            kit::transcript::next_find_match(selected, self.find_results.len(), previous)
        {
            let hit = &self.find_results[index];
            if !matches!(self.transcript.rows.get(hit.row_index), Some(TranscriptRow::Message(index))
                if self.messages.get(*index).is_some_and(|message| message.id == hit.message_id))
            {
                return;
            }
            self.find_selected = Some(hit.message_id.clone());
            self.transcript.list.pause_following_tail();
            self.transcript.list.scroll_to(ListOffset {
                item_ix: hit.row_index,
                offset_in_item: px(0.),
            });
            cx.notify();
        }
    }
    pub(super) fn next_conversation_match(
        &mut self,
        _: &kit::NextConversationMatch,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_conversation_find(false, cx);
        cx.stop_propagation();
    }
    pub(super) fn previous_conversation_match(
        &mut self,
        _: &kit::PreviousConversationMatch,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_conversation_find(true, cx);
        cx.stop_propagation();
    }
    pub(super) fn seed_conversation_find(
        &mut self,
        query: String,
        message_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_conversation_find(&kit::FindInConversation, window, cx);
        if !self.find_open {
            return;
        }
        // A palette handoff may leave another page; restore focus to this chat.
        self.find_previous_focus = Some(self.input.read(cx).focus_handle(cx));
        self.find_input
            .update(cx, |input, cx| input.set_value(query.clone(), window, cx));
        self.find_query = query;
        self.refresh_conversation_find(cx);
        if let Some(index) = self
            .find_results
            .iter()
            .position(|hit| hit.message_id == message_id)
        {
            // Navigate by identity, never carry a palette row index into another surface.
            self.find_selected = Some(message_id);
            let row = self.find_results[index].row_index;
            self.transcript.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(0.),
            });
        }
        cx.notify();
    }
    pub(super) fn render_conversation_find(
        &self,
        inset: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self
            .find_results
            .iter()
            .position(|hit| Some(&hit.message_id) == self.find_selected.as_ref());
        kit::ConversationFindStrip::new(
            &self.find_input,
            kit::conversation_find_status(
                &self.find_query,
                false,
                self.find_results.len(),
                selected,
            ),
            !self.find_results.is_empty(),
        )
        .leading_inset(inset)
        .excerpt(selected.map(|index| self.find_results[index].excerpt.clone().into()))
        .render(
            cx.listener(|host, action, window, cx| match action {
                kit::ConversationFindAction::Previous => host.navigate_conversation_find(true, cx),
                kit::ConversationFindAction::Next => host.navigate_conversation_find(false, cx),
                kit::ConversationFindAction::Close => {
                    host.close_conversation_find(&kit::CloseConversationFind, window, cx)
                }
            }),
            cx,
        )
        .into_any_element()
    }
}
