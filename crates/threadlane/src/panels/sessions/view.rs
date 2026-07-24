//! Sessions panel main view & sidebar list widget.

use super::state::{relative_time_label, SessionListRow, SESSIONS_DATA};
use crate::path_utils::{canonicalize_path, truncate_chars};
use makepad_widgets::*;

fn draw_empty_session_row(cx: &mut Cx2d, list: &mut PortalList, item_id: usize) {
    let item_widget = list.item(cx, item_id, id!(EmptyRow));
    item_widget
        .label(cx, ids!(lbl))
        .set_text(cx, "No sessions yet");
    item_widget.draw_all_unscoped(cx);
}

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
                        draw_empty_session_row(cx, &mut list, item_id);
                        continue;
                    }

                    match data.rows.get(item_id) {
                        Some(SessionListRow::ProjectHeader { project_idx }) => {
                            let project = data.projects.get(*project_idx);
                            let active = data.active_session_id.is_none()
                                && project.is_some_and(|project| {
                                    project.work_dir == data.active_work_dir
                                });
                            let template = if active {
                                id!(ProjectHeaderActive)
                            } else {
                                id!(ProjectHeader)
                            };
                            let item_widget = list.item(cx, item_id, template);
                            let name = project
                                .map(|project| {
                                    if project.available {
                                        truncate_chars(&project.name, 15)
                                    } else {
                                        format!("{} · Missing", truncate_chars(&project.name, 9))
                                    }
                                })
                                .unwrap_or_else(|| "project".to_string());
                            item_widget.label(cx, ids!(name_lbl)).set_text(cx, &name);
                            item_widget.draw_all_unscoped(cx);
                        }
                        Some(SessionListRow::EmptyProject) => {
                            draw_empty_session_row(cx, &mut list, item_id);
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
                            let active = data.is_active(&session.work_dir, &session.id);
                            let context_target = data.is_context_target(&session.work_dir, &session.id);
                            let last = *session_idx + 1 == project.sessions.len();
                            let template = match (context_target, active, last) {
                                (true, _, true) => id!(SessionRowContextLast),
                                (true, _, false) => id!(SessionRowContext),
                                (false, true, true) => id!(SessionRowActiveLast),
                                (false, true, false) => id!(SessionRowActive),
                                (false, false, true) => id!(SessionRowLast),
                                (false, false, false) => id!(SessionRow),
                            };
                            let item_widget = list.item(cx, item_id, template);
                            item_widget
                                .label(cx, ids!(title_lbl))
                                .set_text(cx, &session.title);
                            item_widget
                                .label(cx, ids!(time_lbl))
                                .set_text(cx, &relative_time_label(session.updated_at));
                            let normalized_dir = canonicalize_path(&session.work_dir);
                            let working = data
                                .working_sessions
                                .contains(&(normalized_dir, session.id.clone()));
                            item_widget
                                .widget(cx, ids!(session_row_spinner))
                                .set_visible(cx, working);
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
        // The context menu is an overlay, so list rows beneath it must not also
        // receive hover/press events while a context target is active.
        if SESSIONS_DATA.read().unwrap().context_session_id.is_some() {
            return;
        }
        self.view.handle_event(cx, event, scope);
    }
}
