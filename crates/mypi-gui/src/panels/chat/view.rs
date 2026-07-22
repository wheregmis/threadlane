//! Chat panel main view & transcript list widget.

use super::state::{truncate_chars, ChatMessage, MsgRole, StreamingKind};
use crate::workspace::AppState;
use makepad_widgets::*;

#[derive(Script, ScriptHook, Widget)]
pub struct ChatList {
    #[deref]
    view: View,
}

impl Widget for ChatList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(data) = scope
            .data
            .get::<AppState>()
            .and_then(AppState::active_workspace)
            .map(|workspace| workspace.chat.clone())
        else {
            return DrawStep::done();
        };

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let msg_count = data.messages.len();
                let has_streaming_tail =
                    data.streaming_kind.is_some() && !data.streaming_text.is_empty();
                let items_len = msg_count + has_streaming_tail as usize;
                list.set_item_range(cx, 0, items_len);

                while let Some(item_id) = list.next_visible_item(cx) {
                    if has_streaming_tail && item_id == msg_count {
                        let template = match data.streaming_kind {
                            Some(StreamingKind::Thinking) => id!(ThinkingMsg),
                            _ => id!(AssistantMsg),
                        };
                        let item_widget = list.item(cx, item_id, template);
                        item_widget
                            .markdown(cx, ids!(md))
                            .set_text(cx, &data.streaming_text);
                        if data.streaming_kind == Some(StreamingKind::Thinking) {
                            item_widget
                                .label(cx, ids!(preview_lbl))
                                .set_text(cx, &truncate_chars(&data.streaming_text, 72));
                        }
                        item_widget.draw_all_unscoped(cx);
                        continue;
                    }

                    if let Some(msg) = data.messages.get(item_id) {
                        match msg {
                            ChatMessage::Text { role, text } => match role {
                                MsgRole::User => {
                                    let item_widget = list.item(cx, item_id, id!(UserMsg));
                                    item_widget.markdown(cx, ids!(md)).set_text(cx, text);
                                    item_widget.draw_all_unscoped(cx);
                                }
                                MsgRole::Assistant => {
                                    let item_widget = list.item(cx, item_id, id!(AssistantMsg));
                                    item_widget.markdown(cx, ids!(md)).set_text(cx, text);
                                    item_widget.draw_all_unscoped(cx);
                                }
                                MsgRole::System => {
                                    let item_widget = list.item(cx, item_id, id!(SystemMsg));
                                    item_widget.label(cx, ids!(lbl)).set_text(cx, text);
                                    item_widget.draw_all_unscoped(cx);
                                }
                            },
                            ChatMessage::Thinking { text } => {
                                let item_widget = list.item(cx, item_id, id!(ThinkingMsg));
                                item_widget.markdown(cx, ids!(md)).set_text(cx, text);
                                item_widget
                                    .label(cx, ids!(preview_lbl))
                                    .set_text(cx, &truncate_chars(text, 72));
                                item_widget.draw_all_unscoped(cx);
                            }
                            ChatMessage::Tool {
                                presentation,
                                result_preview,
                                result_metadata,
                                ..
                            } => {
                                let item_widget = list.item(cx, item_id, id!(ToolMsg));
                                item_widget
                                    .label(cx, ids!(icon_lbl))
                                    .set_text(cx, &presentation.icon);
                                item_widget
                                    .label(cx, ids!(title_lbl))
                                    .set_text(cx, &presentation.title);
                                item_widget
                                    .label(cx, ids!(meta_lbl))
                                    .set_text(cx, &presentation.metadata);
                                item_widget
                                    .widget(cx, ids!(meta_lbl))
                                    .set_visible(cx, !presentation.metadata.is_empty());
                                item_widget
                                    .label(cx, ids!(preview_lbl))
                                    .set_text(cx, &presentation.primary);
                                item_widget
                                    .label(cx, ids!(result_meta_lbl))
                                    .set_text(cx, result_metadata);
                                item_widget
                                    .widget(cx, ids!(args_section))
                                    .label(cx, ids!(content_lbl))
                                    .set_text(cx, &presentation.arguments_detail);
                                item_widget
                                    .widget(cx, ids!(args_section))
                                    .set_visible(cx, !presentation.arguments_detail.is_empty());
                                item_widget
                                    .widget(cx, ids!(result_section))
                                    .label(cx, ids!(content_lbl))
                                    .set_text(cx, result_preview);
                                item_widget
                                    .widget(cx, ids!(result_section))
                                    .set_visible(cx, !result_preview.is_empty());
                                item_widget.draw_all_unscoped(cx);
                            }
                        }
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}
