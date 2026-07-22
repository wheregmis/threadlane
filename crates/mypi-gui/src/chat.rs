//! Custom list widgets: chat message list, plan item list, activity feed.
//!
//! Each wraps a `PortalList` (templates are defined in `app.rs`'s
//! `script_mod!`) and reads its rows from the shared state in `state.rs`
//! during the draw pass — same pattern as makepad's aichat example.

use crate::state::{ACTIVITY_DATA, CHAT_DATA, PLAN_DATA};
use crate::state::MsgRole;
use makepad_widgets::*;

// ---------------------------------------------------------------------------
// ChatList — message bubbles + streaming tail
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct ChatList {
    #[deref]
    view: View,
}

impl Widget for ChatList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let data = CHAT_DATA.read().unwrap();

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let msg_count = data.messages.len();
                let items_len = msg_count + data.is_streaming as usize;
                list.set_item_range(cx, 0, items_len);

                while let Some(item_id) = list.next_visible_item(cx) {
                    // The in-progress assistant response streams as a virtual
                    // tail item; it becomes a real message once flushed.
                    if data.is_streaming && item_id == msg_count {
                        let item_widget = list.item(cx, item_id, id!(AssistantMsg));
                        let text = if data.streaming_text.is_empty() {
                            "…"
                        } else {
                            &data.streaming_text
                        };
                        item_widget.markdown(cx, ids!(md)).set_text(cx, text);
                        item_widget.draw_all_unscoped(cx);
                        continue;
                    }

                    if let Some(msg) = data.messages.get(item_id) {
                        match msg.role {
                            MsgRole::User => {
                                let item_widget = list.item(cx, item_id, id!(UserMsg));
                                item_widget.markdown(cx, ids!(md)).set_text(cx, &msg.text);
                                item_widget.draw_all_unscoped(cx);
                            }
                            MsgRole::Assistant => {
                                let item_widget = list.item(cx, item_id, id!(AssistantMsg));
                                item_widget.markdown(cx, ids!(md)).set_text(cx, &msg.text);
                                item_widget.draw_all_unscoped(cx);
                            }
                            MsgRole::System => {
                                let item_widget = list.item(cx, item_id, id!(SystemMsg));
                                item_widget.label(cx, ids!(lbl)).set_text(cx, &msg.text);
                                item_widget.draw_all_unscoped(cx);
                            }
                            MsgRole::Tool => {
                                let item_widget = list.item(cx, item_id, id!(ToolMsg));
                                item_widget.label(cx, ids!(lbl)).set_text(cx, &msg.text);
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

// ---------------------------------------------------------------------------
// PlanList — one row per plan item
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct PlanList {
    #[deref]
    view: View,
}

impl Widget for PlanList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let data = PLAN_DATA.read().unwrap();

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let rows = data.items.len().max(1);
                list.set_item_range(cx, 0, rows);

                while let Some(item_id) = list.next_visible_item(cx) {
                    if data.items.is_empty() {
                        let item_widget = list.item(cx, item_id, id!(EmptyRow));
                        let text = if data.enabled {
                            "Waiting for the planning response…"
                        } else {
                            "No active plan. Use /plan <task>."
                        };
                        item_widget.label(cx, ids!(lbl)).set_text(cx, text);
                        item_widget.draw_all_unscoped(cx);
                    } else if let Some(plan_item) = data.items.get(item_id) {
                        let item_widget = list.item(cx, item_id, id!(PlanRow));
                        item_widget.label(cx, ids!(status_lbl)).set_text(
                            cx,
                            if plan_item.completed { "✓" } else { "○" },
                        );
                        item_widget.label(cx, ids!(desc_lbl)).set_text(
                            cx,
                            &format!("{}. {}", plan_item.index, plan_item.description),
                        );
                        item_widget.draw_all_unscoped(cx);
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

// ---------------------------------------------------------------------------
// ActivityList — compact tool execution feed
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct ActivityList {
    #[deref]
    view: View,
}

impl Widget for ActivityList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let data = ACTIVITY_DATA.read().unwrap();

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let rows = data.len().max(1);
                list.set_item_range(cx, 0, rows);

                while let Some(item_id) = list.next_visible_item(cx) {
                    if data.is_empty() {
                        let item_widget = list.item(cx, item_id, id!(EmptyRow));
                        item_widget
                            .label(cx, ids!(lbl))
                            .set_text(cx, "Tool activity appears here.");
                        item_widget.draw_all_unscoped(cx);
                    } else if let Some(entry) = data.get(item_id) {
                        let item_widget = list.item(cx, item_id, id!(ActivityRow));
                        item_widget
                            .label(cx, ids!(head_lbl))
                            .set_text(cx, &format!("{} {}", entry.status.glyph(), entry.name));
                        item_widget
                            .label(cx, ids!(detail_lbl))
                            .set_text(cx, &entry.detail);
                        item_widget
                            .widget(cx, ids!(detail_lbl))
                            .set_visible(cx, !entry.detail.is_empty());
                        item_widget.draw_all_unscoped(cx);
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
