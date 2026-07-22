//! Custom list widgets: chat transcript, plan items, and sessions sidebar.
//!
//! Each wraps a `PortalList` (templates are defined in `app.rs`'s
//! `script_mod!`) and reads its rows from the shared state in `state.rs`
//! during the draw pass — same pattern as makepad's aichat example.

use crate::state::{
    relative_time_label, ChatMessage, MsgRole, SessionListRow, StreamingKind, CHAT_DATA, PLAN_DATA,
    SESSIONS_DATA,
};
use makepad_widgets::*;
use makepad_widgets::fold_button::FoldButtonAction;

// FoldHeader's stock action check only tests whether the action widget exists
// somewhere in the widget tree. With PortalList rows that makes every row
// respond to the same FoldButton action. This row-local variant additionally
// verifies that the button belongs to this header.
#[derive(Script, ScriptHook, Widget, Animator)]
pub struct ToolFoldHeader {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[rust]
    draw_state: DrawStateWrap<ToolFoldDrawState>,
    #[rust]
    rect_size: f64,
    #[rust]
    area: Area,
    #[find]
    #[redraw]
    #[live]
    header: WidgetRef,
    #[find]
    #[redraw]
    #[live]
    body: WidgetRef,
    #[apply_default]
    animator: Animator,
    #[live]
    opened: f64,
    #[layout]
    layout: Layout,
    #[walk]
    walk: Walk,
    #[live]
    body_walk: Walk,
}

#[derive(Clone)]
enum ToolFoldDrawState { DrawHeader, DrawBody }

impl Widget for ToolFoldHeader {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.animator_handle_event(cx, event).must_redraw() { self.area.redraw(cx); }
        self.header.handle_event(cx, event, scope);
        self.body.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let header_uid = self.header.widget_uid();
            let header_path = cx.widget_tree().path_to(header_uid);
            for action in actions {
                if let Some(widget_action) = action.downcast_ref::<WidgetAction>() {
                    let action_path = cx.widget_tree().path_to(widget_action.widget_uid);
                    let belongs_to_header = !header_path.is_empty()
                        && action_path.starts_with(&header_path);
                    if belongs_to_header {
                        match widget_action.cast::<FoldButtonAction>() {
                            FoldButtonAction::Opening => self.animator_play(cx, ids!(active.on)),
                            FoldButtonAction::Closing => self.animator_play(cx, ids!(active.off)),
                            _ => (),
                        }
                    }
                }
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.draw_state.begin(cx, ToolFoldDrawState::DrawHeader) { cx.begin_turtle(walk, self.layout); }
        if let Some(ToolFoldDrawState::DrawHeader) = self.draw_state.get() {
            let header_walk = self.header.walk(cx);
            self.header.draw_walk(cx, scope, header_walk)?;
            let (body_walk, scroll_y) = if self.rect_size == 0.0 {
                (self.body_walk, 0.0)
            } else {
                (Walk { height: Size::Fixed(self.rect_size * self.opened), ..self.body_walk },
                 self.rect_size * (1.0 - self.opened))
            };
            cx.begin_turtle(body_walk, Layout::flow_down().with_scroll(dvec2(0.0, scroll_y)));
            self.draw_state.set(ToolFoldDrawState::DrawBody);
        }
        if let Some(ToolFoldDrawState::DrawBody) = self.draw_state.get() {
            let body_walk = self.body.walk(cx);
            self.body.draw_walk(cx, scope, body_walk)?;
            let used_y = cx.turtle().used().y;
            if used_y > 0.0 { self.rect_size = used_y; }
            cx.end_turtle(); cx.end_turtle_with_area(&mut self.area); self.draw_state.end();
        }
        DrawStep::done()
    }
}

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
                let has_streaming_tail =
                    data.streaming_kind.is_some() && !data.streaming_text.is_empty();
                let items_len = msg_count + has_streaming_tail as usize;
                list.set_item_range(cx, 0, items_len);

                while let Some(item_id) = list.next_visible_item(cx) {
                    // The in-progress assistant response streams as a virtual
                    // tail item; it becomes a real message once flushed.
                    if has_streaming_tail && item_id == msg_count {
                        let template = match data.streaming_kind {
                            Some(StreamingKind::Thinking) => id!(ThinkingMsg),
                            _ => id!(AssistantMsg),
                        };
                        let item_widget = list.item(cx, item_id, template);
                        item_widget
                            .markdown(cx, ids!(md))
                            .set_text(cx, &data.streaming_text);
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
                                item_widget.draw_all_unscoped(cx);
                            }
                            ChatMessage::Tool {
                                status,
                                presentation,
                                result_preview,
                                result_metadata,
                                ..
                            } => {
                                let item_widget = list.item(cx, item_id, id!(ToolMsg));
                                item_widget
                                    .label(cx, ids!(status_lbl))
                                    .set_text(cx, status.glyph());
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
                                    .label(cx, ids!(args_lbl))
                                    .set_text(cx, &presentation.arguments_detail);
                                item_widget
                                    .widget(cx, ids!(args_section))
                                    .set_visible(cx, !presentation.arguments_detail.is_empty());
                                item_widget
                                    .label(cx, ids!(result_lbl))
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
                        item_widget
                            .label(cx, ids!(status_lbl))
                            .set_text(cx, if plan_item.completed { "✓" } else { "○" });
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
// SessionList — project groups + session rows
// ---------------------------------------------------------------------------

#[derive(Script, ScriptHook, Widget)]
pub struct SessionList {
    #[deref]
    view: View,
}

impl Widget for SessionList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let data = SESSIONS_DATA.read().unwrap();

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let rows = data.rows.len().max(1);
                list.set_item_range(cx, 0, rows);

                while let Some(item_id) = list.next_visible_item(cx) {
                    if data.rows.is_empty() {
                        let item_widget = list.item(cx, item_id, id!(EmptyRow));
                        item_widget
                            .label(cx, ids!(lbl))
                            .set_text(cx, "No agents yet");
                        item_widget.draw_all_unscoped(cx);
                        continue;
                    }

                    match data.rows.get(item_id) {
                        Some(SessionListRow::ProjectHeader { project_idx }) => {
                            let item_widget = list.item(cx, item_id, id!(ProjectHeader));
                            let name = data
                                .projects
                                .get(*project_idx)
                                .map(|p| p.name.as_str())
                                .unwrap_or("project");
                            item_widget.label(cx, ids!(name_lbl)).set_text(cx, name);
                            item_widget.draw_all_unscoped(cx);
                        }
                        Some(SessionListRow::EmptyProject) => {
                            let item_widget = list.item(cx, item_id, id!(EmptyRow));
                            item_widget
                                .label(cx, ids!(lbl))
                                .set_text(cx, "No agents yet");
                            item_widget.draw_all_unscoped(cx);
                        }
                        Some(SessionListRow::Session {
                            project_idx,
                            session_idx,
                        }) => {
                            let Some(project) = data.projects.get(*project_idx) else {
                                continue;
                            };
                            let Some(session) = project.sessions.get(*session_idx) else {
                                continue;
                            };
                            let active = data.active_session_id.as_deref()
                                == Some(session.id.as_str())
                                && data.active_work_dir == session.work_dir;
                            let template = if active {
                                id!(SessionRowActive)
                            } else {
                                id!(SessionRow)
                            };
                            let item_widget = list.item(cx, item_id, template);
                            item_widget
                                .label(cx, ids!(title_lbl))
                                .set_text(cx, &session.title);
                            item_widget
                                .label(cx, ids!(time_lbl))
                                .set_text(cx, &relative_time_label(session.updated_at));
                            item_widget.draw_all_unscoped(cx);
                        }
                        None => {}
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
