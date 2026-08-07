//! Project-scoped supervisor task list shown beside the active chat.

use makepad_widgets::*;
use std::collections::HashMap;
use std::path::PathBuf;
use threadlane_agent::{PlanItemStatus, SessionPlan};
use threadlane_coding_agent::TaskStatus;

script_mod! {
    use mod.prelude.widgets.*
    use mod.components.*

    mod.components.TaskSidebarBase = #(TaskSidebar::register_widget(vm))

    mod.components.TaskSidebarRowBase = RoundedView {
        width: Fill
        height: 64
        flow: Right
        spacing: 9
        align: Align{y: 0.5}
        padding: Inset{left: 10 top: 7 right: 8 bottom: 7}
        margin: Inset{left: 8 right: 8 bottom: 4}
        cursor: MouseCursor.Hand
        draw_bg +: {
            color: theme.color_background
            border_color: theme.color_card
            border_color_hover: theme.color_primary
            border_size: 1.0
            border_radius: 8.0
        }

        status_dot := mod.components.StatusDot {
            visible: true
            draw_bg +: {
                color: theme.color_warning
            }
        }

        task_copy := View {
            width: Fill
            height: Fit
            flow: Down
            spacing: 2
            summary_lbl := mod.components.ClippedLabel {
                width: Fill
                height: 18
                padding: 0
                align: Align{y: 0.5}
                draw_text +: {
                    color: theme.color_primary_foreground
                    text_style: theme.font_bold { font_size: 9.5 }
                }
            }
            agent_lbl := mod.components.ClippedLabel {
                width: Fill
                height: 14
                padding: 0
                align: Align{y: 0.5}
                draw_text +: {
                    color: theme.color_primary
                    text_style +: { font_size: 8.5 }
                }
            }
            activity_lbl := mod.components.ClippedLabel {
                width: Fill
                height: 14
                padding: 0
                align: Align{y: 0.5}
                draw_text +: {
                    color: theme.color_primary
                    text_style: theme.font_code { font_size: 8.0 }
                }
            }
        }

        details_btn := Button {
            width: 22
            height: 22
            padding: 0
            spacing: 0
            text: "…"
            align: Align{x: 0.5 y: 0.5}
            draw_text +: {
                color: theme.color_muted_foreground
                color_hover: theme.color_primary
                color_down: theme.color_primary_foreground
            }
        }


        cancel_btn := mod.components.IconButton {
            width: 22
            height: 22
            visible: false
            icon_walk: Walk{width: 9 height: 9}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/close.svg")
                color: theme.color_muted_foreground
                color_hover: theme.color_destructive
                color_down: theme.color_primary_foreground
            }
        }

        resume_btn := mod.components.IconButton {
            width: 22
            height: 22
            visible: false
            icon_walk: Walk{width: 9 height: 9}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/refresh.svg")
                color: theme.color_muted_foreground
                color_hover: theme.color_primary
                color_down: theme.color_primary_foreground
            }
        }
    }

    mod.components.PlanSidebarRowBase = View {
        width: Fill
        height: 40
        flow: Right
        spacing: 9
        align: Align{y: 0.5}
        padding: Inset{left: 14 right: 12}

        status_dot := mod.components.StatusDot {
            visible: true
            draw_bg +: {
                color: theme.color_muted_foreground
            }
        }
        step_lbl := mod.components.ClippedLabel {
            width: Fill
            height: 20
            padding: 0
            align: Align{y: 0.5}
            draw_text +: {
                color: theme.color_card_foreground
                text_style +: { font_size: 9.0 }
            }
        }
    }

    mod.components.TaskSidebar = set_type_default() do mod.components.TaskSidebarBase {
        width: 280
        height: Fill
        flow: Down
        spacing: 0
        draw_bg +: {
            color: theme.color_background
            border_color: theme.color_card
            border_size: 1.0
            border_radius: 10.0
        }

        sidebar_header := View {
            width: Fill
            height: 42
            flow: Right
            align: Align{y: 0.5}
            padding: Inset{left: 12 right: 8}
            title_lbl := Label {
                width: Fill
                height: 22
                padding: 0
                align: Align{y: 0.5}
                text: "Plan & Agents"
                draw_text +: {
                    color: theme.color_foreground
                    text_style: theme.font_bold { font_size: 11.0 }
                }
            }
            close_btn := mod.components.IconButton {
                draw_icon +: { svg: crate_resource("self:resources/icons/close.svg") }
            }
        }

        header_rule := View {
            width: Fill
            height: 1
            show_bg: true
            draw_bg +: { color: theme.color_card }
        }

        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            drag_scrolling: true

            PlanHeader := View {
                width: Fill
                height: 36
                flow: Right
                align: Align{y: 0.5}
                padding: Inset{left: 12 top: 5 right: 12}
                plan_lbl := Label {
                    width: Fill
                    height: 18
                    padding: 0
                    align: Align{y: 0.5}
                    text: "PLAN PROGRESS"
                    draw_text +: {
                        color: theme.color_primary
                        text_style: theme.font_bold { font_size: 7.5 }
                    }
                }
                progress_lbl := Label {
                    width: Fit
                    height: 18
                    padding: 0
                    align: Align{y: 0.5}
                    draw_text +: {
                        color: theme.color_primary
                        text_style: theme.font_code { font_size: 8.0 }
                    }
                }
            }
            PlanPending := mod.components.PlanSidebarRowBase {}
            PlanInProgress := mod.components.PlanSidebarRowBase {
                status_dot +: { draw_bg +: { color: theme.color_primary } }
                step_lbl +: { draw_text +: { color: theme.color_primary } }
            }
            PlanCompleted := mod.components.PlanSidebarRowBase {
                status_dot +: { draw_bg +: { color: theme.color_success } }
                step_lbl +: { draw_text +: { color: theme.color_muted_foreground } }
            }

            SessionHeader := View {
                width: Fill
                height: 32
                padding: Inset{left: 12 top: 8 right: 12 bottom: 3}
                session_lbl := mod.components.ClippedLabel {
                    width: Fill
                    height: 18
                    padding: 0
                    align: Align{y: 0.5}
                    draw_text +: {
                        color: theme.color_primary
                        text_style: theme.font_bold { font_size: 7.5 }
                    }
                }
            }
            TaskDetail := View {
                width: Fill
                height: 38
                padding: Inset{left: 28 right: 12}
                detail_lbl := mod.components.ClippedLabel {
                    width: Fill
                    height: 30
                    padding: 0
                    align: Align{y: 0.5}
                    draw_text +: {
                        color: theme.color_muted_foreground
                        text_style: theme.font_code { font_size: 8.0 }
                    }
                }
            }


            TaskQueued := mod.components.TaskSidebarRowBase {}
            TaskRunning := mod.components.TaskSidebarRowBase {
                status_dot +: { draw_bg +: { color: theme.color_primary } }
            }
            TaskCompleted := mod.components.TaskSidebarRowBase {
                status_dot +: { draw_bg +: { color: theme.color_success } }
            }
            TaskFailed := mod.components.TaskSidebarRowBase {
                status_dot +: { draw_bg +: { color: theme.color_destructive } }
            }
            TaskCancelled := mod.components.TaskSidebarRowBase {
                status_dot +: { draw_bg +: { color: theme.color_muted_foreground } }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSidebarItem {
    pub id: String,
    pub session_id: String,
    pub session_label: String,
    pub session_file: Option<PathBuf>,
    pub agent: String,
    pub summary: String,
    pub activity: String,
    pub status: TaskStatus,
    pub cancellable: bool,
    pub resumable: bool,
    pub started_at_ms: u128,
    pub finished_at_ms: Option<u128>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TaskSidebarAction {
    Close,
    OpenSession {
        session_id: String,
        session_file: Option<PathBuf>,
    },
    Cancel(String),
    Resume(String),
    ToggleSession(String),
    ToggleTask(String),
    #[default]
    None,
}

/// The two deliberately explicit sections make the sidebar model useful to callers and tests,
/// while rows remain a compact portal-list representation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanSection {
    pub items: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSessionGroup {
    pub session_id: String,
    pub label: String,
    pub current: bool,
    pub items: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TaskSidebarRow {
    PlanHeader,
    PlanItem(usize),
    SessionHeader {
        session_id: String,
        label: String,
        current: bool,
    },
    Task(usize),
    TaskDetail(usize),
}

fn status_rank(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::Idle | TaskStatus::Running | TaskStatus::Waiting => 0,
        TaskStatus::Completed
        | TaskStatus::Failed
        | TaskStatus::Cancelled
        | TaskStatus::Interrupted => 1,
    }
}

fn sidebar_groups(
    items: &[TaskSidebarItem],
    current_session_id: Option<&str>,
) -> Vec<AgentSessionGroup> {
    let mut grouped: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        grouped.entry(&item.session_id).or_default().push(index);
    }
    for indices in grouped.values_mut() {
        indices.sort_by(|left, right| {
            status_rank(items[*left].status)
                .cmp(&status_rank(items[*right].status))
                .then_with(|| items[*right].started_at_ms.cmp(&items[*left].started_at_ms))
                .then_with(|| items[*left].id.cmp(&items[*right].id))
        });
    }
    let mut groups = grouped
        .into_iter()
        .map(|(session_id, indices)| AgentSessionGroup {
            current: current_session_id == Some(session_id),
            label: indices
                .first()
                .map(|i| items[*i].session_label.clone())
                .unwrap_or_default(),
            session_id: session_id.to_owned(),
            items: indices,
        })
        .collect::<Vec<_>>();
    groups.sort_by(|a, b| {
        b.current.cmp(&a.current).then_with(|| {
            let at = a
                .items
                .first()
                .map(|i| items[*i].started_at_ms)
                .unwrap_or_default();
            let bt = b
                .items
                .first()
                .map(|i| items[*i].started_at_ms)
                .unwrap_or_default();
            bt.cmp(&at).then_with(|| a.session_id.cmp(&b.session_id))
        })
    });
    groups
}

#[cfg(test)]
fn sidebar_rows(
    plan: &SessionPlan,
    items: &[TaskSidebarItem],
    current_session_id: Option<&str>,
) -> Vec<TaskSidebarRow> {
    sidebar_rows_filtered(plan, items, current_session_id, &HashMap::new())
}

fn sidebar_rows_filtered(
    plan: &SessionPlan,
    items: &[TaskSidebarItem],
    current_session_id: Option<&str>,
    expanded: &HashMap<String, bool>,
) -> Vec<TaskSidebarRow> {
    let plan_section = PlanSection {
        items: (0..plan.items.len()).collect(),
    };
    let groups = sidebar_groups(items, current_session_id);
    let mut rows = Vec::with_capacity(items.len() + groups.len() + plan_section.items.len() + 1);
    if !plan_section.items.is_empty() {
        rows.push(TaskSidebarRow::PlanHeader);
        rows.extend(plan_section.items.into_iter().map(TaskSidebarRow::PlanItem));
    }
    for group in groups {
        let label = if group.current {
            format!("CURRENT SESSION · {}", group.label.to_uppercase())
        } else {
            group.label.to_uppercase()
        };
        rows.push(TaskSidebarRow::SessionHeader {
            session_id: group.session_id.clone(),
            label,
            current: group.current,
        });
        if expanded.get(&group.session_id).copied().unwrap_or(true) {
            rows.extend(group.items.into_iter().flat_map(|index| {
                let mut rows = vec![TaskSidebarRow::Task(index)];
                if expanded.get(&items[index].id).copied().unwrap_or(false) {
                    rows.push(TaskSidebarRow::TaskDetail(index));
                }
                rows
            }));
        }
    }
    rows
}

fn task_sidebar_row(rows: &[TaskSidebarRow], index: usize) -> Option<&TaskSidebarRow> {
    rows.get(index)
}

fn plan_progress(plan: &SessionPlan) -> (usize, usize) {
    (
        plan.items
            .iter()
            .filter(|item| item.status == PlanItemStatus::Completed)
            .count(),
        plan.items.len(),
    )
}

fn active_plan_items(plan: &SessionPlan) -> usize {
    plan.items
        .iter()
        .filter(|item| item.status == PlanItemStatus::InProgress)
        .count()
}

pub fn task_header_state(plan: &SessionPlan, items: &[TaskSidebarItem]) -> (bool, String) {
    if plan.items.is_empty() && items.is_empty() {
        return (false, String::new());
    }
    let active_tasks = items
        .iter()
        .filter(|item| {
            matches!(
                item.status,
                TaskStatus::Idle | TaskStatus::Running | TaskStatus::Waiting
            )
        })
        .count();
    let active = active_tasks + active_plan_items(plan);
    let count = match active {
        0 => String::new(),
        1..=99 => active.to_string(),
        _ => "99+".to_owned(),
    };
    (true, count)
}

fn plan_template(status: PlanItemStatus) -> LiveId {
    match status {
        PlanItemStatus::Pending => id!(PlanPending),
        PlanItemStatus::InProgress => id!(PlanInProgress),
        PlanItemStatus::Completed => id!(PlanCompleted),
    }
}

fn task_template(status: TaskStatus) -> LiveId {
    match status {
        TaskStatus::Idle | TaskStatus::Waiting => id!(TaskQueued),
        TaskStatus::Running => id!(TaskRunning),
        TaskStatus::Completed => id!(TaskCompleted),
        TaskStatus::Failed => id!(TaskFailed),
        TaskStatus::Cancelled | TaskStatus::Interrupted => id!(TaskCancelled),
    }
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Idle => "Queued",
        TaskStatus::Running => "Working",
        TaskStatus::Waiting => "Waiting",
        TaskStatus::Completed => "Completed",
        TaskStatus::Failed => "Failed",
        TaskStatus::Cancelled => "Cancelled",
        TaskStatus::Interrupted => "Interrupted",
    }
}

fn elapsed_label(started_at_ms: u128, finished_at_ms: Option<u128>) -> String {
    let end_ms = finished_at_ms.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(started_at_ms)
    });
    let seconds = end_ms.saturating_sub(started_at_ms) / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3_600)
    }
}

fn plan_progress_label(plan: &SessionPlan) -> String {
    let (completed, total) = plan_progress(plan);
    let Some(current) = plan
        .items
        .iter()
        .find(|item| item.status == PlanItemStatus::InProgress)
    else {
        return format!("{completed}/{total}");
    };
    format!("{completed}/{total} · {}", current.step)
}

fn task_activity_label(status: TaskStatus, activity: &str) -> String {
    if activity.is_empty() {
        status_label(status).to_owned()
    } else {
        format!("{} · {activity}", status_label(status))
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct TaskSidebar {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    plan: SessionPlan,
    #[rust]
    items: Vec<TaskSidebarItem>,
    #[rust]
    rows: Vec<TaskSidebarRow>,
    #[rust]
    current_session_id: Option<String>,
    #[rust]
    expanded_sessions: HashMap<String, bool>,
}

impl TaskSidebar {
    pub fn set_content(
        &mut self,
        cx: &mut Cx,
        plan: SessionPlan,
        items: Vec<TaskSidebarItem>,
        current_session_id: Option<String>,
    ) {
        if self.plan == plan && self.items == items && self.current_session_id == current_session_id
        {
            return;
        }
        self.rows = sidebar_rows_filtered(
            &plan,
            &items,
            current_session_id.as_deref(),
            &self.expanded_sessions,
        );
        self.plan = plan;
        self.items = items;
        self.current_session_id = current_session_id;
        self.view.redraw(cx);
    }
    pub fn toggle_session(&mut self, cx: &mut Cx, session_id: &str) {
        let expanded = self
            .expanded_sessions
            .entry(session_id.to_owned())
            .or_insert(true);
        *expanded = !*expanded;
        self.rows = sidebar_rows_filtered(
            &self.plan,
            &self.items,
            self.current_session_id.as_deref(),
            &self.expanded_sessions,
        );
        self.view.redraw(cx);
    }

    pub fn toggle_task(&mut self, cx: &mut Cx, task_id: &str) {
        let expanded = self
            .expanded_sessions
            .entry(task_id.to_owned())
            .or_insert(false);
        *expanded = !*expanded;
        self.rows = sidebar_rows_filtered(
            &self.plan,
            &self.items,
            self.current_session_id.as_deref(),
            &self.expanded_sessions,
        );
        self.view.redraw(cx);
    }
}

impl Widget for TaskSidebar {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                list.set_item_range(cx, 0, self.rows.len());
                while let Some(row_index) = list.next_visible_item(cx) {
                    let Some(sidebar_row) = task_sidebar_row(&self.rows, row_index) else {
                        continue;
                    };
                    match sidebar_row {
                        TaskSidebarRow::PlanHeader => {
                            let row = list.item(cx, row_index, id!(PlanHeader));
                            let progress = plan_progress_label(&self.plan);
                            row.label(cx, ids!(progress_lbl)).set_text(cx, &progress);
                            row.draw_all_unscoped(cx);
                        }
                        TaskSidebarRow::PlanItem(item_index) => {
                            let Some(plan_item) = self.plan.items.get(*item_index) else {
                                continue;
                            };
                            let row = list.item(cx, row_index, plan_template(plan_item.status));
                            row.label(cx, ids!(step_lbl)).set_text(cx, &plan_item.step);
                            row.draw_all_unscoped(cx);
                        }
                        TaskSidebarRow::SessionHeader { label, .. } => {
                            let row = list.item(cx, row_index, id!(SessionHeader));
                            row.label(cx, ids!(session_lbl)).set_text(cx, label);
                            row.draw_all_unscoped(cx);
                        }
                        TaskSidebarRow::Task(item_index) => {
                            let Some(task) = self.items.get(*item_index) else {
                                continue;
                            };
                            let row = list.item(cx, row_index, task_template(task.status));
                            row.label(cx, ids!(summary_lbl)).set_text(cx, &task.summary);
                            row.label(cx, ids!(agent_lbl))
                                .set_text(cx, &format!("Agent: {}", task.agent));
                            let activity = format!(
                                "{} · {}",
                                task_activity_label(task.status, &task.activity),
                                elapsed_label(task.started_at_ms, task.finished_at_ms)
                            );
                            row.label(cx, ids!(activity_lbl)).set_text(cx, &activity);
                            row.button(cx, ids!(cancel_btn))
                                .set_visible(cx, task.cancellable);
                            row.button(cx, ids!(resume_btn))
                                .set_visible(cx, task.resumable);
                            row.draw_all_unscoped(cx);
                        }
                        TaskSidebarRow::TaskDetail(item_index) => {
                            let Some(task) = self.items.get(*item_index) else {
                                continue;
                            };
                            let row = list.item(cx, row_index, id!(TaskDetail));
                            let detail = if task.activity.is_empty() {
                                format!(
                                    "{} · {} · {}",
                                    task.agent,
                                    status_label(task.status),
                                    elapsed_label(task.started_at_ms, task.finished_at_ms)
                                )
                            } else {
                                format!(
                                    "{} · {} · {}\n{}",
                                    task.agent,
                                    status_label(task.status),
                                    elapsed_label(task.started_at_ms, task.finished_at_ms),
                                    task.activity
                                )
                            };
                            row.label(cx, ids!(detail_lbl)).set_text(cx, &detail);
                            row.draw_all_unscoped(cx);
                        }
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else {
            return;
        };
        if self.view.button(cx, ids!(close_btn)).clicked(actions) {
            cx.widget_action(self.widget_uid(), TaskSidebarAction::Close);
            return;
        }
        let list = self.view.portal_list(cx, ids!(list));
        for (row_index, row) in list.items_with_actions(actions) {
            let Some(sidebar_row) = self.rows.get(row_index) else {
                continue;
            };
            if let TaskSidebarRow::SessionHeader { session_id, .. } = sidebar_row {
                if let Some(finger_up) = row.as_view().finger_up(actions) {
                    if finger_up.is_over && finger_up.is_primary_hit() && finger_up.was_tap() {
                        let session_id = session_id.clone();
                        cx.widget_action(
                            self.widget_uid(),
                            TaskSidebarAction::ToggleSession(session_id),
                        );
                    }
                }
                continue;
            }
            let TaskSidebarRow::Task(item_index) = sidebar_row else {
                continue;
            };
            let Some(task) = self.items.get(*item_index) else {
                continue;
            };
            if row.button(cx, ids!(details_btn)).clicked(actions) {
                let task_id = task.id.clone();
                self.toggle_task(cx, &task_id);
                cx.widget_action(self.widget_uid(), TaskSidebarAction::ToggleTask(task_id));
                continue;
            }
            if row.button(cx, ids!(cancel_btn)).clicked(actions) {
                cx.widget_action(
                    self.widget_uid(),
                    TaskSidebarAction::Cancel(task.id.clone()),
                );
                continue;
            }
            if row.button(cx, ids!(resume_btn)).clicked(actions) {
                cx.widget_action(
                    self.widget_uid(),
                    TaskSidebarAction::Resume(task.id.clone()),
                );
                continue;
            }
            if let Some(finger_up) = row.as_view().finger_up(actions) {
                if finger_up.is_over && finger_up.is_primary_hit() && finger_up.was_tap() {
                    cx.widget_action(
                        self.widget_uid(),
                        TaskSidebarAction::OpenSession {
                            session_id: task.session_id.clone(),
                            session_file: task.session_file.clone(),
                        },
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_agent::{PlanItem, PlanItemStatus, SessionPlan};

    fn item(
        id: &str,
        session_id: &str,
        status: TaskStatus,
        started_at_ms: u128,
    ) -> TaskSidebarItem {
        TaskSidebarItem {
            id: id.into(),
            session_id: session_id.into(),
            session_label: session_id.into(),
            session_file: None,
            agent: "worker".into(),
            summary: id.into(),
            activity: String::new(),
            status,
            cancellable: false,
            resumable: false,
            started_at_ms,
            finished_at_ms: None,
        }
    }

    #[test]
    fn sidebar_groups_current_session_first_and_active_tasks_first() {
        let items = vec![
            item("done", "chat-b", TaskStatus::Completed, 10),
            item("running", "chat-a", TaskStatus::Running, 20),
            item("older", "chat-a", TaskStatus::Completed, 5),
        ];
        let rows = sidebar_rows(&SessionPlan::default(), &items, Some("chat-a"));
        assert!(matches!(
            &rows[0],
            TaskSidebarRow::SessionHeader { current: true, .. }
        ));
        assert!(matches!(
            &rows[1],
            TaskSidebarRow::Task(index) if items[*index].id == "running"
        ));
        assert!(matches!(
            &rows[2],
            TaskSidebarRow::Task(index) if items[*index].id == "older"
        ));
    }

    #[test]
    fn header_is_hidden_only_when_project_has_no_tasks() {
        assert_eq!(
            task_header_state(&SessionPlan::default(), &[]),
            (false, String::new())
        );
        assert_eq!(
            task_header_state(
                &SessionPlan::default(),
                &[
                    item("a", "chat-a", TaskStatus::Running, 1),
                    item("b", "chat-a", TaskStatus::Completed, 2),
                ]
            ),
            (true, "1".into())
        );
        assert_eq!(
            task_header_state(
                &SessionPlan::default(),
                &[item("done", "chat-a", TaskStatus::Completed, 1)]
            ),
            (true, String::new())
        );
    }

    #[test]
    fn plan_rows_precede_project_tasks_and_report_progress() {
        let plan = SessionPlan {
            explanation: None,
            items: vec![
                PlanItem {
                    step: "Inspect".into(),
                    status: PlanItemStatus::Completed,
                },
                PlanItem {
                    step: "Implement".into(),
                    status: PlanItemStatus::InProgress,
                },
                PlanItem {
                    step: "Verify".into(),
                    status: PlanItemStatus::Pending,
                },
            ],
        };
        let items = vec![item("task", "chat-a", TaskStatus::Running, 1)];
        let rows = sidebar_rows(&plan, &items, Some("chat-a"));

        assert!(matches!(rows[0], TaskSidebarRow::PlanHeader));
        assert!(matches!(rows[1], TaskSidebarRow::PlanItem(0)));
        assert_eq!(plan_progress(&plan), (1, 3));
        assert_eq!(active_plan_items(&plan), 1);
        assert_eq!(task_header_state(&plan, &[]), (true, "1".into()));
    }

    #[test]
    fn header_count_combines_active_plan_and_agent_work() {
        let plan = SessionPlan {
            explanation: None,
            items: vec![PlanItem {
                step: "Implement".into(),
                status: PlanItemStatus::InProgress,
            }],
        };
        let items = vec![item("task", "chat-a", TaskStatus::Running, 1)];

        assert_eq!(task_header_state(&plan, &items), (true, "2".into()));
        assert_eq!(
            task_header_state(&SessionPlan::default(), &items),
            (true, "1".into())
        );
    }

    #[test]
    fn progress_and_activity_labels_include_current_context() {
        let plan = SessionPlan {
            explanation: None,
            items: vec![PlanItem {
                step: "Implement sidebar".into(),
                status: PlanItemStatus::InProgress,
            }],
        };
        assert_eq!(plan_progress_label(&plan), "0/1 · Implement sidebar");
        assert_eq!(
            task_activity_label(TaskStatus::Running, "Reading events"),
            "Working · Reading events"
        );
    }

    #[test]
    fn elapsed_label_freezes_at_task_completion() {
        assert_eq!(elapsed_label(1_000, Some(5_000)), "4s");
    }

    #[test]
    fn collapsed_session_rows_preserve_stable_session_identity() {
        let items = vec![
            item("a", "session-a", TaskStatus::Running, 2),
            item("b", "session-b", TaskStatus::Running, 1),
        ];
        let mut expanded = HashMap::new();
        expanded.insert("session-a".to_owned(), false);
        let rows = sidebar_rows_filtered(
            &SessionPlan::default(),
            &items,
            Some("session-b"),
            &expanded,
        );

        assert!(matches!(
            rows.iter().find(|row| matches!(row, TaskSidebarRow::SessionHeader { session_id, .. } if session_id == "session-a")),
            Some(_)
        ));
        assert!(!rows
            .iter()
            .any(|row| matches!(row, TaskSidebarRow::Task(index) if items[*index].id == "a")));
        assert!(rows
            .iter()
            .any(|row| matches!(row, TaskSidebarRow::Task(index) if items[*index].id == "b")));
    }

    #[test]
    fn stale_portal_row_is_ignored_after_the_range_shrinks() {
        let rows = vec![TaskSidebarRow::Task(0)];

        assert_eq!(task_sidebar_row(&rows, 1), None);
    }
}
