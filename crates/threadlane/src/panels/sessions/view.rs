//! Sessions panel main view & sidebar list widget.

use super::state::{
    relative_time_label, ProjectGroup, SessionHealth, SessionListRow, SessionsData, SESSIONS_DATA,
};
use crate::components::project_header::ProjectHeaderAction;
use crate::components::session_row::SessionRow;
use crate::path_utils::truncate_chars;
use makepad_widgets::*;
use std::path::PathBuf;

const SCROLLBAR_HIDE_DELAY: f64 = 0.75;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FixedProject {
    project_idx: usize,
    header_row: usize,
}

#[derive(Clone, Debug, Default)]
pub enum SessionListAction {
    ToggleProject(PathBuf),
    NewSession(PathBuf),
    DetachProject(PathBuf),
    #[default]
    None,
}

fn project_header_label(project: Option<&ProjectGroup>) -> String {
    project
        .map(|project| {
            if project.available {
                truncate_chars(&project.name, 15)
            } else {
                format!("{} · Missing", truncate_chars(&project.name, 9))
            }
        })
        .unwrap_or_else(|| "project".to_string())
}

fn project_is_active(data: &SessionsData, project: Option<&ProjectGroup>) -> bool {
    data.active_session_id.is_none()
        && project.is_some_and(|project| project.work_dir == data.active_work_dir)
}

fn fixed_project_for_headers(
    rows: &[SessionListRow],
    mut header_has_passed: impl FnMut(usize) -> bool,
) -> Option<FixedProject> {
    let mut headers = rows
        .iter()
        .enumerate()
        .filter_map(|(header_row, row)| match row {
            SessionListRow::ProjectHeader { project_idx } => Some(FixedProject {
                project_idx: *project_idx,
                header_row,
            }),
            _ => None,
        });
    let mut fixed = headers.next()?;
    for candidate in headers {
        if !header_has_passed(candidate.header_row) {
            break;
        }
        fixed = candidate;
    }
    Some(fixed)
}

fn draw_empty_session_row(cx: &mut Cx2d, list: &mut PortalList, item_id: usize) {
    let item_widget = list.item(cx, item_id, id!(EmptyRow));
    item_widget
        .label(cx, ids!(lbl))
        .set_text(cx, "No sessions yet");
    item_widget.draw_all_unscoped(cx);
}

fn session_row_template(_context_target: bool, _active: bool, last: bool) -> LiveId {
    if last {
        id!(SessionRowLast)
    } else {
        id!(SessionRow)
    }
}

fn session_health_badge(health: SessionHealth) -> Option<&'static str> {
    match health {
        SessionHealth::Healthy => None,
        SessionHealth::Recovering => Some("Recovery pending"),
        SessionHealth::Warning => Some("Recovery needs attention"),
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct SessionList {
    #[deref]
    view: View,
    #[rust]
    fixed_work_dir: Option<PathBuf>,
    #[rust]
    fixed_active: bool,
    #[rust]
    list_hovered: bool,
    #[rust]
    scrolling_recently: bool,
    #[rust]
    scrollbar_revealed: bool,
    #[rust]
    scrollbar_hide_timer: Timer,
    #[rust]
    search_query: String,
    #[rust]
    fixed_tree_top: bool,
    #[rust]
    last_row_count: usize,
}

impl SessionList {
    fn set_scrollbar_revealed(&mut self, cx: &mut Cx, revealed: bool) {
        if self.scrollbar_revealed == revealed {
            return;
        }
        self.scrollbar_revealed = revealed;
        let mut list = self.view.widget(cx, ids!(list));
        if revealed {
            script_apply_eval!(cx, list, {
                use mod.prelude.widgets.*
                scroll_bar +: {
                    draw_bg +: {
                        color: theme.color_input
                        color_hover: theme.color_primary
                        color_drag: theme.color_primary
                        border_color: theme.color_transparent
                        border_color_hover: theme.color_transparent
                        border_color_drag: theme.color_transparent
                    }
                }
            });
        } else {
            script_apply_eval!(cx, list, {
                use mod.prelude.widgets.*
                scroll_bar +: {
                    draw_bg +: {
                        color: theme.color_transparent
                        color_hover: theme.color_transparent
                        color_drag: theme.color_transparent
                        border_color: theme.color_transparent
                        border_color_hover: theme.color_transparent
                        border_color_drag: theme.color_transparent
                    }
                }
            });
        }
        list.redraw(cx);
    }

    fn restart_scrollbar_hide_timer(&mut self, cx: &mut Cx) {
        if !self.scrollbar_hide_timer.is_empty() {
            cx.stop_timer(self.scrollbar_hide_timer);
        }
        self.scrollbar_hide_timer = cx.start_timeout(SCROLLBAR_HIDE_DELAY);
        self.scrolling_recently = true;
        self.set_scrollbar_revealed(cx, true);
    }

    fn fixed_row(&self, cx: &mut Cx) -> WidgetRef {
        self.view.widget(
            cx,
            if self.fixed_active {
                ids!(fixed_project_header_active)
            } else {
                ids!(fixed_project_header)
            },
        )
    }

    fn handle_fixed_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(work_dir) = self.fixed_work_dir.clone() else {
            return;
        };
        let row = self.fixed_row(cx);
        let Some(action) = actions.find_widget_action(row.widget_uid()) else {
            return;
        };
        let action = match action.cast::<ProjectHeaderAction>() {
            ProjectHeaderAction::Toggle => SessionListAction::ToggleProject(work_dir),
            ProjectHeaderAction::NewSession => SessionListAction::NewSession(work_dir),
            ProjectHeaderAction::Detach => SessionListAction::DetachProject(work_dir),
            ProjectHeaderAction::None => return,
        };
        cx.widget_action(self.widget_uid(), action);
    }

    fn configure_fixed_header(&mut self, cx: &mut Cx, data: &SessionsData) {
        let slot = self.view.widget(cx, ids!(fixed_header_slot));
        if data.projects.is_empty() {
            slot.set_visible(cx, false);
            self.fixed_work_dir = None;
            return;
        }
        slot.set_visible(cx, true);

        let fixed = {
            let list = self.view.portal_list(cx, ids!(list));
            let list_rect = self.view.widget(cx, ids!(list)).area().rect(cx);
            let first_id = list.first_id();
            fixed_project_for_headers(&data.rows, |header_row| {
                if let Some((_, item)) = list.get_item(header_row) {
                    let rect = item.area().rect(cx);
                    if rect.size.y <= 0.0 || list_rect.size.y <= 0.0 {
                        return false;
                    }
                    rect.pos.y + rect.size.y <= list_rect.pos.y + 0.5
                } else {
                    header_row < first_id
                }
            })
        };
        let Some(fixed) = fixed else {
            self.fixed_work_dir = None;
            slot.set_visible(cx, false);
            return;
        };
        let Some(project) = data.projects.get(fixed.project_idx) else {
            self.fixed_work_dir = None;
            return;
        };

        self.fixed_active = project_is_active(data, Some(project));
        self.fixed_work_dir = Some(project.work_dir.clone());
        let needs_upward_stem = fixed.project_idx > 0;
        if self.fixed_tree_top != needs_upward_stem {
            self.fixed_tree_top = needs_upward_stem;
            let mut normal = self.view.widget(cx, ids!(fixed_project_header));
            let mut active = self.view.widget(cx, ids!(fixed_project_header_active));
            if needs_upward_stem {
                script_apply_eval!(cx, normal, {
                    draw_bg +: { tree_top: #(1.0f64) }
                });
                script_apply_eval!(cx, active, {
                    draw_bg +: { tree_top: #(1.0f64) }
                });
            } else {
                script_apply_eval!(cx, normal, {
                    draw_bg +: { tree_top: #(0.0f64) }
                });
                script_apply_eval!(cx, active, {
                    draw_bg +: { tree_top: #(0.0f64) }
                });
            }
        }
        let normal = self.view.widget(cx, ids!(fixed_project_header));
        let active = self.view.widget(cx, ids!(fixed_project_header_active));
        normal.set_visible(cx, !self.fixed_active);
        active.set_visible(cx, self.fixed_active);
        let row = self.fixed_row(cx);
        row.label(cx, ids!(name_lbl))
            .set_text(cx, &project_header_label(Some(project)));
    }
}

impl Widget for SessionList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let data = SESSIONS_DATA.read().unwrap();
        if self.search_query != data.search_query {
            self.search_query.clone_from(&data.search_query);
            self.fixed_work_dir = None;
            self.view
                .portal_list(cx, ids!(list))
                .set_first_id_and_scroll(0, 0.0);
        }
        self.configure_fixed_header(cx, &data);

        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let rows = data.rows.len().max(1);
                if self.last_row_count != rows {
                    self.last_row_count = rows;
                    list.set_first_id_and_scroll(0, 0.0);
                }
                list.set_item_range(cx, 0, rows);

                while let Some(item_id) = list.next_visible_item(cx) {
                    if data.rows.is_empty() {
                        draw_empty_session_row(cx, &mut list, item_id);
                        continue;
                    }

                    match data.rows.get(item_id) {
                        Some(SessionListRow::ProjectHeader { .. }) if item_id == 0 => {
                            // The first project is represented by the real fixed
                            // header above this viewport. Keep a tiny drawn item so
                            // PortalList retains its required zero-based range.
                            list.item(cx, item_id, id!(FixedHeaderSpacer))
                                .draw_all_unscoped(cx);
                        }
                        Some(SessionListRow::ProjectHeader { project_idx }) => {
                            let project = data.projects.get(*project_idx);
                            let active = project_is_active(&data, project);
                            let template = if active {
                                id!(ProjectHeaderActive)
                            } else {
                                id!(ProjectHeader)
                            };
                            let item_widget = list.item(cx, item_id, template);
                            item_widget
                                .label(cx, ids!(name_lbl))
                                .set_text(cx, &project_header_label(project));
                            item_widget.draw_all_unscoped(cx);
                        }
                        Some(SessionListRow::EmptyProject) => {
                            draw_empty_session_row(cx, &mut list, item_id);
                        }
                        Some(SessionListRow::Overflow {
                            hidden_count,
                            showing_all,
                            ..
                        }) => {
                            let item_widget = list.item(cx, item_id, id!(SessionOverflow));
                            let text = if *showing_all {
                                "Show less".to_string()
                            } else {
                                format!("Show more · {hidden_count}")
                            };
                            item_widget
                                .button(cx, ids!(overflow_btn))
                                .set_text(cx, &text);
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
                            let active = data.is_active(&session.work_dir, &session.id);
                            let context_target =
                                data.is_context_target(&session.work_dir, &session.id);
                            let last = !data.rows[item_id + 1..].iter().any(|row| {
                                matches!(
                                    row,
                                    SessionListRow::Session {
                                        project_idx: next_project_idx,
                                        ..
                                    } if *next_project_idx == *project_idx
                                )
                            });
                            let template = session_row_template(context_target, active, last);
                            let item_widget = list.item(cx, item_id, template);
                            if let Some(mut row) = item_widget.borrow_mut::<SessionRow>() {
                                row.set_state(cx, active, context_target);
                            }
                            item_widget
                                .label(cx, ids!(title_lbl))
                                .set_text(cx, &session.title);
                            item_widget
                                .label(cx, ids!(time_lbl))
                                .set_text(cx, &relative_time_label(session.updated_at));
                            let working = data
                                .working_sessions
                                .get(&session.work_dir)
                                .is_some_and(|sessions| sessions.contains(&session.id));
                            item_widget
                                .widget(cx, ids!(session_row_spinner))
                                .set_visible(cx, working);
                            let health = data.session_health_for(&session.work_dir, &session.id);
                            let mut health_badge = item_widget.label(cx, ids!(health_lbl));
                            health_badge.set_visible(cx, health != SessionHealth::Healthy);
                            if let Some(label) = session_health_badge(health) {
                                health_badge.set_text(cx, label);
                                if health == SessionHealth::Warning {
                                    script_apply_eval!(cx, health_badge, {
                                        use mod.prelude.widgets.*
                                        draw_text +: { color: theme.color_destructive }
                                    });
                                } else {
                                    script_apply_eval!(cx, health_badge, {
                                        use mod.prelude.widgets.*
                                        draw_text +: { color: theme.color_warning }
                                    });
                                }
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
        if self.scrollbar_hide_timer.is_event(event).is_some() {
            self.scrollbar_hide_timer = Timer::empty();
            self.scrolling_recently = false;
            if !self.list_hovered {
                self.set_scrollbar_revealed(cx, false);
            }
        }

        let list_rect = self.view.widget(cx, ids!(list)).area().clipped_rect(cx);
        match event {
            Event::MouseMove(event) => {
                self.list_hovered = list_rect.contains(event.abs);
                if self.list_hovered {
                    self.set_scrollbar_revealed(cx, true);
                } else if !self.scrolling_recently {
                    self.set_scrollbar_revealed(cx, false);
                }
            }
            Event::MouseLeave(_) => {
                self.list_hovered = false;
                if !self.scrolling_recently {
                    self.set_scrollbar_revealed(cx, false);
                }
            }
            Event::Scroll(event) if list_rect.contains(event.abs) => {
                self.restart_scrollbar_hide_timer(cx);
            }
            _ => {}
        }

        // The context menu is an overlay, so list rows beneath it must not also
        // receive hover/press events while a context target is active.
        let context_menu_open = SESSIONS_DATA.read().unwrap().context_session_id.is_some();
        if context_menu_open {
            return;
        }

        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let list = self.view.portal_list(cx, ids!(list));
            if list.scrolled(actions) {
                self.restart_scrollbar_hide_timer(cx);
                self.view.redraw(cx);
            }
            self.handle_fixed_actions(cx, actions);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::sessions::state::SessionHealth;
    use std::collections::HashSet;

    fn project(project_idx: usize) -> SessionListRow {
        SessionListRow::ProjectHeader { project_idx }
    }

    fn session(project_idx: usize, session_idx: usize) -> SessionListRow {
        SessionListRow::Session {
            project_idx,
            session_idx,
        }
    }

    #[test]
    fn fixed_header_starts_with_the_first_project() {
        let rows = vec![project(0), session(0, 0), project(1), session(1, 0)];
        assert_eq!(
            fixed_project_for_headers(&rows, |_| false),
            Some(FixedProject {
                project_idx: 0,
                header_row: 0,
            })
        );
    }

    #[test]
    fn fixed_header_advances_only_past_headers_that_left_the_viewport() {
        let rows = vec![
            project(0),
            session(0, 0),
            project(1),
            SessionListRow::EmptyProject,
            project(2),
            session(2, 0),
        ];
        let passed = HashSet::from([2]);
        assert_eq!(
            fixed_project_for_headers(&rows, |row| passed.contains(&row)),
            Some(FixedProject {
                project_idx: 1,
                header_row: 2,
            })
        );

        let passed = HashSet::from([2, 4]);
        assert_eq!(
            fixed_project_for_headers(&rows, |row| passed.contains(&row)),
            Some(FixedProject {
                project_idx: 2,
                header_row: 4,
            })
        );
    }

    #[test]
    fn fixed_header_requires_contiguous_passed_project_headers() {
        let rows = vec![project(0), project(1), project(2)];
        let passed = HashSet::from([2]);
        assert_eq!(
            fixed_project_for_headers(&rows, |row| passed.contains(&row)),
            Some(FixedProject {
                project_idx: 0,
                header_row: 0,
            })
        );
        assert_eq!(fixed_project_for_headers(&[], |_| true), None);
    }

    #[test]
    fn session_template_only_distinguishes_last_connector() {
        assert_eq!(session_row_template(true, true, true), id!(SessionRowLast));
        assert_eq!(session_row_template(true, false, true), id!(SessionRowLast));
        assert_eq!(session_row_template(false, true, true), id!(SessionRowLast));
        assert_eq!(session_row_template(true, true, false), id!(SessionRow));
        assert_eq!(session_row_template(true, false, false), id!(SessionRow));
        assert_eq!(session_row_template(false, true, false), id!(SessionRow));
        assert_eq!(
            session_row_template(false, false, true),
            id!(SessionRowLast)
        );
        assert_eq!(session_row_template(false, false, false), id!(SessionRow));
    }

    #[test]
    fn session_health_badge_is_hidden_for_healthy_sessions_and_uses_text_for_issues() {
        assert_eq!(session_health_badge(SessionHealth::Healthy), None);
        assert_eq!(
            session_health_badge(SessionHealth::Recovering),
            Some("Recovery pending")
        );
        assert_eq!(
            session_health_badge(SessionHealth::Warning),
            Some("Recovery needs attention")
        );
    }
}
