use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::InteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;

use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::theme::ActiveTheme;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, Selectable, Sizable, WindowExt};

use threadlane_ui_state::{
    AppState, GitHubTab, SessionAttention, SessionInfo, TrajectoryEntry, WorkspacePage,
};
use threadlane_ui_state::{actions::AppAction, controller};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionRemovalKind {
    Archive,
    Remove,
}

impl SessionRemovalKind {
    fn title(self) -> &'static str {
        match self {
            Self::Archive => "Archive session?",
            Self::Remove => "Remove session?",
        }
    }

    fn description(self, title: &str, project: &str, worktree_note: Option<&str>) -> String {
        let base = match self {
            // Archive hides the session from the list but keeps its
            // transcript in the archive; Remove destroys it permanently.
            Self::Archive => format!(
                "“{title}” ({project}) will leave the active list. Its transcript stays in the archive."
            ),
            Self::Remove => {
                format!("“{title}” ({project}) will be permanently deleted, transcript included.")
            }
        };
        match worktree_note {
            Some(note) => format!("{base}\n{note}"),
            None => base,
        }
    }

    fn action_prefix(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Remove => "remove",
        }
    }

    fn button_props(self) -> DialogButtonProps {
        match self {
            Self::Archive => DialogButtonProps::default()
                .ok_text("Archive")
                .show_cancel(true),
            Self::Remove => DialogButtonProps::default()
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true),
        }
    }

    fn dispatch_action(
        self,
        work_dir: PathBuf,
        session_id: String,
        delete_worktree: bool,
    ) -> AppAction {
        match self {
            Self::Archive => AppAction::SettleSession {
                work_dir,
                session_id,
                delete_worktree,
            },
            Self::Remove => AppAction::RemoveSession {
                work_dir,
                session_id,
                delete_worktree,
            },
        }
    }
}

fn open_session_removal_dialog(
    window: &mut Window,
    cx: &mut App,
    model: Entity<AppState>,
    work_dir: PathBuf,
    session_id: String,
    is_worktree: bool,
    git_branch: Option<String>,
    kind: SessionRemovalKind,
) {
    let delete_worktree = Rc::new(Cell::new(true));
    window.open_alert_dialog(cx, {
        let model = model.clone();
        let work_dir = work_dir.clone();
        let session_id = session_id.clone();
        let delete_worktree = delete_worktree.clone();
        move |alert, _window, _cx| {
            let model = model.clone();
            let work_dir = work_dir.clone();
            let session_id = session_id.clone();
            let delete_worktree = delete_worktree.clone();
            // Identify by title + project, never a raw session id; spell
            // out the worktree effect and the Archive-vs-Remove distinction.
            let (title, project_name) = model
                .read(_cx)
                .projects
                .iter()
                .find(|project| project.work_dir == work_dir)
                .and_then(|project| {
                    project
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)
                        .map(|session| (session.title.clone(), project.name.clone()))
                })
                .unwrap_or_else(|| ("Untitled session".into(), "project".into()));
            let worktree_note = is_worktree.then(|| {
                let branch = git_branch
                    .as_deref()
                    .map(|branch| format!(" on branch '{branch}'"))
                    .unwrap_or_default();
                if delete_worktree.get() {
                    format!("Its worktree{branch} will be deleted too. Uncheck below to keep it.")
                } else {
                    format!("Its worktree{branch} will be kept.")
                }
            });
            let mut alert = alert
                .title(kind.title())
                .description(kind.description(&title, &project_name, worktree_note.as_deref()))
                .button_props(kind.button_props());

            if is_worktree {
                let delete_worktree_click = delete_worktree.clone();
                let model_click = model.clone();
                let label = if let Some(branch) = &git_branch {
                    format!("Delete associated worktree ({branch})")
                } else {
                    "Delete associated worktree".to_string()
                };
                alert = alert.child(
                    div().pt_2().child(
                        Checkbox::new(SharedString::from(format!(
                            "{}-delete-worktree-{}",
                            kind.action_prefix(),
                            session_id
                        )))
                        .checked(delete_worktree.get())
                        .label(label)
                        .on_click(move |checked, _window, cx| {
                            delete_worktree_click.set(*checked);
                            model_click.update(cx, |_state, cx| {
                                cx.notify();
                            });
                        }),
                    ),
                );
            }

            alert.on_ok(move |_event, _window, cx| {
                let delete_worktree_val = if is_worktree {
                    delete_worktree.get()
                } else {
                    false
                };
                model.update(cx, |state, cx| {
                    controller::dispatch(
                        state,
                        kind.dispatch_action(
                            work_dir.clone(),
                            session_id.clone(),
                            delete_worktree_val,
                        ),
                    );
                    cx.notify();
                });
                true
            })
        }
    });
}

fn open_archive_session_dialog(
    window: &mut Window,
    cx: &mut App,
    model: Entity<AppState>,
    work_dir: PathBuf,
    session_id: String,
    is_worktree: bool,
    git_branch: Option<String>,
) {
    open_session_removal_dialog(
        window,
        cx,
        model,
        work_dir,
        session_id,
        is_worktree,
        git_branch,
        SessionRemovalKind::Archive,
    );
}

fn open_remove_session_dialog(
    window: &mut Window,
    cx: &mut App,
    model: Entity<AppState>,
    work_dir: PathBuf,
    session_id: String,
    is_worktree: bool,
    git_branch: Option<String>,
) {
    open_session_removal_dialog(
        window,
        cx,
        model,
        work_dir,
        session_id,
        is_worktree,
        git_branch,
        SessionRemovalKind::Remove,
    );
}

fn safe_file_stem(title: &str) -> String {
    let stem = title
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let stem = stem.trim_matches('-');
    if stem.is_empty() {
        "threadlane-session".into()
    } else {
        stem.into()
    }
}

fn read_jsonl_for_export(path: &std::path::Path) -> Result<Vec<serde_json::Value>, String> {
    let contents = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    Ok(contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(
            |(index, line)| match serde_json::from_str::<serde_json::Value>(line) {
                Ok(record) => serde_json::json!({ "line": index + 1, "record": record }),
                Err(error) => serde_json::json!({
                    "line": index + 1,
                    "raw": line,
                    "parse_error": error.to_string(),
                }),
            },
        )
        .collect())
}

fn build_diagnostic_export(
    session_file: &std::path::Path,
    session_id: &str,
    title: &str,
    work_dir: &std::path::Path,
    runtime: Option<&threadlane_coding_agent::controller::SessionRuntime>,
    trajectory: Vec<TrajectoryEntry>,
    include_log: bool,
) -> Result<serde_json::Value, String> {
    let exported_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let selected_model = runtime.map(|runtime| runtime.selected_model.clone());
    let system_prompt = runtime.map(|runtime| runtime.system_prompt.clone());
    let harness_error = runtime.and_then(|runtime| runtime.harness_error.clone());
    let runtime_status = runtime.map(|runtime| format!("{:?}", runtime.status()));
    let log = if include_log {
        let canonical = read_jsonl_for_export(session_file)?;
        Some(serde_json::json!({
            "canonical_records": canonical,
        }))
    } else {
        None
    };

    Ok(serde_json::json!({
        "schema_version": 1,
        "exported_at_unix": exported_at_unix,
        "session": {
            "id": session_id,
            "title": title,
            "project_root": work_dir.display().to_string(),
            "session_file": session_file.display().to_string(),
            "selected_model": selected_model,
            "runtime_status": runtime_status,
            "harness_error": harness_error,
        },
        "system_prompt": system_prompt,
        "trajectory": trajectory,
        "session_log": log,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DateGroup {
    NeedsYou,
    Working,
    Today,
    Yesterday,
    ThisWeek,
    Older,
}

#[derive(Clone)]
enum HistoryRow {
    Group(DateGroup),
    Session(SessionInfo, SessionAttention),
}

fn same_history_row_identity(left: &HistoryRow, right: &HistoryRow) -> bool {
    match (left, right) {
        (HistoryRow::Group(left), HistoryRow::Group(right)) => left == right,
        (HistoryRow::Session(left, _), HistoryRow::Session(right, _)) => {
            left.id == right.id && left.work_dir == right.work_dir
        }
        _ => false,
    }
}

/// Sidebar search predicate: matches title, id, project, branch, and
/// directory name (all pre-lowercased by the caller).
fn history_query_matches(
    title: &str,
    id: &str,
    project_name: &str,
    branch: &str,
    dir_name: &str,
    query: &str,
) -> bool {
    query.is_empty()
        || title.contains(query)
        || id.contains(query)
        || project_name.contains(query)
        || branch.contains(query)
        || dir_name.contains(query)
}

fn flatten_history_sessions(
    mut sessions: Vec<(SessionInfo, SessionAttention)>,
    now: u64,
) -> Vec<HistoryRow> {
    sessions.sort_by(|(left, left_attention), (right, right_attention)| {
        let left_group = history_group(*left_attention, left.updated_at, now);
        let right_group = history_group(*right_attention, right.updated_at, now);
        left_group
            .rank()
            .cmp(&right_group.rank())
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.title.cmp(&right.title))
    });

    let mut rows = Vec::with_capacity(sessions.len() + DateGroup::COUNT);
    let mut previous_group = None;
    for (session, attention) in sessions {
        let group = history_group(attention, session.updated_at, now);
        if previous_group != Some(group) {
            rows.push(HistoryRow::Group(group));
            previous_group = Some(group);
        }
        rows.push(HistoryRow::Session(session, attention));
    }
    rows
}

impl DateGroup {
    const COUNT: usize = 6;

    fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This Week",
            Self::Older => "Older",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::NeedsYou => 0,
            Self::Working => 1,
            Self::Today => 2,
            Self::Yesterday => 3,
            Self::ThisWeek => 4,
            Self::Older => 5,
        }
    }
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Callers pass a shared `now` so a render pass performs one clock read
/// instead of one per row.
fn get_date_group(timestamp: u64, now: u64) -> DateGroup {
    let seconds = now.saturating_sub(timestamp);
    if seconds < 86400 {
        DateGroup::Today
    } else if seconds < 172800 {
        DateGroup::Yesterday
    } else if seconds < 604800 {
        DateGroup::ThisWeek
    } else {
        DateGroup::Older
    }
}

fn history_group(attention: SessionAttention, timestamp: u64, now: u64) -> DateGroup {
    match attention {
        SessionAttention::NeedsYou => DateGroup::NeedsYou,
        SessionAttention::Working => DateGroup::Working,
        SessionAttention::Ready | SessionAttention::Idle => get_date_group(timestamp, now),
    }
}

fn format_time_ago(timestamp: u64, now: u64) -> String {
    let seconds = now.saturating_sub(timestamp);
    match seconds {
        0..=59 => "Just now".to_string(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

pub struct SidebarView {
    model: Entity<AppState>,
    search_input: Entity<InputState>,
    /// Hash of the model state the sidebar renders; lets the observer skip
    /// notifications for streaming updates that cannot change any row.
    history_fingerprint: u64,
    /// Flattened, sorted rows cached per fingerprint for the virtual list.
    history_cache: Option<(u64, Vec<HistoryRow>)>,
    attention_filter: Option<SessionAttention>,
    history_list_state: ListState,
    _subscriptions: Vec<Subscription>,
}

fn sidebar_session_fingerprint(session: &SessionInfo, attention: SessionAttention) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    threadlane_ui_state::hash_session_identity(&mut hasher, session);
    session.work_dir.hash(&mut hasher);
    session.session_file.hash(&mut hasher);
    session.updated_at.hash(&mut hasher);
    match session.github_issue.as_ref() {
        Some(issue) => {
            true.hash(&mut hasher);
            issue.host.hash(&mut hasher);
            issue.owner.hash(&mut hasher);
            issue.repo.hash(&mut hasher);
            issue.number.hash(&mut hasher);
            issue.url.hash(&mut hasher);
        }
        None => false.hash(&mut hasher),
    }
    session.is_worktree.hash(&mut hasher);
    session.worktree_available.hash(&mut hasher);
    attention.hash(&mut hasher);
    hasher.finish()
}

struct SidebarSessionIdentity {
    title: String,
    tooltip: String,
}

fn sidebar_session_identity(session: &SessionInfo) -> SidebarSessionIdentity {
    let Some(issue) = session.github_issue.as_ref() else {
        return SidebarSessionIdentity {
            title: session.title.clone(),
            tooltip: session.title.clone(),
        };
    };
    let prefix = format!("#{}", issue.number);
    let title = session.title.trim();
    let issue_title = title
        .strip_prefix(&prefix)
        .filter(|rest| rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace))
        .map(str::trim_start)
        .unwrap_or(title);
    let title = if issue_title.is_empty() {
        prefix.clone()
    } else {
        format!("{prefix} {issue_title}")
    };
    SidebarSessionIdentity {
        tooltip: format!("{}/{}\n{}", issue.owner, issue.repo, title),
        title,
    }
}

/// Hash of every piece of `AppState` the sidebar renders. Streaming deltas
/// mutate messages, plans, and usage without touching any of these fields, so
/// an unchanged hash lets the observer skip `cx.notify()` entirely. The minute
/// bucket keeps relative timestamps fresh without firing every second.
fn sidebar_fingerprint(state: &AppState, now: u64) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    state.active_work_dir.hash(&mut hasher);
    state.active_session_id.hash(&mut hasher);
    state.workspace_page.hash(&mut hasher);
    state.github_tab.hash(&mut hasher);
    state.automations.snapshot.revision.hash(&mut hasher);
    state.sidebar_project_filter.hash(&mut hasher);
    for byte in state.search_query.trim().bytes() {
        hasher.write_u8(byte.to_ascii_lowercase());
    }
    (now / 60).hash(&mut hasher);
    for project in &state.projects {
        project.name.hash(&mut hasher);
        project.work_dir.hash(&mut hasher);
        for session in &project.sessions {
            sidebar_session_fingerprint(session, state.session_attention(session))
                .hash(&mut hasher);
        }
    }
    // Hash-map iteration is stable between notifications unless the map changes;
    // avoiding temporary sorted key vectors keeps streaming notifications cheap.
    for (work_dir, status) in &state.git_statuses {
        work_dir.hash(&mut hasher);
        if let Some(pr) = status.pr.as_ref() {
            pr.number.hash(&mut hasher);
            pr.state.hash(&mut hasher);
            pr.is_draft.hash(&mut hasher);
            pr.total_checks.hash(&mut hasher);
            pr.failing_checks.hash(&mut hasher);
            pr.pending_checks.hash(&mut hasher);
            pr.passing_checks.hash(&mut hasher);
        }
    }
    for ((work_dir, branch), pr) in &state.git_prs {
        work_dir.hash(&mut hasher);
        branch.hash(&mut hasher);
        if let Some(pr) = pr {
            pr.number.hash(&mut hasher);
            pr.state.hash(&mut hasher);
            pr.is_draft.hash(&mut hasher);
            pr.total_checks.hash(&mut hasher);
            pr.failing_checks.hash(&mut hasher);
            pr.pending_checks.hash(&mut hasher);
            pr.passing_checks.hash(&mut hasher);
            pr.comments_count.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn session_pr_info<'a>(
    session: &SessionInfo,
    prs: &'a std::collections::HashMap<
        (std::path::PathBuf, String),
        Option<threadlane_git::GitHubPrInfo>,
    >,
) -> Option<&'a threadlane_git::GitHubPrInfo> {
    let branch = session.git_branch.as_ref()?;
    prs.get(&(session.work_dir.clone(), branch.clone()))
        .and_then(Option::as_ref)
}

fn pr_status_label(pr: &threadlane_git::GitHubPrInfo) -> &'static str {
    if pr.state.eq_ignore_ascii_case("merged") {
        "Merged"
    } else if pr.is_draft || pr.state.eq_ignore_ascii_case("draft") {
        "Draft"
    } else if pr.state.eq_ignore_ascii_case("closed") {
        "Closed"
    } else {
        "Open"
    }
}

fn pr_status_tooltip(pr: &threadlane_git::GitHubPrInfo) -> String {
    format!(
        "PR #{} · {}\n{}\n{} → {}\nChecks: {} passed · {} pending · {} failed\nDiscussion: {} comments · {} review comments\n{}",
        pr.number,
        pr_status_label(pr),
        pr.title,
        pr.head_ref,
        pr.base_ref,
        pr.passing_checks,
        pr.pending_checks,
        pr.failing_checks,
        pr.comments_count,
        pr.review_comments.len(),
        pr.url,
    )
}

impl SidebarView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search sessions…"));

        let sub1 = cx.observe(&model, |this, model, cx| {
            let fingerprint = sidebar_fingerprint(model.read(cx), now_unix_secs());
            if this.history_fingerprint != fingerprint {
                this.history_fingerprint = fingerprint;
                cx.notify();
            }
        });

        let model_clone = model.clone();
        let sub2 = cx.subscribe_in(
            &search_input,
            window,
            move |_this, search_input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = search_input.read(cx).value().to_string();
                    model_clone.update(cx, |state, cx| {
                        state.search_query = query;
                        cx.notify();
                    });
                }
            },
        );

        let history_fingerprint = sidebar_fingerprint(model.read(cx), now_unix_secs());
        Self {
            model,
            search_input,
            history_fingerprint,
            attention_filter: None,
            history_cache: None,
            history_list_state: ListState::new(0, ListAlignment::Top, window.rem_size() * 4.5),
            _subscriptions: vec![sub1, sub2],
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .pt(threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE)
            .pb_1()
            .bg(theme.title_bar)
            .child(
                Button::new("new-task-btn")
                    .accessibility_label("Start a new task (⌘N)")
                    .ghost()
                    .w_full()
                    .justify_start()
                    .tooltip("Start a new task (⌘N)")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .w_full()
                            .child(Icon::new(IconName::Plus).size_4())
                            .child(div().flex_1().child("New task"))
                            .child(
                                div()
                                    .px_1p5()
                                    .py(rems(0.03125))
                                    .rounded_sm()
                                    .bg(theme.muted)
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("⌘N"),
                            ),
                    )
                    .on_click(move |_event, window, cx| {
                        window.dispatch_action(Box::new(crate::BeginNewTask), cx);
                    }),
            )
            .child(self.render_github_nav(cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mt_2()
                    .px_2()
                    .h(rems(2.25))
                    .rounded_md()
                    .text_color(theme.muted_foreground)
                    .child(IconName::Search)
                    .child(
                        div().flex_1().child(
                            Input::new(&self.search_input)
                                .appearance(false)
                                .bordered(false)
                                .aria_label("Search sessions"),
                        ),
                    ),
            )
    }

    fn render_project_filter(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (projects, selected_filter) = {
            let state = self.model.read(cx);
            (
                state
                    .projects
                    .iter()
                    .map(|project| {
                        (
                            project.name.clone(),
                            project.work_dir.clone(),
                            project.sessions.len(),
                        )
                    })
                    .collect::<Vec<_>>(),
                state.sidebar_project_filter.clone(),
            )
        };
        let selected_label = selected_filter
            .as_ref()
            .and_then(|selected| {
                projects
                    .iter()
                    .find(|(_, work_dir, _)| work_dir == selected)
                    .map(|(name, _, _)| name.clone())
            })
            .unwrap_or_else(|| "All projects".into());
        let filter_model = self.model.clone();
        let attach_model = self.model.clone();

        div()
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .pb_1()
            .child(
                div().min_w_0().flex_1().child(
                    Button::new("sidebar-project-filter")
                        .icon(IconName::Folder)
                        .label(selected_label.clone())
                        .accessibility_label(format!(
                            "Filter sessions by project: {selected_label}"
                        ))
                        .tooltip("Filter sessions by project")
                        .dropdown_caret(true)
                        .selected(true)
                        .w_full()
                        .justify_start()
                        .dropdown_menu(move |menu, _window, _cx| {
                            let all_model = filter_model.clone();
                            let total_sessions: usize =
                                projects.iter().map(|(_, _, count)| count).sum();
                            let mut menu = menu.item(
                                PopupMenuItem::new(format!("All projects · {total_sessions}"))
                                    .checked(selected_filter.is_none())
                                    .on_click(move |_event, _window, cx| {
                                        all_model.update(cx, |state, cx| {
                                            controller::dispatch(
                                                state,
                                                AppAction::SetSidebarProjectFilter(None),
                                            );
                                            cx.notify();
                                        });
                                    }),
                            );
                            for (name, work_dir, session_count) in projects.clone() {
                                let model = filter_model.clone();
                                let checked = selected_filter.as_ref() == Some(&work_dir);
                                let item_label = format!("{name} · {session_count}");
                                menu = menu.item(
                                    PopupMenuItem::new(item_label).checked(checked).on_click(
                                        move |_event, _window, cx| {
                                            model.update(cx, |state, cx| {
                                                controller::dispatch(
                                                    state,
                                                    AppAction::SetSidebarProjectFilter(Some(
                                                        work_dir.clone(),
                                                    )),
                                                );
                                                cx.notify();
                                            });
                                        },
                                    ),
                                );
                            }
                            menu
                        }),
                ),
            )
            .child(
                Button::new("attach-project-btn")
                    .icon(IconName::Plus)
                    .accessibility_label("Attach project")
                    .tooltip("Attach project…")
                    .ghost()
                    .xsmall()
                    .on_click(move |_event, _window, cx| {
                        let model = attach_model.clone();
                        cx.spawn(async move |cx| {
                            let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await
                            else {
                                return;
                            };
                            let path = folder.path().to_path_buf();
                            let _ = model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::AttachProject(path));
                                cx.notify();
                            });
                        })
                        .detach();
                    }),
            )
            .bg(theme.title_bar)
    }

    fn has_history_filters(&self, state: &AppState) -> bool {
        !state.search_query.trim().is_empty()
            || state.sidebar_project_filter.is_some()
            || self.attention_filter.is_some()
    }

    fn clear_history_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attention_filter = None;
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.model.update(cx, |state, cx| {
            state.search_query.clear();
            controller::dispatch(state, AppAction::SetSidebarProjectFilter(None));
            cx.notify();
        });
        cx.notify();
    }

    fn render_history_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let state = self.model.read(cx);
        let session_count = state
            .projects
            .iter()
            .filter(|project| {
                state
                    .sidebar_project_filter
                    .as_ref()
                    .is_none_or(|selected| &project.work_dir == selected)
            })
            .map(|project| project.sessions.len())
            .sum::<usize>();
        let mut attention_counts = [0usize; 3];
        for project in &state.projects {
            if state
                .sidebar_project_filter
                .as_ref()
                .is_some_and(|selected| &project.work_dir != selected)
            {
                continue;
            }
            for session in &project.sessions {
                match state.session_attention(session) {
                    SessionAttention::NeedsYou => attention_counts[0] += 1,
                    SessionAttention::Working => attention_counts[1] += 1,
                    SessionAttention::Ready => attention_counts[2] += 1,
                    SessionAttention::Idle => {}
                }
            }
        }

        let selected_filter = self.attention_filter;
        let sidebar = cx.entity().downgrade();
        let has_filters = self.has_history_filters(state);

        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .pt_2()
            .pb_1()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(format!("Tasks · {session_count}")),
                    )
                    .child(
                        Button::new("sidebar-status-filter")
                            .debug_selector(|| "sidebar-status-filter".into())
                            .label(selected_filter.map_or("All statuses", SessionAttention::label))
                            .accessibility_label("Filter tasks by status")
                            .tooltip("Filter tasks by status")
                            .small()
                            .ghost()
                            .dropdown_caret(true)
                            .selected(selected_filter.is_some())
                            .dropdown_menu(move |menu, _window, _cx| {
                                let mut menu = menu;
                                for (filter, label) in [
                                    (None, format!("All statuses · {session_count}")),
                                    (
                                        Some(SessionAttention::NeedsYou),
                                        format!("Needs you · {}", attention_counts[0]),
                                    ),
                                    (
                                        Some(SessionAttention::Working),
                                        format!("Working · {}", attention_counts[1]),
                                    ),
                                    (
                                        Some(SessionAttention::Ready),
                                        format!("Ready · {}", attention_counts[2]),
                                    ),
                                ] {
                                    let sidebar = sidebar.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(label)
                                            .checked(selected_filter == filter)
                                            .on_click(move |_, _, cx| {
                                                let _ = sidebar.update(cx, |this, cx| {
                                                    this.attention_filter = filter;
                                                    cx.notify();
                                                });
                                            }),
                                    );
                                }
                                menu
                            }),
                    ),
            )
            .when(has_filters, |this| {
                this.child(
                    Button::new("sidebar-clear-filters")
                        .debug_selector(|| "sidebar-clear-filters".into())
                        .label("Clear filters")
                        .tooltip("Clear search, project, and status filters")
                        .small()
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.clear_history_filters(window, cx);
                        })),
                )
            })
    }

    fn render_session_card(
        &self,
        session: &SessionInfo,
        attention: SessionAttention,
        is_active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let status_indicator = match attention {
            SessionAttention::NeedsYou => Some(
                div()
                    .debug_selector({
                        let id = session.id.clone();
                        move || format!("session-attention-{id}")
                    })
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(rems(0.21875))
                    .px_1p5()
                    .py(rems(0.03125))
                    .rounded_full()
                    .bg(theme.warning.opacity(0.15))
                    .text_color(theme.warning)
                    .child(div().size(rems(0.375)).rounded_full().bg(theme.warning))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child(attention.label()),
                    )
                    .into_any_element(),
            ),
            SessionAttention::Working => Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(rems(0.21875))
                    .px_1p5()
                    .py(rems(0.03125))
                    .rounded_full()
                    .bg(theme.info.opacity(0.12))
                    .text_color(theme.foreground)
                    .child(Spinner::new().xsmall().color(theme.info))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child(attention.label()),
                    )
                    .into_any_element(),
            ),
            SessionAttention::Ready => Some(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(attention.label())
                    .into_any_element(),
            ),
            SessionAttention::Idle => None,
        };

        let bg_color = if is_active {
            theme.sidebar_accent
        } else {
            gpui::transparent_black()
        };

        let border_color = if is_active {
            theme.primary.opacity(0.38)
        } else {
            gpui::transparent_black()
        };

        let title_color = if is_active {
            theme.foreground
        } else {
            theme.sidebar_foreground
        };
        let session_identity = sidebar_session_identity(session);
        let session_title = session_identity.title;
        let time_ago = format_time_ago(session.updated_at, now_unix_secs());
        let project = self
            .model
            .read(cx)
            .projects
            .iter()
            .find(|project| {
                project
                    .sessions
                    .iter()
                    .any(|candidate| candidate.session_file == session.session_file)
            })
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "Project".to_string());
        // Rich hover card (Synara ThreadHoverCardContent pattern): keep the
        // row to title + status, move project path, branch/worktree, recency,
        // and attention detail into the tooltip.
        let work_dir_display = session.work_dir.to_string_lossy().into_owned();
        let branch_display = session.git_branch.as_deref().unwrap_or("no branch");
        let worktree_display = if session.is_worktree {
            if session.worktree_available {
                "worktree"
            } else {
                "worktree unavailable"
            }
        } else {
            "local checkout"
        };
        let session_tooltip = format!(
            "{}\n{} · {}\nBranch: {branch_display} ({worktree_display})\n{} · {}",
            session_identity.tooltip,
            project,
            work_dir_display,
            time_ago,
            attention.label(),
        );

        let work_dir = session.work_dir.clone();
        let session_id = session.id.clone();
        let model = self.model.clone();
        let title_work_dir = session.work_dir.clone();
        let title_session_id = session.id.clone();
        let title_model = self.model.clone();
        let context_work_dir = session.work_dir.clone();
        let context_session_id = session.id.clone();
        let context_model = self.model.clone();
        let terminal_model = self.model.clone();
        let terminal_work_dir = session.runtime_work_dir.clone();
        let terminal_unavailable = session.is_worktree && !session.worktree_available;
        let context_is_worktree = session.is_worktree;
        let context_git_branch = session.git_branch.clone();
        let copy_session_file = session.session_file.display().to_string();
        let export_log_source = session.session_file.clone();
        let export_trajectory_title = session.title.clone();
        let quick_settle_model = self.model.clone();
        let quick_settle_work_dir = session.work_dir.clone();
        let quick_settle_session_id = session.id.clone();
        let quick_settle_is_worktree = session.is_worktree;
        let quick_settle_git_branch = session.git_branch.clone();

        // Full-row screen-reader label: the inner title button only carries
        // the title, so status, project, branch, and recency live here.
        // Keyboard users operate the row through its focusable title button
        // (Tab, Enter to select); this label makes the row itself announce.
        let branch_suffix = session
            .git_branch
            .as_deref()
            .map(|branch| format!(", branch {branch}"))
            .unwrap_or_default();
        let session_row_label = format!(
            "{}, project {}, {}, {}{}",
            session_title,
            project,
            attention.label(),
            time_ago,
            branch_suffix,
        );

        let pr_info = session_pr_info(session, &self.model.read(cx).git_prs).cloned();

        let pr_meta = pr_info.map(|pr| {
            let state_upper = pr.state.to_uppercase();
            let is_merged = state_upper == "MERGED";
            let is_draft = pr.is_draft || state_upper == "DRAFT";
            let is_closed = state_upper == "CLOSED";
            let tooltip = pr_status_tooltip(&pr);

            let (pr_bg, pr_fg, pr_label, pr_icon) = if is_merged {
                (
                    theme.success.opacity(0.18),
                    theme.success,
                    format!("#{}", pr.number),
                    Icon::default().path("icons/git/branch.svg"),
                )
            } else if is_draft {
                (
                    theme.secondary,
                    theme.muted_foreground,
                    format!("#{}", pr.number),
                    Icon::default().path("icons/git/compare.svg"),
                )
            } else if is_closed {
                (
                    theme.danger.opacity(0.12),
                    theme.danger,
                    format!("#{}", pr.number),
                    Icon::default().path("icons/git/compare.svg"),
                )
            } else {
                (
                    theme.primary.opacity(0.12),
                    theme.primary,
                    format!("#{}", pr.number),
                    Icon::default().path("icons/git/compare.svg"),
                )
            };

            div().flex().flex_none().items_center().gap_1().child(
                Button::new(SharedString::from(format!(
                    "session-pr-{}-{}",
                    session.id, pr.number
                )))
                .icon(pr_icon)
                .label(pr_label)
                .accessibility_label(format!(
                    "Pull request #{}, {}",
                    pr.number,
                    pr_status_label(&pr)
                ))
                .tooltip(tooltip)
                .ghost()
                .xsmall()
                .bg(pr_bg)
                .text_color(pr_fg),
            )
        });

        let mut row2_items = Vec::new();
        row2_items.push(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_1()
                .text_color(theme.muted_foreground)
                .child(
                    Icon::new(IconName::Folder)
                        .xsmall()
                        .text_color(theme.muted_foreground.opacity(0.6)),
                )
                .child(div().min_w_0().truncate().child(project))
                .into_any_element(),
        );

        if let Some(pr_chips) = pr_meta {
            row2_items.push(
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground.opacity(0.4))
                    .child("•")
                    .into_any_element(),
            );
            row2_items.push(pr_chips.into_any_element());
        }

        if session.is_worktree && !session.worktree_available {
            let branch_display = session.git_branch.as_deref().unwrap_or("worktree");
            let tooltip = format!(
                "Worktree unavailable\nBranch: '{branch_display}'\nNot checked out locally\nRecorded path: {}\nSession history remains available",
                session.runtime_work_dir.display()
            );
            row2_items.push(
                Button::new(SharedString::from(format!(
                    "session-worktree-{}",
                    session.id
                )))
                .icon(Icon::default().path("icons/git/branch.svg"))
                .label("Not checked out")
                .accessibility_label(format!(
                    "Worktree unavailable for branch '{branch_display}', not checked out locally"
                ))
                .tooltip(tooltip)
                .ghost()
                .xsmall()
                .bg(theme.warning.opacity(0.12))
                .text_color(theme.warning)
                .into_any_element(),
            );
        }

        div()
            .id(SharedString::from(format!("session-card-{}", session.id)))
            .group("session-card")
            .tooltip(move |window, cx| Tooltip::new(session_tooltip.clone()).build(window, cx))
            .role(Role::ListItem)
            .aria_label(session_row_label.clone())
            .relative()
            .flex()
            .items_stretch()
            .w_full()
            .my(rems(0.09375))
            .rounded_md()
            .bg(bg_color)
            .border_1()
            .border_color(border_color)
            .hover(|style| {
                style.bg(if is_active {
                    theme.sidebar_accent
                } else {
                    theme.list_hover
                })
            })
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                let work_dir = work_dir.clone();
                let session_id = session_id.clone();
                model.update(cx, |state, cx| {
                    controller::dispatch(
                        state,
                        AppAction::SelectSession {
                            work_dir,
                            session_id,
                        },
                    );
                    cx.notify();
                });
            })
            .when(is_active, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_1()
                        .bottom_1()
                        .w(rems(0.15625))
                        .rounded_r_full()
                        .bg(theme.primary.opacity(0.82)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(rems(0.1875))
                    .px_2p5()
                    .py_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                Button::new(SharedString::from(format!(
                                    "session-title-{}",
                                    session.id
                                )))
                                .debug_selector({
                                    let id = session.id.clone();
                                    move || format!("session-title-{id}")
                                })
                                .accessibility_label(session_title.clone())
                                .ghost()
                                .xsmall()
                                .compact()
                                .flex_1()
                                .min_w_0()
                                .px_0()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(move |_, _, cx| {
                                    title_model.update(cx, |state, cx| {
                                        controller::dispatch(
                                            state,
                                            AppAction::SelectSession {
                                                work_dir: title_work_dir.clone(),
                                                session_id: title_session_id.clone(),
                                            },
                                        );
                                        cx.notify();
                                    });
                                })
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .text_sm()
                                        .font_weight(if is_active {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::MEDIUM
                                        })
                                        .text_color(title_color)
                                        .truncate()
                                        .child(session_title),
                                ),
                            )
                            .child(
                                div()
                                    .relative()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap_1()
                                    .child(
                                        div().flex().items_center().gap_1().child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                // Trailing-slot swap (Synara SidebarRowHoverActions
                                                // pattern): timestamp fades out when the hover
                                                // actions appear, so the 223px row never shows
                                                // both at once. Layout width is preserved for
                                                // stability; only visual crowding is removed.
                                                .opacity(1.0)
                                                .group_hover("session-card", |style| {
                                                    style.opacity(0.0)
                                                })
                                                .when(is_active, |this| this.opacity(0.0))
                                                .child(time_ago),
                                        ),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "settle-session-{}",
                                            session.id
                                        )))
                                        .icon(Icon::default().path("icons/archive.svg"))
                                        .ghost()
                                        .xsmall()
                                        .accessibility_label("Archive session")
                                        .opacity(0.0)
                                        .group_hover("session-card", |style| style.opacity(1.0))
                                        .focus_visible(|style| style.opacity(1.0))
                                        // Touch and no-hover users never get
                                        // group_hover: the selected row always
                                        // shows its archive action.
                                        .when(is_active, |button| button.opacity(1.0))
                                        .tooltip("Archive session")
                                        // The card selects a session on mouse-down. Keep action buttons from
                                        // bubbling that event, otherwise archiving first selects the row and
                                        // queues hydration for the file that is about to be archived.
                                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                            cx.stop_propagation();
                                        })
                                        .on_click(
                                            move |_event, window, cx| {
                                                if quick_settle_is_worktree {
                                                    open_archive_session_dialog(
                                                        window,
                                                        cx,
                                                        quick_settle_model.clone(),
                                                        quick_settle_work_dir.clone(),
                                                        quick_settle_session_id.clone(),
                                                        true,
                                                        quick_settle_git_branch.clone(),
                                                    );
                                                } else {
                                                    quick_settle_model.update(cx, |state, cx| {
                                                        controller::dispatch(
                                                            state,
                                                            AppAction::SettleSession {
                                                                work_dir: quick_settle_work_dir
                                                                    .clone(),
                                                                session_id: quick_settle_session_id
                                                                    .clone(),
                                                                delete_worktree: false,
                                                            },
                                                        );
                                                        cx.notify();
                                                    });
                                                }
                                            },
                                        ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_xs()
                            .min_w_0()
                            .flex_wrap()
                            .children(row2_items)
                            .children(status_indicator),
                    ),
            )
            .context_menu(move |menu, _window, _cx| {
                let open_model = context_model.clone();
                let open_work_dir = context_work_dir.clone();
                let open_session_id = context_session_id.clone();
                let copy_session_id = context_session_id.clone();
                let copy_project_path = context_work_dir.to_string_lossy().into_owned();
                let copy_session_file = copy_session_file.clone();
                let export_log_model = context_model.clone();
                let export_log_source = export_log_source.clone();
                let export_log_session_id = context_session_id.clone();
                let export_log_title = export_trajectory_title.clone();
                let export_log_work_dir = context_work_dir.clone();
                let export_trajectory_model = context_model.clone();
                let export_trajectory_source = export_log_source.clone();
                let export_trajectory_session_id = context_session_id.clone();
                let export_trajectory_title = export_trajectory_title.clone();
                let export_trajectory_work_dir = context_work_dir.clone();
                let settle_model = context_model.clone();
                let settle_work_dir = context_work_dir.clone();
                let settle_session_id = context_session_id.clone();
                let settle_is_worktree = context_is_worktree;
                let settle_git_branch = context_git_branch.clone();
                let remove_model = context_model.clone();
                let remove_work_dir = context_work_dir.clone();
                let remove_session_id = context_session_id.clone();
                let remove_is_worktree = context_is_worktree;
                let remove_git_branch = context_git_branch.clone();

                menu.item(PopupMenuItem::new("Open Session").on_click(
                    move |_event, _window, cx| {
                        open_model.update(cx, |state, cx| {
                            controller::dispatch(
                                state,
                                AppAction::SelectSession {
                                    work_dir: open_work_dir.clone(),
                                    session_id: open_session_id.clone(),
                                },
                            );
                            cx.notify();
                        });
                    },
                ))
                .item({
                    let item = PopupMenuItem::new(if terminal_unavailable {
                        "Open Terminal Here — worktree unavailable"
                    } else {
                        "Open Terminal Here"
                    });
                    if terminal_unavailable {
                        item.disabled(true)
                    } else {
                        item.on_click({
                            let terminal_model = terminal_model.clone();
                            let terminal_work_dir = terminal_work_dir.clone();
                            move |_event, _window, cx| {
                                terminal_model.update(cx, |state, cx| {
                                    controller::dispatch(
                                        state,
                                        AppAction::OpenTerminalAt(terminal_work_dir.clone()),
                                    );
                                    cx.notify();
                                });
                            }
                        })
                    }
                })
                .item(
                    PopupMenuItem::new("Copy Session ID").on_click(move |_event, _window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_session_id.clone()));
                    }),
                )
                .item(PopupMenuItem::new("Copy Project Root Path").on_click(
                    move |_event, _window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_project_path.clone()));
                    },
                ))
                .item(PopupMenuItem::new("Copy Session File Path").on_click(
                    move |_event, _window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_session_file.clone()));
                    },
                ))
                .separator()
                .item(PopupMenuItem::new("Export Session Log…").on_click(
                    move |_event, _window, cx| {
                        let model = export_log_model.clone();
                        let source = export_log_source.clone();
                        let session_id = export_log_session_id.clone();
                        let title = export_log_title.clone();
                        let work_dir = export_log_work_dir.clone();
                        let (trajectory, runtime) = model.update(cx, |state, _cx| {
                            (
                                state.session_trajectory(&session_id).to_vec(),
                                Some(
                                    state.ensure_session_runtime(work_dir.clone(), source.clone()),
                                ),
                            )
                        });
                        cx.spawn(async move |cx| {
                            let default_name =
                                format!("{}-session-diagnostics.json", safe_file_stem(&title));
                            let Some(destination) = rfd::AsyncFileDialog::new()
                                .set_file_name(&default_name)
                                .save_file()
                                .await
                            else {
                                return;
                            };
                            // Blocking file + JSON work hops to the background
                            // executor: session logs can be tens of MB, and
                            // this continuation already left the UI thread.
                            let destination_path = destination.path().to_path_buf();
                            let result = cx
                                .background_executor()
                                .spawn(async move {
                                    build_diagnostic_export(
                                        &source,
                                        &session_id,
                                        &title,
                                        &work_dir,
                                        runtime.as_deref(),
                                        trajectory,
                                        true,
                                    )
                                    .and_then(|value| {
                                        serde_json::to_vec_pretty(&value)
                                            .map_err(|error| error.to_string())
                                    })
                                    .and_then(|bytes| {
                                        std::fs::write(&destination_path, bytes)
                                            .map_err(|error| error.to_string())
                                    })
                                })
                                .await;
                            let _ = model.update(cx, |state, cx| {
                                state.session_status = Some(match result {
                                    Ok(()) => "Session diagnostics exported".into(),
                                    Err(error) => {
                                        format!("Could not export session diagnostics: {error}")
                                    }
                                });
                                cx.notify();
                            });
                        })
                        .detach();
                    },
                ))
                .item(PopupMenuItem::new("Export Trajectory…").on_click(
                    move |_event, _window, cx| {
                        let model = export_trajectory_model.clone();
                        let session_id = export_trajectory_session_id.clone();
                        let title = export_trajectory_title.clone();
                        let source = export_trajectory_source.clone();
                        let work_dir = export_trajectory_work_dir.clone();
                        let (trajectory, runtime) = model.update(cx, |state, _cx| {
                            (
                                state.session_trajectory(&session_id).to_vec(),
                                Some(
                                    state.ensure_session_runtime(work_dir.clone(), source.clone()),
                                ),
                            )
                        });
                        cx.spawn(async move |cx| {
                            let default_name =
                                format!("{}-trajectory.json", safe_file_stem(&title));
                            let Some(destination) = rfd::AsyncFileDialog::new()
                                .set_file_name(&default_name)
                                .save_file()
                                .await
                            else {
                                return;
                            };
                            let destination_path = destination.path().to_path_buf();
                            let result = cx
                                .background_executor()
                                .spawn(async move {
                                    build_diagnostic_export(
                                        &source,
                                        &session_id,
                                        &title,
                                        &work_dir,
                                        runtime.as_deref(),
                                        trajectory,
                                        false,
                                    )
                                    .and_then(|value| {
                                        serde_json::to_vec_pretty(&value)
                                            .map_err(|error| error.to_string())
                                    })
                                    .and_then(|bytes| {
                                        std::fs::write(&destination_path, bytes)
                                            .map_err(|error| error.to_string())
                                    })
                                })
                                .await;
                            let _ = model.update(cx, |state, cx| {
                                state.session_status = Some(match result {
                                    Ok(()) => "Trajectory exported".into(),
                                    Err(error) => format!("Could not export trajectory: {error}"),
                                });
                                cx.notify();
                            });
                        })
                        .detach();
                    },
                ))
                .separator()
                .item(
                    PopupMenuItem::new("Archive Session").on_click(move |_event, window, cx| {
                        open_archive_session_dialog(
                            window,
                            cx,
                            settle_model.clone(),
                            settle_work_dir.clone(),
                            settle_session_id.clone(),
                            settle_is_worktree,
                            settle_git_branch.clone(),
                        );
                    }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Remove Session").on_click(move |_event, window, cx| {
                        open_remove_session_dialog(
                            window,
                            cx,
                            remove_model.clone(),
                            remove_work_dir.clone(),
                            remove_session_id.clone(),
                            remove_is_worktree,
                            remove_git_branch.clone(),
                        );
                    }),
                )
            })
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings_model = self.model.clone();
        let theme = cx.theme().colors;
        let settings_selected =
            self.model.read(cx).workspace_page == threadlane_ui_state::WorkspacePage::Settings;

        div().flex_none().px_3().py_2().child(
            Button::new("sidebar-settings")
                .debug_selector(|| "sidebar-settings".into())
                .accessibility_label("Open settings")
                .tooltip("Open settings")
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_start()
                        .gap_2()
                        .child(IconName::Settings)
                        .child("Settings"),
                )
                .ghost()
                .selected(settings_selected)
                .w_full()
                .justify_start()
                .text_color(theme.muted_foreground)
                .on_click(move |_event, _window, cx| {
                    settings_model.update(cx, |state, cx| {
                        controller::dispatch(state, AppAction::OpenSettings);
                        cx.notify();
                    });
                }),
        )
    }

    fn render_github_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.model.read(cx);
        let automation_model = self.model.clone();
        let attention = state.automations.snapshot.runs.iter().filter(|run| run.needs_attention()).count();
        let label = if attention == 0 { "Automations".to_string() } else { format!("Automations · {attention}") };
        div().flex().flex_col().gap_1()
            .child(Button::new("sidebar-automations").debug_selector(|| "sidebar-automations".into())
                .child(div().flex().items_center().gap_2().w_full()
                    .child(Icon::from(IconName::Calendar).size_4()).child(label))
                .accessibility_label(format!("Automations, {attention} runs need attention"))
                .ghost().w_full().justify_start().selected(state.workspace_page == WorkspacePage::Automations)
                .on_click(move |_, _, cx| automation_model.update(cx, |state, cx| {
                    controller::dispatch(state, AppAction::OpenAutomations); cx.notify();
                })))
            .children(
            [
                (GitHubTab::Issues, "sidebar-issues", "icons/git/issue.svg"),
                (
                    GitHubTab::PullRequests,
                    "sidebar-pull-requests",
                    "icons/git/pull-request.svg",
                ),
            ]
            .into_iter()
            .map(|(tab, id, icon)| {
                let model = self.model.clone();
                let label = format!("Open GitHub {}", tab.label().to_lowercase());
                let selected = state.workspace_page == WorkspacePage::GitHub && state.github_tab == tab;
                Button::new(id)
                    .debug_selector(move || id.into())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .w_full()
                            .child(Icon::default().path(icon).size_4())
                            .child(tab.label()),
                    )
                    .accessibility_label(if selected {
                        format!("{label}, current view")
                    } else {
                        label.clone()
                    })
                    .tooltip(label)
                    .ghost()
                    .selected(selected)
                    .w_full()
                    .justify_start()
                    .on_click(move |_event, _window, cx| {
                        model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::OpenGitHubTab(tab));
                            cx.notify();
                        });
                    })
            }),
        )
    }

    /// Filter, group, and sort sessions for the history list. Only runs when
    /// `sidebar_fingerprint` changes; `render_history` otherwise reuses the
    /// cached result instead of cloning and sorting every row per frame.
    fn build_history_rows(&self, state: &AppState, query: &str, now: u64) -> Vec<HistoryRow> {
        let mut sessions = Vec::new();
        let mut seen_sessions = std::collections::HashSet::new();
        for project in state.projects.iter().filter(|project| {
            state
                .sidebar_project_filter
                .as_ref()
                .is_none_or(|selected| &project.work_dir == selected)
        }) {
            let project_name = project.name.to_lowercase();
            for session in project.sessions.iter() {
                if !seen_sessions.insert((session.work_dir.clone(), session.id.clone())) {
                    continue;
                }
                // Sidebar search matches title, id, project, branch, and
                // directory name so filtered tasks stay findable by context.
                if !history_query_matches(
                    &session.title.to_lowercase(),
                    &session.id.to_lowercase(),
                    &project_name,
                    &session
                        .git_branch
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase(),
                    &session
                        .work_dir
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_lowercase(),
                    query,
                ) {
                    continue;
                }
                let attention = state.session_attention(session);
                if self
                    .attention_filter
                    .is_some_and(|filter| filter != attention)
                {
                    continue;
                }
                sessions.push((session.clone(), attention));
            }
        }
        flatten_history_sessions(sessions, now)
    }

    fn render_history_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        match self
            .history_cache
            .as_ref()
            .and_then(|(_, rows)| rows.get(index))
            .cloned()
        {
            Some(HistoryRow::Group(group)) => div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .pt(if index == 0 {
                    window.rem_size() * 0.25
                } else {
                    window.rem_size() * 0.75
                })
                .pb_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground.opacity(0.8))
                        .child(group.label()),
                )
                .child(
                    div()
                        .h(rems(0.0625))
                        .flex_1()
                        .bg(theme.border.opacity(0.35)),
                )
                .into_any_element(),
            Some(HistoryRow::Session(session, attention)) => {
                let state = self.model.read(cx);
                let is_active = state.workspace_page == WorkspacePage::Chat
                    && state.active_work_dir.as_ref() == Some(&session.work_dir)
                    && state.active_session_id.as_deref() == Some(session.id.as_str());
                self.render_session_card(&session, attention, is_active, cx)
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    fn render_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let state = self.model.read(cx);
        let query = state.search_query.trim().to_lowercase();
        let has_filters = self.has_history_filters(state);
        let now = now_unix_secs();

        let mut fingerprint = sidebar_fingerprint(state, now);
        if let Some(filter) = self.attention_filter {
            use std::hash::{Hash, Hasher};
            let mut filter_hasher = std::collections::hash_map::DefaultHasher::new();
            filter.label().hash(&mut filter_hasher);
            fingerprint ^= filter_hasher.finish();
        }
        self.history_fingerprint = fingerprint;
        let cache_matches = self
            .history_cache
            .as_ref()
            .is_some_and(|(cached, _)| *cached == fingerprint);
        if !cache_matches {
            let rows = self.build_history_rows(state, &query, now);
            let same_rows = self.history_cache.as_ref().is_some_and(|(_, cached)| {
                cached.len() == rows.len()
                    && cached
                        .iter()
                        .zip(&rows)
                        .all(|(left, right)| same_history_row_identity(left, right))
            });
            if !same_rows {
                self.history_list_state.reset(rows.len());
            }
            self.history_cache = Some((fingerprint, rows));
        }

        let row_count = self
            .history_cache
            .as_ref()
            .map_or(0, |(_, rows)| rows.len());
        if row_count == 0 {
            return div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .px_4()
                .py_6()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if has_filters {
                    "No matching tasks"
                } else {
                    "No tasks yet"
                })
                .when(has_filters, |this| {
                    this.child(
                        Button::new("empty-history-clear-filters")
                            .debug_selector(|| "empty-history-clear-filters".into())
                            .label("Clear filters")
                            .tooltip("Clear search, project, and status filters")
                            .outline()
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.clear_history_filters(window, cx);
                            })),
                    )
                })
                .children((!has_filters).then(|| {
                    Button::new("empty-history-new-task")
                        .icon(IconName::Plus)
                        .label("New task")
                        .outline()
                        .small()
                        .accessibility_label("Start a new task")
                        .tooltip("Start a new task (⌘N)")
                        .on_click(move |_event, window, cx| {
                            window.dispatch_action(Box::new(crate::BeginNewTask), cx);
                        })
                }))
                .into_any_element();
        }

        div()
            .relative()
            .size_full()
            .child(
                list(
                    self.history_list_state.clone(),
                    cx.processor(Self::render_history_row),
                )
                .size_full()
                .pb_3()
                .with_sizing_behavior(ListSizingBehavior::Auto),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(gpui_component::scroll::Scrollbar::vertical(
                        &self.history_list_state,
                    )),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DateGroup, HistoryRow, flatten_history_sessions, format_time_ago, history_query_matches,
        pr_status_label, pr_status_tooltip, same_history_row_identity, session_pr_info,
        sidebar_session_fingerprint, sidebar_session_identity,
    };
    use std::collections::HashMap;
    use threadlane_git::GitHubPrInfo;
    use threadlane_ui_state::{SessionAttention, SessionHealth, SessionInfo};

    fn session(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            title: id.into(),
            work_dir: "/project".into(),
            runtime_work_dir: "/project".into(),
            session_file: format!("/project/{id}.jsonl").into(),
            updated_at: 0,
            health: SessionHealth::Healthy,
            git_branch: None,
            github_issue: None,
            is_worktree: false,
            worktree_available: true,
        }
    }

    #[gpui::test]
    fn sidebar_task_title_and_navigation_support_keyboard_activation(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::*;
        use std::{cell::Cell, rc::Rc};
        use threadlane_ui_state::{AppState, GitHubTab, WorkspacePage};

        struct Harness {
            sidebar: Entity<super::SidebarView>,
            session: SessionInfo,
        }

        impl Render for Harness {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                self.sidebar.update(cx, |sidebar, cx| {
                    div()
                        .tab_group()
                        .w(rems(13.9375))
                        .child(sidebar.render_session_card(
                            &self.session,
                            SessionAttention::NeedsYou,
                            false,
                            cx,
                        ))
                        .child(sidebar.render_github_nav(cx))
                        .child(sidebar.render_footer(cx))
                        .into_any_element()
                })
            }
        }

        cx.update(gpui_component::init);
        let temporary = tempfile::tempdir().unwrap();
        let mut task = session("keyboard-task");
        // A missing project cannot be persisted to the user's project registry.
        task.work_dir = temporary.path().join("missing-project");
        let (harness, cx) = cx.add_window_view(|window, cx| Harness {
            sidebar: cx.new(|cx| {
                let model = cx.new(|_| {
                    let mut state = AppState::default();
                    state.active_work_dir = None;
                    state.active_session_id = None;
                    state.pending_hydrations.clear();
                    state
                });
                super::SidebarView::new(model, window, cx)
            }),
            session: task.clone(),
        });
        let model = harness.read_with(cx, |harness, cx| harness.sidebar.read(cx).model.clone());
        let changes = Rc::new(Cell::new(0));
        let _subscription = cx.update(|_, cx| {
            let changes = changes.clone();
            cx.observe(&model, move |_, _| changes.set(changes.get() + 1))
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let attention = cx.debug_bounds("session-attention-keyboard-task").unwrap();
        assert!(
            attention.right() <= px(223.0),
            "attention must not clip in a narrow sidebar"
        );
        let title = cx.debug_bounds("session-title-keyboard-task").unwrap();
        assert!(
            title.size.width > px(0.0),
            "the title must retain usable width"
        );
        cx.simulate_click(title.center(), Modifiers::default());
        assert_eq!(changes.get(), 1, "title click must select only once");
        model.read_with(cx, |state, _| {
            assert_eq!(state.active_session_id.as_deref(), Some("keyboard-task"));
            assert_eq!(state.pending_hydrations.len(), 1);
        });

        model.update(cx, |state, _| state.active_session_id = None);
        cx.update(|window, cx| {
            window.blur(cx);
            window.focus_next(cx);
            window.draw(cx).clear(cx);
        });
        for key in ["enter", "space"] {
            let previous_changes = changes.get();
            let keystroke = Keystroke::parse(key).unwrap();
            cx.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(KeyUpEvent { keystroke });
            assert_eq!(changes.get(), previous_changes + 1);
            model.read_with(cx, |state, _| {
                assert_eq!(state.active_session_id.as_deref(), Some("keyboard-task"));
            });
        }

        cx.update(|window, cx| window.focus_next(cx)); // Archive remains separate.
        for (page, tab) in [
            (WorkspacePage::Automations, GitHubTab::Issues),
            (WorkspacePage::GitHub, GitHubTab::Issues),
            (WorkspacePage::GitHub, GitHubTab::PullRequests),
            (WorkspacePage::Settings, GitHubTab::PullRequests),
        ] {
            cx.update(|window, cx| {
                window.focus_next(cx);
                window.draw(cx).clear(cx);
            });
            let keystroke = Keystroke::parse("enter").unwrap();
            cx.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(KeyUpEvent { keystroke });
            model.read_with(cx, |state, _| {
                assert_eq!(state.workspace_page, page);
                assert_eq!(state.github_tab, tab);
                assert_eq!(state.active_session_id.as_deref(), Some("keyboard-task"));
            });
        }
        for (id, tab) in [
            ("sidebar-issues", GitHubTab::Issues),
            ("sidebar-pull-requests", GitHubTab::PullRequests),
        ] {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = cx.debug_bounds(id).unwrap();
            assert!(bounds.right() <= px(223.0));
            cx.simulate_click(bounds.center(), Modifiers::default());
            model.read_with(cx, |state, _| {
                assert_eq!(state.workspace_page, WorkspacePage::GitHub);
                assert_eq!(state.github_tab, tab);
            });
        }
    }

    #[test]
    fn history_rows_prioritize_attention_then_keep_date_order() {
        let now = 700_000;
        let mut needs_older = session("needs-older");
        needs_older.updated_at = now - 200;
        let mut needs_newer = session("needs-newer");
        needs_newer.updated_at = now - 100;
        let mut working = session("working");
        working.updated_at = now - 300;
        let mut ready_today = session("ready-today");
        ready_today.updated_at = now - 400;
        let mut idle_yesterday = session("idle-yesterday");
        idle_yesterday.updated_at = now - 90_000;

        let rows = flatten_history_sessions(
            vec![
                (ready_today, SessionAttention::Ready),
                (needs_older, SessionAttention::NeedsYou),
                (idle_yesterday, SessionAttention::Idle),
                (working, SessionAttention::Working),
                (needs_newer, SessionAttention::NeedsYou),
            ],
            now,
        );

        assert!(matches!(rows[0], HistoryRow::Group(DateGroup::NeedsYou)));
        assert!(
            matches!(&rows[1], HistoryRow::Session(item, SessionAttention::NeedsYou) if item.id == "needs-newer")
        );
        assert!(
            matches!(&rows[2], HistoryRow::Session(item, SessionAttention::NeedsYou) if item.id == "needs-older")
        );
        assert!(matches!(rows[3], HistoryRow::Group(DateGroup::Working)));
        assert!(
            matches!(&rows[4], HistoryRow::Session(item, SessionAttention::Working) if item.id == "working")
        );
        assert!(matches!(rows[5], HistoryRow::Group(DateGroup::Today)));
        assert!(
            matches!(&rows[6], HistoryRow::Session(item, SessionAttention::Ready) if item.id == "ready-today")
        );
        assert!(matches!(rows[7], HistoryRow::Group(DateGroup::Yesterday)));
        assert!(
            matches!(&rows[8], HistoryRow::Session(item, SessionAttention::Idle) if item.id == "idle-yesterday")
        );
        assert!(same_history_row_identity(
            &rows[1],
            &HistoryRow::Session(session("needs-newer"), SessionAttention::Idle)
        ));
        assert!(!same_history_row_identity(&rows[1], &rows[4]));
    }

    #[test]
    fn recent_timestamps_use_stable_labels() {
        assert_eq!(format_time_ago(100, 100), "Just now");
        assert_eq!(format_time_ago(41, 100), "Just now");
        assert_eq!(format_time_ago(40, 100), "1m ago");
    }

    #[test]
    fn history_search_matches_context_beyond_title_and_id() {
        assert!(history_query_matches(
            "fix login",
            "abc",
            "mypi",
            "main",
            "mypi",
            ""
        ));
        assert!(history_query_matches(
            "fix login",
            "abc",
            "mypi",
            "main",
            "mypi",
            "login"
        ));
        assert!(history_query_matches(
            "fix login",
            "abc123",
            "mypi",
            "main",
            "mypi",
            "abc"
        ));
        assert!(history_query_matches(
            "other", "abc", "mypi", "main", "mypi", "mypi"
        ));
        assert!(history_query_matches(
            "other",
            "abc",
            "mypi",
            "feature/search",
            "mypi",
            "search"
        ));
        assert!(history_query_matches(
            "other",
            "abc",
            "mypi",
            "main",
            "checkout-dir",
            "checkout"
        ));
        assert!(!history_query_matches(
            "other", "abc", "mypi", "main", "mypi", "zzz"
        ));
    }

    #[test]
    fn sessions_in_one_project_use_their_own_branch_pr() {
        let mut first = session("first");
        first.git_branch = Some("feature/one".into());
        let mut second = session("second");
        second.git_branch = Some("feature/two".into());
        let prs = HashMap::from([
            (
                (first.work_dir.clone(), "feature/one".into()),
                Some(GitHubPrInfo {
                    number: 11,
                    ..Default::default()
                }),
            ),
            (
                (second.work_dir.clone(), "feature/two".into()),
                Some(GitHubPrInfo {
                    number: 22,
                    ..Default::default()
                }),
            ),
        ]);

        assert_eq!(session_pr_info(&first, &prs).unwrap().number, 11);
        assert_eq!(session_pr_info(&second, &prs).unwrap().number, 22);
    }

    #[test]
    fn merged_pr_tooltip_exposes_status_checks_and_discussion() {
        let pr = GitHubPrInfo {
            number: 114,
            title: "Improve review flow".into(),
            url: "https://example.test/pull/114".into(),
            state: "MERGED".into(),
            head_ref: "feature/review".into(),
            base_ref: "main".into(),
            comments_count: 9,
            passing_checks: 9,
            ..Default::default()
        };

        assert_eq!(pr_status_label(&pr), "Merged");
        let tooltip = pr_status_tooltip(&pr);
        assert!(tooltip.contains("PR #114 · Merged"));
        assert!(tooltip.contains("feature/review → main"));
        assert!(tooltip.contains("Checks: 9 passed"));
        assert!(tooltip.contains("Discussion: 9 comments"));
        assert!(tooltip.contains("https://example.test/pull/114"));
    }

    #[test]
    fn changing_a_session_branch_changes_the_sidebar_fingerprint() {
        let mut item = session("session");
        item.git_branch = Some("feature/one".into());
        let first = sidebar_session_fingerprint(&item, SessionAttention::Idle);

        item.git_branch = Some("feature/two".into());

        assert_ne!(
            first,
            sidebar_session_fingerprint(&item, SessionAttention::Idle)
        );
    }

    #[test]
    fn attention_changes_the_sidebar_fingerprint() {
        let item = session("session");

        assert_ne!(
            sidebar_session_fingerprint(&item, SessionAttention::Idle),
            sidebar_session_fingerprint(&item, SessionAttention::NeedsYou)
        );
    }

    #[test]
    fn github_issue_identity_prefixes_titles_once_and_refreshes_sidebar() {
        let mut item = session("session");
        item.title = "Fix linked task browser".into();
        item.github_issue = Some(threadlane_git::GitHubIssueRef {
            host: "github.com".into(),
            owner: "threadlane".into(),
            repo: "app".into(),
            number: 42,
            url: "https://github.com/threadlane/app/issues/42".into(),
        });

        let before = sidebar_session_fingerprint(&item, SessionAttention::Idle);
        let identity = sidebar_session_identity(&item);
        assert_eq!(identity.title, "#42 Fix linked task browser");
        assert!(identity.tooltip.contains("threadlane/app"));
        assert!(identity.tooltip.contains("Fix linked task browser"));

        item.title = "#42 Fix linked task browser".into();
        assert_eq!(
            sidebar_session_identity(&item).title,
            "#42 Fix linked task browser"
        );

        item.title = "#42".into();
        assert_eq!(sidebar_session_identity(&item).title, "#42");

        item.github_issue = Some(threadlane_git::GitHubIssueRef {
            number: 43,
            ..item.github_issue.clone().unwrap()
        });
        assert_ne!(
            before,
            sidebar_session_fingerprint(&item, SessionAttention::Idle)
        );
    }
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(theme.title_bar)
            .child(self.render_header(cx))
            .child(self.render_project_filter(cx))
            .child(self.render_history_header(cx))
            .child(div().flex_1().min_h_0().child(self.render_history(cx)))
            .child(self.render_footer(cx))
    }
}
