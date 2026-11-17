use threadlane_ui_kit::DateGroup;
use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::InteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;

use gpui_component::button::{Button, ButtonVariants};
use gpui_component::progress::Progress;
use gpui_component::theme::ActiveTheme;
use gpui_component::{Disableable, Icon, Sizable, WindowExt};

use threadlane_ui_state::{
    snooze_return_label, AppState, GitHubTab, SessionAttention, SessionInfo, SessionSnooze,
    TrajectoryEntry, WorkspacePage, SNOOZE_OPTIONS,
};
use threadlane_ui_state::{actions::AppAction, controller};
use threadlane_updater::UpdateStatus;

use threadlane_ui_kit::SidebarSessionRemoval as SessionRemovalKind;

fn session_card_git_status<'a>(statuses: &'a std::collections::HashMap<PathBuf, threadlane_git::GitStatus>, session: &SessionInfo) -> Option<&'a threadlane_git::GitStatus> {
    if session.is_worktree && !session.worktree_available {
        return None;
    }
    statuses.get(&session.runtime_work_dir)
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
            let target = threadlane_ui_kit::SidebarSessionRemovalTarget::new(
                &session_id,
                title,
                project_name,
            );
            let target = if is_worktree {
                target.worktree(git_branch.clone())
            } else {
                target
            };
            let toggle = delete_worktree.clone();
            let toggle_model = model.clone();
            let alert = threadlane_ui_kit::sidebar_session_removal_dialog(
                alert,
                kind,
                &target,
                delete_worktree.get(),
                move |checked, _, cx| {
                    toggle.set(checked);
                    toggle_model.update(cx, |_, cx| cx.notify());
                },
            );

            alert.on_ok(move |_event, _window, cx| {
                let delete_worktree_val = if is_worktree {
                    delete_worktree.get()
                } else {
                    false
                };
                model.update(cx, |state, cx| {
                    controller::dispatch(
                        state,
                        match kind {
                            SessionRemovalKind::Archive => AppAction::SettleSession {
                                work_dir: work_dir.clone(),
                                session_id: session_id.clone(),
                                delete_worktree: delete_worktree_val,
                            },
                            SessionRemovalKind::Remove => AppAction::RemoveSession {
                                work_dir: work_dir.clone(),
                                session_id: session_id.clone(),
                                delete_worktree: delete_worktree_val,
                            },
                        },
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


#[derive(Clone)]
enum HistoryRow {
    Group(DateGroup),
    /// The `Snoozed (n)` collapsible header; the count travels with the row
    /// so rendering stays a pure function of cached rows.
    SnoozedHeader(usize),
    /// The bool records whether the session showed a New result marker when
    /// the rows were built, so a marker toggling on an otherwise identical
    /// row can invalidate its cached height; the SessionSnooze plays the
    /// same role for the snooze chip.
    Session(SessionInfo, SessionAttention, bool, Option<SessionSnooze>),
}

fn same_history_row_identity(left: &HistoryRow, right: &HistoryRow) -> bool {
    match (left, right) {
        (HistoryRow::Group(left), HistoryRow::Group(right)) => left == right,
        (HistoryRow::SnoozedHeader(_), HistoryRow::SnoozedHeader(_)) => true,
        (HistoryRow::Session(left, ..), HistoryRow::Session(right, ..)) => {
            left.id == right.id && left.work_dir == right.work_dir
        }
        _ => false,
    }
}

/// Signals affecting a session row's height (the signals row renders only
/// when at least one applies). Compared when row identities match so the
/// ListState can remeasure exactly the rows that gained or lost height.
fn history_row_height_inputs(left: &HistoryRow, right: &HistoryRow) -> bool {
    match (left, right) {
        (
            HistoryRow::Session(_, left_attention, left_unseen, left_snooze),
            HistoryRow::Session(_, right_attention, right_unseen, right_snooze),
        ) => {
            left_attention != right_attention
                || left_unseen != right_unseen
                || left_snooze != right_snooze
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

#[cfg(test)]
fn flatten_history_sessions(
    sessions: Vec<(SessionInfo, SessionAttention)>,
    now: u64,
) -> Vec<HistoryRow> {
    flatten_history_sessions_with_pins(
        sessions
            .into_iter()
            .map(|(s, a)| (s, a, false, false, None))
            .collect(),
        now,
        false,
    )
}

fn flatten_history_sessions_with_pins(
    mut sessions: Vec<(SessionInfo, SessionAttention, bool, bool, Option<SessionSnooze>)>,
    now: u64,
    snoozed_collapsed: bool,
) -> Vec<HistoryRow> {
    // Confirmed snoozes group under Snoozed regardless of pin or attention —
    // the pin/attention grouping is preserved underneath and reasserts at
    // return. Pending (unconfirmed) snoozes keep their normal group.
    let group_of = |pinned: bool,
                    attention: SessionAttention,
                    updated_at: u64,
                    snooze: Option<SessionSnooze>| {
        if snooze.is_some_and(|snooze| !snooze.pending) {
            DateGroup::Snoozed
        } else {
            history_group_with_pin(pinned, attention, updated_at, now)
        }
    };
    sessions.sort_by(|left, right| {
        let left_group = group_of(left.2, left.1, left.0.updated_at, left.4);
        let right_group = group_of(right.2, right.1, right.0.updated_at, right.4);
        left_group
            .rank()
            .cmp(&right_group.rank())
            .then_with(|| match (left_group, left.4, right.4) {
                // Snoozed rows order by soonest return; everything else by recency.
                (DateGroup::Snoozed, Some(left_snooze), Some(right_snooze)) => {
                    left_snooze.wake_at.cmp(&right_snooze.wake_at)
                }
                _ => right.0.updated_at.cmp(&left.0.updated_at),
            })
            .then_with(|| left.0.title.cmp(&right.0.title))
    });

    let snoozed_count = sessions
        .iter()
        .filter(|session| session.4.is_some_and(|snooze| !snooze.pending))
        .count();
    let mut rows = Vec::with_capacity(sessions.len() + DateGroup::COUNT);
    let mut previous_group = None;
    for (session, attention, pinned, has_unseen_result, snooze) in sessions {
        let group = group_of(pinned, attention, session.updated_at, snooze);
        if previous_group != Some(group) {
            rows.push(if group == DateGroup::Snoozed {
                HistoryRow::SnoozedHeader(snoozed_count)
            } else {
                HistoryRow::Group(group)
            });
            previous_group = Some(group);
        }
        // Collapse hides the rows, never the header; search bypasses the
        // collapse because the caller passes `snoozed_collapsed` only when
        // the query is empty.
        if group == DateGroup::Snoozed && snoozed_collapsed {
            continue;
        }
        rows.push(HistoryRow::Session(
            session,
            attention,
            has_unseen_result,
            snooze,
        ));
    }
    rows
}


fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}


use threadlane_ui_kit::session_history_group as history_group_with_pin;

fn update_control_label(status: &UpdateStatus) -> Option<String> {
    Some(match status {
        UpdateStatus::Idle | UpdateStatus::UpToDate => return None,
        UpdateStatus::Checking => "Checking for updates".into(),
        UpdateStatus::Available(info) => format!("Download Threadlane {}", info.version),
        UpdateStatus::Downloading { version, progress } => format!(
            "Downloading Threadlane {version}: {}%",
            (progress.clamp(0.0, 1.0) * 100.0).round()
        ),
        UpdateStatus::ReadyToInstall { info, .. } => {
            format!("Restart to install Threadlane {}", info.version)
        }
        UpdateStatus::Installing => "Installing update; Threadlane will restart".into(),
        UpdateStatus::Error(error) => {
            let detail: String = error.chars().take(160).collect();
            let suffix = if error.chars().count() > 160 {
                "…"
            } else {
                ""
            };
            format!("Update failed: {detail}{suffix}. Retry update check")
        }
    })
}

pub struct SidebarView {
    model: Entity<AppState>,
    /// Hash of the model state the sidebar renders; lets the observer skip
    /// notifications for streaming updates that cannot change any row.
    history_fingerprint: u64,
    title_generating: HashSet<PathBuf>,
    update_label: Option<String>,
    /// Flattened, sorted rows cached per fingerprint for the virtual list.
    history_cache: Option<(u64, Vec<HistoryRow>)>,
    history_list_state: ListState,
    /// Snoozed section collapsed state — view-owned, bypassed by search.
    snoozed_collapsed: bool,
    /// The deadline the view-owned wake task is sleeping for; `None` when
    /// no snooze records exist.
    snooze_deadline_armed: Option<u64>,
    /// Bumped on every re-arm so a superseded task exits instead of
    /// double-firing the reconcile.
    snooze_timer_epoch: u64,
    _subscriptions: Vec<Subscription>,
}

fn sidebar_session_fingerprint(
    session: &SessionInfo,
    attention: SessionAttention,
    has_unseen_result: bool,
) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    threadlane_ui_state::hash_session_identity(&mut hasher, session);
    session.work_dir.hash(&mut hasher);
    session.session_file.hash(&mut hasher);
    session.updated_at.hash(&mut hasher);
    has_unseen_result.hash(&mut hasher);
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

#[cfg(test)]
use threadlane_ui_kit::session_identity as sidebar_session_identity;
#[cfg(test)]
use threadlane_ui_kit::{sidebar_pr_status_label as pr_status_label, sidebar_pr_status_tooltip as pr_status_tooltip, session_time_ago as format_time_ago};

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
    state.is_generating.hash(&mut hasher);
    state.active_session_is_loading().hash(&mut hasher);
    for (work_dir, id) in &state.pinned_sessions {
        work_dir.hash(&mut hasher);
        id.hash(&mut hasher);
    }
    for byte in state.search_query.trim().bytes() {
        hasher.write_u8(byte.to_ascii_lowercase());
    }
    // Snooze records are hashed in full — a deadline is never folded into
    // the minute bucket, so an expiry changes the rows only through the
    // reconcile that drops the record (driven by the view's wake task).
    for (work_dir, session_id, wake_at, pending, save_failed) in
        state.session_snooze_entries()
    {
        work_dir.hash(&mut hasher);
        session_id.hash(&mut hasher);
        wake_at.hash(&mut hasher);
        pending.hash(&mut hasher);
        save_failed.hash(&mut hasher);
    }
    (now / 60).hash(&mut hasher);
    for project in &state.projects {
        project.name.hash(&mut hasher);
        project.work_dir.hash(&mut hasher);
        for session in &project.sessions {
            sidebar_session_fingerprint(
                session,
                state.session_attention(session),
                state.session_has_unseen_result(session),
            )
            .hash(&mut hasher);
        }
    }
    // Hash-map iteration is stable between notifications unless the map changes;
    // avoiding temporary sorted key vectors keeps streaming notifications cheap.
    for (work_dir, status) in &state.git_statuses {
        work_dir.hash(&mut hasher);
        status.branch.hash(&mut hasher);
        status.files.len().hash(&mut hasher);
        let additions: u32 = status.files.iter().map(|f| f.additions).sum();
        let deletions: u32 = status.files.iter().map(|f| f.deletions).sum();
        additions.hash(&mut hasher);
        deletions.hash(&mut hasher);
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
    if let Some(ctx) = state.active_context_window() {
        ctx.current_tokens.hash(&mut hasher);
        ctx.context_limit.hash(&mut hasher);
        ctx.effective_model.hash(&mut hasher);
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

fn export_sidebar_session(
    model: Entity<AppState>,
    session: &SessionInfo,
    include_log: bool,
    cx: &mut App,
) {
    let source = session.session_file.clone();
    let session_id = session.id.clone();
    let title = session.title.clone();
    let work_dir = session.work_dir.clone();
    let (trajectory, runtime) = model.update(cx, |state, _cx| {
        (
            state.session_trajectory(&session_id).to_vec(),
            Some(state.ensure_session_runtime(work_dir.clone(), source.clone())),
        )
    });
    cx.spawn(async move |cx| {
        let default_name = format!(
            "{}-{}.json",
            safe_file_stem(&title),
            if include_log {
                "session-diagnostics"
            } else {
                "trajectory"
            }
        );
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
                    include_log,
                )
                .and_then(|value| {
                    serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())
                })
                .and_then(|bytes| {
                    std::fs::write(&destination_path, bytes).map_err(|error| error.to_string())
                })
            })
            .await;
        let _ = model.update(cx, |state, cx| {
            state.session_status = Some(match result {
                Ok(()) => if include_log {
                    "Session diagnostics exported"
                } else {
                    "Trajectory exported"
                }
                .into(),
                Err(error) => {
                    format!(
                        "Could not export {}: {error}",
                        if include_log {
                            "session diagnostics"
                        } else {
                            "trajectory"
                        }
                    )
                }
            });
            cx.notify();
        });
    })
    .detach();
}

fn execute_sidebar_session_action(
    action: threadlane_ui_kit::SidebarSessionAction,
    model: &Entity<AppState>,
    view: &WeakEntity<SidebarView>,
    session: &SessionInfo,
    window: &mut Window,
    cx: &mut App,
) {
    use threadlane_ui_kit::SidebarSessionAction as Action;
    let intent = match action {
        Action::Open => AppAction::SelectSession {
            work_dir: session.work_dir.clone(),
            session_id: session.id.clone(),
        },
        Action::TogglePin => AppAction::TogglePinSession {
            work_dir: session.work_dir.clone(),
            session_id: session.id.clone(),
        },
        Action::Snooze(duration_secs) => AppAction::SnoozeSession {
            work_dir: session.work_dir.clone(),
            session_id: session.id.clone(),
            duration_secs,
        },
        Action::Unsnooze => AppAction::UnsnoozeSession {
            work_dir: session.work_dir.clone(),
            session_id: session.id.clone(),
        },
        Action::RetrySnooze => AppAction::RetrySnoozeSave {
            work_dir: session.work_dir.clone(),
            session_id: session.id.clone(),
        },
        Action::OpenTerminal => AppAction::OpenTerminalAt(session.runtime_work_dir.clone()),
        Action::RegenerateTitle => {
            let _ = view.update(cx, |view, cx| {
                view.regenerate_title(session.clone(), window, cx)
            });
            return;
        }
        Action::Fork => {
            let work = model
                .read(cx)
                .prepare_session_fork(session.work_dir.clone(), session.id.clone());
            let work = match work {
                Ok(work) => work,
                Err(error) => {
                    window.push_notification(error, cx);
                    return;
                }
            };
            window.push_notification("Forking session… The fork will use the same checkout.", cx);
            let model = model.clone();
            let project = session.work_dir.clone();
            cx.spawn(async move |cx| {
                let result = cx.background_executor().spawn(async move { work() }).await;
                let _ = model.update(cx, |state, cx| {
                    match result {
                        Ok((id, sessions)) => {
                            state.finish_session_fork(project, id, sessions);
                        }
                        Err(error) => {
                            state.session_status = Some(format!("Could not fork session: {error}"));
                        }
                    }
                    cx.notify();
                });
            })
            .detach();

            return;
        }
        Action::CopyId | Action::CopyProjectPath | Action::CopySessionFile => {
            let text = match action {
                Action::CopyId => session.id.clone(),
                Action::CopyProjectPath => session.work_dir.display().to_string(),
                _ => session.session_file.display().to_string(),
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            return;
        }
        Action::ExportLog | Action::ExportTrajectory => {
            export_sidebar_session(model.clone(), session, action == Action::ExportLog, cx);
            return;
        }
        Action::Archive => {
            open_archive_session_dialog(
                window,
                cx,
                model.clone(),
                session.work_dir.clone(),
                session.id.clone(),
                session.is_worktree,
                session.git_branch.clone(),
            );
            return;
        }
        Action::Remove => {
            open_remove_session_dialog(
                window,
                cx,
                model.clone(),
                session.work_dir.clone(),
                session.id.clone(),
                session.is_worktree,
                session.git_branch.clone(),
            );
            return;
        }
    };
    model.update(cx, |state, cx| {
        controller::dispatch(state, intent);
        cx.notify();
    });
}

fn sidebar_snooze_status(snooze: SessionSnooze) -> threadlane_ui_kit::SidebarSnoozeStatus {
    if snooze.pending {
        if snooze.save_failed {
            threadlane_ui_kit::SidebarSnoozeStatus::SaveFailed
        } else {
            threadlane_ui_kit::SidebarSnoozeStatus::Saving
        }
    } else {
        threadlane_ui_kit::SidebarSnoozeStatus::Snoozed(snooze_return_label(snooze.wake_at))
    }
}

fn render_sidebar_session_menu(
    menu: gpui_component::menu::PopupMenu,
    model: Entity<AppState>,
    view: WeakEntity<SidebarView>,
    session: SessionInfo,
    scope: threadlane_ui_kit::SidebarSessionMenuScope,
    window: &mut Window,
    cx: &mut Context<gpui_component::menu::PopupMenu>,
) -> gpui_component::menu::PopupMenu {
    use threadlane_ui_kit::{SidebarSessionMenuState, SidebarSnoozeChoice, SidebarSnoozeMenu};
    let state = model.read(cx);
    let snooze = match state.session_snooze(&session.work_dir, &session.id) {
        Some(snooze) => SidebarSnoozeMenu::Status(sidebar_snooze_status(snooze)),
        None => match state.session_snooze_eligibility(&session) {
            Ok(()) => SidebarSnoozeMenu::Available(
                SNOOZE_OPTIONS
                    .iter()
                    .map(|(label, secs)| {
                        SidebarSnoozeChoice::new(*label, *secs)
                            .with_return_label(snooze_return_label(now_unix_secs() + secs))
                    })
                    .collect(),
            ),
            Err(reason) => SidebarSnoozeMenu::Unavailable(reason),
        },
    };
    let title_generating = view.upgrade().is_some_and(|view| {
        view.read(cx)
            .title_generating
            .contains(&session.session_file)
    });
    let menu_state = SidebarSessionMenuState::new(snooze)
        .pinned(state.is_session_pinned(&session.work_dir, &session.id))
        .title_generating(title_generating)
        .title_loading(
            state.active_session_matches(&session.id, &session.session_file)
                && state.active_session_is_loading(),
        )
        .terminal_available(!session.is_worktree || session.worktree_available);
    threadlane_ui_kit::sidebar_session_menu(
        menu,
        menu_state,
        scope,
        move |action, window, cx| {
            execute_sidebar_session_action(action, &model, &view, &session, window, cx)
        },
        window,
        cx,
    )
}

impl SidebarView {
    pub fn new(model: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub1 = cx.observe(&model, |this, model, cx| {
            let fingerprint = sidebar_fingerprint(model.read(cx), now_unix_secs());
            let update_label = update_control_label(&model.read(cx).update_status);
            if this.history_fingerprint != fingerprint || this.update_label != update_label {
                this.update_label = update_label;
                this.history_fingerprint = fingerprint;
                cx.notify();
            }
        });

        let history_fingerprint = sidebar_fingerprint(model.read(cx), now_unix_secs());
        let update_label = update_control_label(&model.read(cx).update_status);
        Self {
            model,
            update_label,
            history_fingerprint,
            history_cache: None,
            title_generating: HashSet::new(),
            history_list_state: ListState::new(0, ListAlignment::Top, window.rem_size() * 4.5),
            snoozed_collapsed: false,
            snooze_deadline_armed: None,
            snooze_timer_epoch: 0,
            _subscriptions: vec![sub1],
        }
    }

    fn regenerate_title(
        &mut self,
        session: SessionInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title_generating.contains(&session.session_file) {
            return;
        }
        let state = self.model.read(cx);
        let active = state.active_session_matches(&session.id, &session.session_file);
        if active && state.active_session_is_loading() {
            return;
        }
        let messages = active.then(|| state.messages.clone());
        let model = state.selected_model.clone();
        let runtime = match threadlane_ui_state::chat::executor() {
            Ok(runtime) => runtime,
            Err(error) => {
                window.push_notification(error, cx);
                return;
            }
        };
        self.title_generating.insert(session.session_file.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let source = session.session_file.clone();
            let prompt = cx.background_executor().spawn(async move {
                let messages = match messages {
                    Some(messages) => messages,
                    None => std::sync::Arc::new(
                        threadlane_ui_state::projection::compute_session_messages(&source)?,
                    ),
                };
                let prompt = threadlane_ui_state::chat::conversation_title_prompt(&messages);
                if prompt.trim().is_empty() {
                    return Err("The session has no conversation to generate a title from.".to_owned());
                }
                Ok(prompt)
            }).await;
            let result = match prompt {
                Ok(prompt) => {
                    let work_dir = session.runtime_work_dir.clone();
                    runtime.spawn(async move {
                        threadlane_ui_state::chat::generate_text(
                            model,
                            work_dir,
                            "Return only a concise session title, maximum 42 Unicode characters. No Markdown, explanations or tools.".into(),
                            prompt,
                        ).await
                    }).await.unwrap_or_else(|error| Err(error.to_string()))
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.title_generating.remove(&session.session_file);
                let unchanged = this.model.read(cx).projects.iter()
                    .flat_map(|project| &project.sessions)
                    .any(|current| current.session_file == session.session_file
                        && current.id == session.id && current.title == session.title);
                let result = if unchanged {
                    result.and_then(|raw| threadlane_ui_state::chat::persist_regenerated_title(
                        &session.session_file, &raw,
                    ))
                } else {
                    Err("The session was removed or its title changed; generated text was not applied.".into())
                };
                match result {
                    Ok(title) => this.model.update(cx, |state, cx| {
                        for current in state.projects.iter_mut().flat_map(|project| &mut project.sessions) {
                            if current.session_file == session.session_file {
                                current.title = title.clone();
                            }
                        }
                        cx.notify();
                    }),
                    Err(error) => window.push_notification(
                        format!("Couldn’t regenerate title for “{}”: {error}", session.title), cx,
                    ),
                }
                cx.notify();
            });
        }).detach();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.model.read(cx);
        threadlane_ui_kit::sidebar_header(
            state.active_session_is_loading(),
            state.is_generating,
            state.active_session_attention().unwrap_or(SessionAttention::Idle),
            self.render_github_nav(cx).into_any_element(),
            threadlane_ui_theme::APP_CAPTION_STRIP,
            |window, cx| window.dispatch_action(Box::new(crate::BeginNewTask), cx),
            cx,
        )
    }

    fn render_project_filter(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

        threadlane_ui_kit::sidebar_project_filter(
            &selected_label,
            selected_filter.is_some(),
            move |menu, window, cx| {
                let model = filter_model.clone();
                threadlane_ui_kit::sidebar_project_menu(menu, &projects, selected_filter.as_deref(), move |selected, _, cx| {
                    model.update(cx, |state, cx| { controller::dispatch(state, AppAction::SetSidebarProjectFilter(selected)); cx.notify(); });
                }, window, cx)
            },
            threadlane_ui_kit::sidebar_attach_project_button().on_click(
                move |_event, _window, cx| {
                    let model = attach_model.clone();
                    cx.spawn(async move |cx| {
                        let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                            return;
                        };
                        let path = folder.path().to_path_buf();
                        let _ = model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::AttachProject(path));
                            cx.notify();
                        });
                    })
                    .detach();
                },
            ),
            cx,
        )
    }

    fn has_history_filters(&self, state: &AppState) -> bool {
        state.sidebar_project_filter.is_some()
    }

    fn clear_history_filters(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |state, cx| {
            controller::dispatch(state, AppAction::SetSidebarProjectFilter(None));
            cx.notify();
        });
        cx.notify();
    }

    fn render_session_card(
        &self,
        session: &SessionInfo,
        attention: SessionAttention,
        is_active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = self.model.read(cx);
        let project = state
            .projects
            .iter()
            .find(|project| {
                project
                    .sessions
                    .iter()
                    .any(|candidate| candidate.session_file == session.session_file)
            })
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "Project".into());
        let card_state = threadlane_ui_kit::SidebarSessionCardState {
            project,
            show_project: state.projects.len() > 1,
            attention,
            selected: is_active,
            pinned: state.is_session_pinned(&session.work_dir, &session.id),
            unseen_result: state.session_has_unseen_result(session),
            snooze: state
                .session_snooze(&session.work_dir, &session.id)
                .map(sidebar_snooze_status),
            git_status: session_card_git_status(&state.git_statuses, session).cloned(),
            pr: session_pr_info(session, &state.git_prs).cloned(),
            now: now_unix_secs(),
        };
        let model = self.model.clone();
        let view = cx.entity().downgrade();
        let target = session.clone();
        let quick_model = self.model.clone();
        let quick_view = view.clone();
        let quick_target = session.clone();
        let full_model = self.model.clone();
        let full_view = view.clone();
        let full_target = session.clone();
        threadlane_ui_kit::sidebar_session_card(
            session,
            card_state,
            move |action, window, cx| {
                // Preserve the quick-archive contract: ordinary checkouts archive
                // immediately; associated worktrees require the existing confirmation.
                if action == threadlane_ui_kit::SidebarSessionAction::Archive && !target.is_worktree
                {
                    model.update(cx, |state, cx| {
                        controller::dispatch(
                            state,
                            AppAction::SettleSession {
                                work_dir: target.work_dir.clone(),
                                session_id: target.id.clone(),
                                delete_worktree: false,
                            },
                        );
                        cx.notify();
                    });
                } else {
                    execute_sidebar_session_action(action, &model, &view, &target, window, cx);
                }
            },
            move |menu, window, cx| {
                render_sidebar_session_menu(
                    menu,
                    quick_model.clone(),
                    quick_view.clone(),
                    quick_target.clone(),
                    threadlane_ui_kit::SidebarSessionMenuScope::Quick,
                    window,
                    cx,
                )
            },
            move |menu, window, cx| {
                render_sidebar_session_menu(
                    menu,
                    full_model.clone(),
                    full_view.clone(),
                    full_target.clone(),
                    threadlane_ui_kit::SidebarSessionMenuScope::Full,
                    window,
                    cx,
                )
            },
            cx,
        )
    }

    fn render_update_control(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let status = &self.model.read(cx).update_status;
        let label = update_control_label(status)?;
        let busy = matches!(
            status,
            UpdateStatus::Checking | UpdateStatus::Downloading { .. } | UpdateStatus::Installing
        );
        let icon = if matches!(
            status,
            UpdateStatus::Available(_) | UpdateStatus::Downloading { .. }
        ) {
            "icons/download.svg"
        } else {
            "icons/refresh-cw.svg"
        };
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(
                    Button::new("sidebar-update")
                        .debug_selector(|| "sidebar-update".into())
                        .icon(Icon::default().path(icon))
                        .small()
                        .ghost()
                        .accessibility_label(label.clone())
                        .tooltip(label.clone())
                        .loading(busy)
                        .disabled(busy)
                        .when(matches!(status, UpdateStatus::Error(_)), |button| {
                            button.text_color(cx.theme().danger)
                        })
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(crate::ActivateUpdate), cx)
                        }),
                )
                .when(busy, |control| {
                    control.child(
                        Progress::new("sidebar-update-progress")
                            .accessibility_label(label)
                            .xsmall()
                            .w_8()
                            .loading(!matches!(status, UpdateStatus::Downloading { .. }))
                            .value(match status {
                                UpdateStatus::Downloading { progress, .. } => {
                                    progress.clamp(0.0, 1.0) * 100.0
                                }
                                _ => 0.0,
                            }),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings_model = self.model.clone();
        let pairing_model = self.model.clone();
        let settings_selected =
            self.model.read(cx).workspace_page == threadlane_ui_state::WorkspacePage::Settings;

        threadlane_ui_kit::sidebar_footer_surface(cx)
            .child(
                threadlane_ui_kit::sidebar_settings_button(settings_selected, cx)
                    .on_click(move |_event, _window, cx| {
                        settings_model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::OpenSettings);
                            cx.notify();
                        });
                    }),
            )
            .child(
                threadlane_ui_kit::sidebar_pairing_button(cx)
                    .on_click(move |_event, window, cx| {
                        threadlane_ui_pairing::open_pairing_dialog(
                            pairing_model.clone(),
                            window,
                            cx,
                        );
                    }),
            )
            .children(self.render_update_control(cx))
    }

    fn render_github_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use threadlane_ui_kit::SidebarDestination;
        let state = self.model.read(cx);
        let selected = match state.workspace_page {
            WorkspacePage::Automations => Some(SidebarDestination::Automations),
            WorkspacePage::GitHub => Some(match state.github_tab {
                GitHubTab::Issues => SidebarDestination::Issues,
                GitHubTab::PullRequests => SidebarDestination::PullRequests,
            }),
            _ => None,
        };
        let attention = state.automations.snapshot.runs.iter().filter(|run| run.needs_attention()).count();
        let open_prs = state.git_prs.values().filter_map(|pr| pr.as_ref())
            .filter(|pr| pr.state.eq_ignore_ascii_case("OPEN")).count();
        let model = self.model.clone();
        threadlane_ui_kit::sidebar_navigation(selected, attention, open_prs, move |destination, _, cx| {
            model.update(cx, |state, cx| {
                controller::dispatch(state, match destination {
                    SidebarDestination::Automations => AppAction::OpenAutomations,
                    SidebarDestination::Issues => AppAction::OpenGitHubTab(GitHubTab::Issues),
                    SidebarDestination::PullRequests => AppAction::OpenGitHubTab(GitHubTab::PullRequests),
                });
                cx.notify();
            });
        }, cx)
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
                let is_pinned = state.is_session_pinned(&session.work_dir, &session.id);
                let has_unseen_result = state.session_has_unseen_result(session);
                let snooze = state.session_snooze(&session.work_dir, &session.id);
                sessions.push((
                    session.clone(),
                    attention,
                    is_pinned,
                    has_unseen_result,
                    snooze,
                ));
            }
        }
        // Search reveals matching snoozed rows regardless of the section's
        // collapsed state; only the plain list honors it.
        flatten_history_sessions_with_pins(
            sessions,
            now,
            self.snoozed_collapsed && query.is_empty(),
        )
    }

    fn render_history_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self
            .history_cache
            .as_ref()
            .and_then(|(_, rows)| rows.get(index))
            .cloned()
        {
            Some(HistoryRow::Group(group)) => {
                threadlane_ui_kit::session_group_header(group, index == 0, window, cx).into_any_element()
            }
            Some(HistoryRow::SnoozedHeader(count)) => {
                let owner = cx.entity().downgrade();
                threadlane_ui_kit::sidebar_snoozed_header(count, self.snoozed_collapsed, index == 0, move |_, cx| {
                    let _ = owner.update(cx, |this, cx| { this.snoozed_collapsed = !this.snoozed_collapsed; cx.notify(); });
                }, window, cx).into_any_element()
            }
            Some(HistoryRow::Session(session, attention, _has_unseen_result, _snooze)) => {
                let state = self.model.read(cx);
                let is_active = state.workspace_page == WorkspacePage::Chat
                    && state.active_work_dir.as_ref() == Some(&session.work_dir)
                    && state.active_session_id.as_deref() == Some(session.id.as_str());
                threadlane_ui_kit::session_history_card_row(self.render_session_card(&session, attention, is_active, cx))
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    /// One view-owned wake task for the nearest snooze deadline — no
    /// per-row timers. The sleep is capped at 60s so a forward clock jump
    /// or a resume lands within a minute instead of waiting out a stale
    /// relative delay; each wake re-checks wall time and only reconciles
    /// when the deadline truly passed.
    fn arm_snooze_deadline(&mut self, cx: &mut Context<Self>) {
        let next = self.model.read(cx).next_snooze_deadline();
        if next == self.snooze_deadline_armed {
            return;
        }
        self.snooze_deadline_armed = next;
        self.snooze_timer_epoch += 1;
        let Some(deadline) = next else {
            return;
        };
        let epoch = self.snooze_timer_epoch;
        cx.spawn(async move |this, cx| {
            loop {
                let remaining = deadline.saturating_sub(now_unix_secs());
                if remaining == 0 {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(remaining.min(60)))
                    .await;
                let still_armed = this
                    .update(cx, |this, _| {
                        this.snooze_timer_epoch == epoch
                            && this.snooze_deadline_armed == Some(deadline)
                    })
                    .unwrap_or(false);
                if !still_armed {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.model.update(cx, |state, _cx| {
                    state.reconcile_session_snoozes();
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn render_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.model.read(cx);
        let query = state.search_query.trim().to_lowercase();
        let has_filters = self.has_history_filters(state);
        let now = now_unix_secs();

        let fingerprint = sidebar_fingerprint(state, now);
        self.history_fingerprint = fingerprint;
        // The collapse flag is view-owned (not part of the model
        // fingerprint), so it folds into the row-cache key here.
        let cache_key = fingerprint ^ (self.snoozed_collapsed as u64);
        let cache_matches = self
            .history_cache
            .as_ref()
            .is_some_and(|(cached, _)| *cached == cache_key);
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
            } else if let Some((_, cached)) = self.history_cache.as_ref() {
                // The ListState caches measured heights per index; a row that
                // gains or loses a signals row (attention, New result) keeps
                // a stale height unless its range is remeasured.
                let mut remeasure: Option<(usize, usize)> = None;
                for (index, (left, right)) in cached.iter().zip(&rows).enumerate() {
                    if history_row_height_inputs(left, right) {
                        match &mut remeasure {
                            Some((_, last)) => *last = index + 1,
                            slot => *slot = Some((index, index + 1)),
                        }
                    }
                }
                if let Some((start, end)) = remeasure {
                    self.history_list_state.remeasure_items(start..end);
                }
            }
            self.history_cache = Some((cache_key, rows));
        }
        self.arm_snooze_deadline(cx);

        let row_count = self
            .history_cache
            .as_ref()
            .map_or(0, |(_, rows)| rows.len());
        if row_count == 0 {
            let owner = cx.entity().downgrade();
            return threadlane_ui_kit::sidebar_history_empty(has_filters, move |action, window, cx| match action {
                threadlane_ui_kit::SidebarEmptyAction::ClearFilters => { let _ = owner.update(cx, |this, cx| this.clear_history_filters(cx)); }
                threadlane_ui_kit::SidebarEmptyAction::NewTask => window.dispatch_action(Box::new(crate::BeginNewTask), cx),
            }, cx).into_any_element();
        }

        threadlane_ui_kit::session_list(self.history_list_state.clone(), cx.processor(Self::render_history_row))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        flatten_history_sessions, flatten_history_sessions_with_pins, format_time_ago,
        history_query_matches, pr_status_label, pr_status_tooltip, same_history_row_identity,
        session_pr_info, sidebar_session_fingerprint, sidebar_session_identity, DateGroup,
        HistoryRow,
    };
    use std::collections::HashMap;
    use threadlane_git::GitHubPrInfo;
    use threadlane_ui_state::{
        SessionAttention, SessionCompletionSummary, SessionHealth, SessionInfo, SessionSnooze,
    };

    #[gpui::test]
    fn update_control_stays_beside_settings_and_tracks_progress(cx: &mut gpui::TestAppContext) {
        use gpui::*;
        use threadlane_ui_state::AppState;
        use threadlane_updater::UpdateStatus;

        struct Footer(
            Entity<super::SidebarView>,
            std::rc::Rc<std::cell::Cell<usize>>,
            FocusHandle,
        );
        impl Render for Footer {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let activations = self.1.clone();
                div()
                    .track_focus(&self.2)
                    .on_action(move |_: &crate::ActivateUpdate, _, _| {
                        activations.set(activations.get() + 1);
                    })
                    .child(self.0.update(cx, |sidebar, cx| {
                        div()
                            .tab_group()
                            .w(rems(13.9375))
                            .child(sidebar.render_footer(cx))
                    }))
            }
        }

        cx.update(gpui_component::init);
        let model = cx.new(|_| AppState::default());
        let activations = std::rc::Rc::new(std::cell::Cell::new(0));
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let sidebar = cx.new(|cx| super::SidebarView::new(model.clone(), window, cx));
            let footer = cx.new(|cx| {
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                Footer(sidebar, activations.clone(), focus)
            });
            gpui_component::Root::new(footer, window, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("sidebar-update").is_none());

        for status in [
            UpdateStatus::Downloading {
                version: "9.9.9".into(),
                progress: 0.25,
            },
            UpdateStatus::Downloading {
                version: "9.9.9".into(),
                progress: 0.75,
            },
            UpdateStatus::Checking,
            UpdateStatus::Installing,
            UpdateStatus::Error("offline".into()),
        ] {
            model.update(cx, |state, cx| {
                state.update_status = status;
                cx.notify();
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let settings = cx.debug_bounds("sidebar-settings").unwrap();
            let update = cx
                .debug_bounds("sidebar-update")
                .expect("update belongs in sidebar footer");
            assert!(update.left() >= settings.right());
            assert!(update.right() <= px(223.0));
            cx.simulate_click(update.center(), Modifiers::default());
        }
        assert_eq!(activations.get(), 1, "busy controls must not activate");
        cx.update(|window, cx| {
            window.blur(cx);
            window.focus_next(cx); // Settings
            window.focus_next(cx); // Share with mobile
            window.focus_next(cx); // Retry update
            window.draw(cx).clear(cx);
        });
        let keystroke = Keystroke::parse("enter").unwrap();
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
        assert_eq!(
            activations.get(),
            2,
            "update action must support keyboard activation"
        );
        model.update(cx, |state, cx| {
            state.update_status = UpdateStatus::UpToDate;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("sidebar-update").is_none());
    }

    #[test]
    fn update_progress_label_preserves_version_and_bounds_percentage() {
        use threadlane_updater::UpdateStatus;
        for (progress, percentage) in [(-0.5, 0), (0.25, 25), (0.75, 75), (1.5, 100)] {
            assert_eq!(
                super::update_control_label(&UpdateStatus::Downloading {
                    version: "9.9.9".into(),
                    progress,
                })
                .unwrap(),
                format!("Downloading Threadlane 9.9.9: {percentage}%")
            );
        }
        assert!(super::update_control_label(&UpdateStatus::Idle).is_none());
        assert!(super::update_control_label(&UpdateStatus::UpToDate).is_none());
        let label = super::update_control_label(&UpdateStatus::Error("界".repeat(1000))).unwrap();
        assert!(label.chars().count() < 220);
        assert!(label.ends_with("…. Retry update check"));
    }

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
            completion_summary: SessionCompletionSummary::Unknown,
        }
    }

    #[test]
    fn sidebar_git_badge_only_uses_the_available_session_checkout() {
        let mut statuses = std::collections::HashMap::new();
        let mut task = session("git-badge");
        statuses.insert(task.work_dir.clone(), threadlane_git::GitStatus::default());
        assert!(super::session_card_git_status(&statuses, &task).is_some());
        task.is_worktree = true;
        task.runtime_work_dir = "/project/worktree".into();
        assert!(super::session_card_git_status(&statuses, &task).is_none());
        statuses.insert(task.runtime_work_dir.clone(), threadlane_git::GitStatus::default());
        assert!(super::session_card_git_status(&statuses, &task).is_some());
        task.worktree_available = false;
        assert!(super::session_card_git_status(&statuses, &task).is_none());
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
        cx.update(|window, cx| window.focus_next(cx)); // Session actions menu.
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
            matches!(&rows[1], HistoryRow::Session(item, SessionAttention::NeedsYou, ..) if item.id == "needs-newer")
        );
        assert!(
            matches!(&rows[2], HistoryRow::Session(item, SessionAttention::NeedsYou, ..) if item.id == "needs-older")
        );
        assert!(matches!(rows[3], HistoryRow::Group(DateGroup::Working)));
        assert!(
            matches!(&rows[4], HistoryRow::Session(item, SessionAttention::Working, ..) if item.id == "working")
        );
        assert!(matches!(rows[5], HistoryRow::Group(DateGroup::Today)));
        assert!(
            matches!(&rows[6], HistoryRow::Session(item, SessionAttention::Ready, ..) if item.id == "ready-today")
        );
        assert!(matches!(rows[7], HistoryRow::Group(DateGroup::Yesterday)));
        assert!(
            matches!(&rows[8], HistoryRow::Session(item, SessionAttention::Idle, ..) if item.id == "idle-yesterday")
        );
        assert!(same_history_row_identity(
            &rows[1],
            &HistoryRow::Session(
                session("needs-newer"),
                SessionAttention::Idle,
                false,
                None,
            )
        ));
        assert!(!same_history_row_identity(&rows[1], &rows[4]));
    }

    #[test]
    fn history_rows_prioritize_pinned_sessions_first() {
        let now = 1_000_000;
        let mut pinned_idle = session("pinned-idle");
        pinned_idle.updated_at = now - 90_000;
        let mut needs_newer = session("needs-newer");
        needs_newer.updated_at = now - 100;
        let mut working = session("working");
        working.updated_at = now - 200;

        let rows = flatten_history_sessions_with_pins(
            vec![
                (working, SessionAttention::Working, false, false, None),
                (needs_newer, SessionAttention::NeedsYou, false, false, None),
                (pinned_idle, SessionAttention::Idle, true, false, None),
            ],
            now,
            false,
        );

        assert!(matches!(rows[0], HistoryRow::Group(DateGroup::Pinned)));
        assert!(
            matches!(&rows[1], HistoryRow::Session(item, SessionAttention::Idle, ..) if item.id == "pinned-idle")
        );
        assert!(matches!(rows[2], HistoryRow::Group(DateGroup::NeedsYou)));
        assert!(
            matches!(&rows[3], HistoryRow::Session(item, SessionAttention::NeedsYou, ..) if item.id == "needs-newer")
        );
        assert!(matches!(rows[4], HistoryRow::Group(DateGroup::Working)));
        assert!(
            matches!(&rows[5], HistoryRow::Session(item, SessionAttention::Working, ..) if item.id == "working")
        );
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
        let first = sidebar_session_fingerprint(&item, SessionAttention::Idle, false);

        item.git_branch = Some("feature/two".into());

        assert_ne!(
            first,
            sidebar_session_fingerprint(&item, SessionAttention::Idle, false)
        );
    }

    #[test]
    fn attention_changes_the_sidebar_fingerprint() {
        let item = session("session");

        assert_ne!(
            sidebar_session_fingerprint(&item, SessionAttention::Idle, false),
            sidebar_session_fingerprint(&item, SessionAttention::NeedsYou, false)
        );
    }

    #[test]
    fn unseen_result_changes_the_sidebar_fingerprint() {
        let item = session("session");

        assert_ne!(
            sidebar_session_fingerprint(&item, SessionAttention::Idle, false),
            sidebar_session_fingerprint(&item, SessionAttention::Idle, true)
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

        let before = sidebar_session_fingerprint(&item, SessionAttention::Idle, false);
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
            sidebar_session_fingerprint(&item, SessionAttention::Idle, false)
        );
    }

    fn snooze(wake_at: u64, pending: bool) -> Option<SessionSnooze> {
        Some(SessionSnooze {
            wake_at,
            pending,
            save_failed: false,
        })
    }

    #[test]
    fn snoozed_sessions_group_at_the_bottom_by_return_time() {
        let now = 700_000;
        let mut far = session("far");
        far.updated_at = now - 10;
        let mut near = session("near");
        near.updated_at = now - 20;
        let mut pinned = session("pinned");
        pinned.updated_at = now - 5;

        let rows = flatten_history_sessions_with_pins(
            vec![
                (far, SessionAttention::Idle, false, false, snooze(now + 7200, false)),
                (pinned, SessionAttention::Idle, true, false, None),
                (near, SessionAttention::Idle, false, false, snooze(now + 100, false)),
            ],
            now,
            false,
        );

        assert!(matches!(rows[0], HistoryRow::Group(DateGroup::Pinned)));
        assert!(matches!(
            &rows[1],
            HistoryRow::Session(item, ..) if item.id == "pinned"
        ));
        assert!(matches!(rows[2], HistoryRow::SnoozedHeader(2)));
        assert!(matches!(
            &rows[3],
            HistoryRow::Session(item, ..) if item.id == "near"
        ));
        assert!(matches!(
            &rows[4],
            HistoryRow::Session(item, ..) if item.id == "far"
        ));
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn a_pending_snooze_stays_in_its_normal_group() {
        let now = 700_000;
        let mut pending = session("pending");
        pending.updated_at = now - 10;
        let mut confirmed = session("confirmed");
        confirmed.updated_at = now - 30;

        let rows = flatten_history_sessions_with_pins(
            vec![
                (pending, SessionAttention::Idle, false, false, snooze(now + 100, true)),
                (confirmed, SessionAttention::Idle, false, false, snooze(now + 200, false)),
            ],
            now,
            false,
        );

        // The unconfirmed save keeps its date group; only the confirmed
        // record lands under Snoozed.
        assert!(matches!(rows[0], HistoryRow::Group(DateGroup::Today)));
        assert!(matches!(
            &rows[1],
            HistoryRow::Session(item, ..) if item.id == "pending"
        ));
        assert!(matches!(rows[2], HistoryRow::SnoozedHeader(1)));
        assert!(matches!(
            &rows[3],
            HistoryRow::Session(item, ..) if item.id == "confirmed"
        ));
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn collapsing_the_snoozed_section_keeps_only_its_header() {
        let now = 700_000;
        let mut item = session("snoozed");
        item.updated_at = now - 10;

        let rows = flatten_history_sessions_with_pins(
            vec![(item, SessionAttention::Idle, false, false, snooze(now + 100, false))],
            now,
            true,
        );

        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], HistoryRow::SnoozedHeader(1)));
    }

    #[gpui::test]
    fn snoozed_rows_move_groups_and_stay_searchable_when_collapsed(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::*;
        use threadlane_ui_state::{AppState, ProjectInfo, RunCompletionToken};

        cx.update(gpui_component::init);
        let temporary = tempfile::tempdir().unwrap();
        let project = temporary.path().join("snooze-project");
        let session_file = project.join(".threadlane/sessions/sess.jsonl");
        std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
        let mut item = session("snoozed-task");
        item.work_dir = project.clone();
        item.runtime_work_dir = project.clone();
        item.session_file = session_file;
        item.completion_summary =
            SessionCompletionSummary::Latest(RunCompletionToken {
                record_id: "r".into(),
                run_id: "r".into(),
                seq: 1,
            });

        let (root, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| {
                let mut state = AppState::default();
                state.active_work_dir = None;
                state.active_session_id = None;
                state.pending_hydrations.clear();
                state.projects.push(ProjectInfo {
                    name: "snooze-project".into(),
                    work_dir: project.clone(),
                    sessions: vec![item.clone()],
                    is_expanded: true,
                });
                state.sidebar_project_filter = Some(project.clone());
                state
            });
            gpui_component::Root::new(
                cx.new(|cx| super::SidebarView::new(model, window, cx)),
                window,
                cx,
            )
        });
        let sidebar = root.read_with(cx, |root, _| {
            root.view()
                .clone()
                .downcast::<super::SidebarView>()
                .unwrap()
        });
        let model = sidebar.read_with(cx, |view, _| view.model.clone());

        let rows_at = |cx: &mut gpui::VisualTestContext| {
            sidebar.update(cx, |view, cx| {
                view.build_history_rows(model.read(cx), "", 0)
            })
        };

        // Baseline: the settled row sits in its normal date group.
        assert!(rows_at(cx)
            .iter()
            .all(|row| !matches!(row, HistoryRow::SnoozedHeader(_))));
        assert!(rows_at(cx).iter().any(|row| matches!(
            row,
            HistoryRow::Session(item, ..) if item.id == "snoozed-task"
        )));

        model.update(cx, |state, _| {
            state
                .snooze_session(&project, "snoozed-task", 3_600)
                .unwrap();
        });
        // The write runs on the serialized background writer; drain until
        // the confirmation lands so the row can move.
        let confirmed = (0..200).any(|_| {
            model.update(cx, |state, _| {
                state.drain_chat_stream(Vec::new());
            });
            std::thread::sleep(std::time::Duration::from_millis(10));
            model.read_with(cx, |state, _| {
                state
                    .session_snooze(&project, "snoozed-task")
                    .is_some_and(|snooze| !snooze.pending)
            })
        });
        assert!(confirmed, "snooze write was never confirmed");
        model.update(cx, |_, cx| cx.notify());

        let rows = rows_at(cx);
        let header = rows
            .iter()
            .position(|row| matches!(row, HistoryRow::SnoozedHeader(1)))
            .expect("a snoozed section header is expected");
        assert!(matches!(
            &rows[header + 1],
            HistoryRow::Session(item, SessionAttention::Idle | SessionAttention::Ready, _, Some(snooze))
                if item.id == "snoozed-task" && !snooze.pending
        ));
        // Rows still appear in the snoozed section even while collapsed only
        // the header remains.
        sidebar.update(cx, |view, _| view.snoozed_collapsed = true);
        let collapsed = rows_at(cx);
        assert!(matches!(collapsed[header], HistoryRow::SnoozedHeader(1)));
        assert_eq!(collapsed.len(), header + 1);

        // Search reveals the matching row regardless of the collapse.
        let searched = sidebar.update(cx, |view, cx| {
            view.build_history_rows(model.read(cx), "snoozed-task", 0)
        });
        assert!(searched.iter().any(|row| matches!(
            row,
            HistoryRow::Session(item, ..) if item.id == "snoozed-task"
        )));

        // Unsnooze returns the row to its normal group immediately.
        model.update(cx, |state, _| {
            state.unsnooze_session(&project, "snoozed-task")
        });
        sidebar.update(cx, |view, _| view.snoozed_collapsed = false);
        let rows = rows_at(cx);
        assert!(rows
            .iter()
            .all(|row| !matches!(row, HistoryRow::SnoozedHeader(_))));
        assert!(rows.iter().any(|row| matches!(
            row,
            HistoryRow::Session(item, ..) if item.id == "snoozed-task"
        )));
    }

    #[gpui::test]
    fn project_filter_menu_selects_and_clears_without_switching_chat(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::*;
        use threadlane_ui_state::{AppState, ProjectInfo};

        struct Filter(Entity<super::SidebarView>);
        impl Render for Filter {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().w(rems(16.5)).child(self.0.update(cx, |sidebar, cx| {
                    sidebar.render_project_filter(cx).into_any_element()
                }))
            }
        }
        cx.update(gpui_component::init);
        let temporary = tempfile::tempdir().unwrap();
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        let model = cx.new(|_| {
            let mut state = AppState::default();
            state.pending_hydrations.clear();
            state.projects = vec![
                ProjectInfo {
                    name: "First".into(),
                    work_dir: first.clone(),
                    sessions: Vec::new(),
                    is_expanded: true,
                },
                ProjectInfo {
                    name: "Second".into(),
                    work_dir: second.clone(),
                    sessions: Vec::new(),
                    is_expanded: true,
                },
            ];
            state.active_work_dir = Some(first.clone());
            state.active_session_id = Some("active-chat".into());
            state.sidebar_project_filter = None;
            state
        });
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let sidebar = cx.new(|cx| super::SidebarView::new(model.clone(), window, cx));
            gpui_component::Root::new(cx.new(|_| Filter(sidebar)), window, cx)
        });
        for (steps, expected) in [(3, Some(second)), (1, None)] {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let trigger = cx.debug_bounds("sidebar-project-filter").unwrap();
            cx.simulate_click(trigger.center(), Modifiers::default());
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            for _ in 0..steps {
                cx.simulate_keystrokes("down");
            }
            cx.simulate_keystrokes("enter");
            model.read_with(cx, |state, _| {
                assert_eq!(state.sidebar_project_filter, expected);
                assert_eq!(state.active_work_dir.as_ref(), Some(&first));
                assert_eq!(state.active_session_id.as_deref(), Some("active-chat"));
            });
        }
        // Escape dismisses without changing the last confirmed filter.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let trigger = cx.debug_bounds("sidebar-project-filter").unwrap();
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("down down down escape");
        model.read_with(cx, |state, _| {
            assert!(state.sidebar_project_filter.is_none())
        });
    }
    #[gpui::test]
    fn session_actions_menu_opens_and_escape_dismisses(cx: &mut gpui::TestAppContext) {
        use gpui::*;
        use threadlane_ui_state::{AppState, RunCompletionToken};

        struct Card {
            sidebar: Entity<super::SidebarView>,
            session: SessionInfo,
        }
        impl Render for Card {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar
                        .render_session_card(&self.session, SessionAttention::Ready, false, cx)
                        .into_any_element()
                })
            }
        }

        cx.update(gpui_component::init);
        let temporary = tempfile::tempdir().unwrap();
        let project = temporary.path().join("menu-project");
        let session_file = project.join(".threadlane/sessions/sess.jsonl");
        std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
        let mut item = session("menu-task");
        item.work_dir = project.clone();
        item.runtime_work_dir = project.clone();
        item.session_file = session_file;
        item.completion_summary =
            SessionCompletionSummary::Latest(RunCompletionToken {
                record_id: "r".into(),
                run_id: "r".into(),
                seq: 1,
            });
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| {
                let mut state = AppState::default();
                state.active_work_dir = None;
                state.active_session_id = None;
                state.pending_hydrations.clear();
                state
            });
            let sidebar = cx.new(|cx| super::SidebarView::new(model, window, cx));
            gpui_component::Root::new(
                cx.new(|_| Card {
                    sidebar,
                    session: item.clone(),
                }),
                window,
                cx,
            )
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let focus_before = cx.update(|window, cx| window.focused(cx));
        let trigger = cx
            .debug_bounds("session-actions-menu-task")
            .expect("the visible session-actions trigger exists");
        cx.simulate_click(trigger.center(), Modifiers::default());
        // The popover mounts its menu content on the next frame after any
        // deferred open-state work settles.
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let menu_focus = cx.update(|window, cx| window.focused(cx));
        assert!(
            menu_focus.is_some() && menu_focus != focus_before,
            "the actions menu opens from the row trigger and takes focus"
        );

        // Arrow/Enter operate the menu: nothing is preselected, so Down
        // lands on "Open Session" and a second Down on "Pin Session" —
        // Enter activates it, an observable state change that only the
        // menu can produce.
        cx.simulate_keystrokes("down");
        cx.simulate_keystrokes("down");
        cx.simulate_keystrokes("enter");
        let pinned = _root.read_with(cx, |root, cx| {
            root.view()
                .clone()
                .downcast::<Card>()
                .unwrap()
                .read(cx)
                .sidebar
                .read(cx)
                .model
                .read(cx)
                .is_session_pinned(&project, "menu-task")
        });
        assert!(pinned, "activating Pin Session must pin the session");

        // Activating the item dismisses the menu, but a dismissed menu is
        // not observable through focus (the kit only restores focus that is
        // contained in the popover's trigger focus). Probe it functionally:
        // if the menu were still open, Down/Down/Enter would land on the
        // relabeled "Unpin Session" and clear the pin. On a closed popover
        // the same keystrokes do nothing more than re-open it.
        let still_pinned = |cx: &mut gpui::VisualTestContext| {
            cx.simulate_keystrokes("down");
            cx.simulate_keystrokes("down");
            cx.simulate_keystrokes("enter");
            _root.read_with(cx, |root, cx| {
                root.view()
                    .clone()
                    .downcast::<Card>()
                    .unwrap()
                    .read(cx)
                    .sidebar
                    .read(cx)
                    .model
                    .read(cx)
                    .is_session_pinned(&project, "menu-task")
            })
        };
        assert!(
            still_pinned(cx),
            "activating an item must dismiss the menu"
        );

        // The probe re-opened the popover; Escape must dismiss the topmost
        // menu (verified the same way).
        cx.simulate_keystrokes("escape");
        assert!(still_pinned(cx), "escape must dismiss the topmost menu");
    }
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        threadlane_ui_kit::sidebar_surface(cx)
            .child(self.render_header(cx))
            .child(self.render_project_filter(cx))
            .child(div().flex_1().min_h_0().child(self.render_history(cx)))
            .child(self.render_footer(cx))
    }
}
