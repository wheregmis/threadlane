//! Sessions panel main view & sidebar list widget.

use super::state::{relative_time_label, SessionListRow, SESSIONS_DATA};
use makepad_widgets::*;

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
                            let context_target = data.context_session_id.as_deref()
                                == Some(session.id.as_str())
                                && data.context_work_dir == session.work_dir;
                            let template = if context_target {
                                id!(SessionRowContext)
                            } else if active {
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
                            if active {
                                item_widget
                                    .widget(cx, ids!(session_row_spinner))
                                    .set_visible(cx, data.is_working);
                            }
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
