use gpui::prelude::FluentBuilder;
use gpui::InteractiveElement;
use gpui::*;

use gpui_component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::theme::ActiveTheme;
use gpui_component::{Icon, IconName, Selectable, Sizable, WindowExt};

use crate::app::{actions::AppAction, controller};
use crate::state::{AppState, SessionHealth, SessionInfo, TrajectoryEntry};

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
    runtime: Option<&crate::services::sessions::SessionRuntime>,
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
    Today,
    Yesterday,
    ThisWeek,
    Older,
}

#[derive(Clone)]
enum HistoryRow {
    Group(DateGroup),
    Session(SessionInfo),
}

fn same_history_row_identity(left: &HistoryRow, right: &HistoryRow) -> bool {
    match (left, right) {
        (HistoryRow::Group(left), HistoryRow::Group(right)) => left == right,
        (HistoryRow::Session(left), HistoryRow::Session(right)) => {
            left.id == right.id && left.work_dir == right.work_dir
        }
        _ => false,
    }
}

fn flatten_history_groups(grouped: Vec<(DateGroup, Vec<SessionInfo>)>) -> Vec<HistoryRow> {
    grouped
        .into_iter()
        .filter(|(_, sessions)| !sessions.is_empty())
        .flat_map(|(group, sessions)| {
            std::iter::once(HistoryRow::Group(group))
                .chain(sessions.into_iter().map(HistoryRow::Session))
        })
        .collect()
}

impl DateGroup {
    fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This Week",
            Self::Older => "Older",
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
    history_list_state: ListState,
    _subscriptions: Vec<Subscription>,
}

fn sidebar_session_fingerprint(session: &SessionInfo) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    session.id.hash(&mut hasher);
    session.title.hash(&mut hasher);
    session.work_dir.hash(&mut hasher);
    session.session_file.hash(&mut hasher);
    session.updated_at.hash(&mut hasher);
    session.health.hash(&mut hasher);
    session.git_branch.hash(&mut hasher);
    session.is_worktree.hash(&mut hasher);
    session.worktree_available.hash(&mut hasher);
    hasher.finish()
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
    state.sidebar_project_filter.hash(&mut hasher);
    state.search_query.trim().to_lowercase().hash(&mut hasher);
    (now / 60).hash(&mut hasher);
    for project in &state.projects {
        project.name.hash(&mut hasher);
        project.work_dir.hash(&mut hasher);
        for session in &project.sessions {
            sidebar_session_fingerprint(session).hash(&mut hasher);
            state
                .session_is_generating(&session.session_file)
                .hash(&mut hasher);
        }
    }
    let mut git_work_dirs: Vec<&std::path::PathBuf> = state.git_statuses.keys().collect();
    git_work_dirs.sort();
    for work_dir in git_work_dirs {
        work_dir.hash(&mut hasher);
        if let Some(pr) = state.git_statuses[work_dir].pr.as_ref() {
            pr.number.hash(&mut hasher);
            pr.state.hash(&mut hasher);
            pr.is_draft.hash(&mut hasher);
            pr.total_checks.hash(&mut hasher);
            pr.failing_checks.hash(&mut hasher);
            pr.pending_checks.hash(&mut hasher);
            pr.passing_checks.hash(&mut hasher);
        }
    }
    let mut git_prs: Vec<_> = state.git_prs.iter().collect();
    git_prs.sort_by(|left, right| left.0.cmp(right.0));
    for ((work_dir, branch), pr) in git_prs {
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
    pub(crate) fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search tasks…"));

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
            history_cache: None,
            history_list_state: ListState::new(0, ListAlignment::Top, px(72.0)),
            _subscriptions: vec![sub1, sub2],
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let theme = cx.theme().colors;

        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .px_3()
            .pt(px(48.0))
            .pb_1()
            .bg(theme.title_bar)
            .child(
                Button::new("new-task-btn")
                    .icon(IconName::Plus)
                    .label("New Task")
                    .ghost()
                    .w_full()
                    .on_click(move |_event, _window, cx| {
                        model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::BeginNewTask);
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(36.0))
                    .rounded_md()
                    .text_color(theme.muted_foreground)
                    .child(IconName::Search)
                    .child(
                        div().flex_1().child(
                            Input::new(&self.search_input)
                                .appearance(false)
                                .bordered(false),
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
                    .map(|project| (project.name.clone(), project.work_dir.clone()))
                    .collect::<Vec<_>>(),
                state.sidebar_project_filter.clone(),
            )
        };
        let selected_label = selected_filter
            .as_ref()
            .and_then(|selected| {
                projects
                    .iter()
                    .find(|(_, work_dir)| work_dir == selected)
                    .map(|(name, _)| name.clone())
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
                        .label(selected_label)
                        .dropdown_caret(true)
                        .selected(true)
                        .w_full()
                        .justify_start()
                        .dropdown_menu(move |menu, _window, _cx| {
                            let all_model = filter_model.clone();
                            let mut menu = menu.item(
                                PopupMenuItem::new("All projects")
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
                            for (name, work_dir) in projects.clone() {
                                let model = filter_model.clone();
                                let checked = selected_filter.as_ref() == Some(&work_dir);
                                menu =
                                    menu.item(PopupMenuItem::new(name).checked(checked).on_click(
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
                                    ));
                            }
                            menu
                        }),
                ),
            )
            .child(
                Button::new("attach-project-btn")
                    .icon(IconName::Plus)
                    .tooltip("Attach Project")
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

    fn render_history_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .pt_2()
            .pb_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("RECENT"),
                    )
                    .child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .rounded_full()
                            .bg(theme.secondary)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(session_count.to_string()),
                    ),
            )
    }

    fn render_session_card(
        &self,
        session: &SessionInfo,
        is_active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let health = session.health.clone();
        let state = self.model.read(cx);
        let is_generating = state.session_is_generating(&session.session_file)
            || (is_active && state.is_generating);
        let is_working = health == SessionHealth::Working || is_generating;

        let status_indicator = if is_working {
            Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(3.0))
                    .px_1()
                    .rounded_full()
                    .bg(theme.primary.opacity(0.1))
                    .child(gpui_component::spinner::Spinner::new().xsmall())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.primary)
                            .child("Running"),
                    )
                    .into_any_element(),
            )
        } else {
            match health {
                SessionHealth::Warning => Some(
                    Tag::new()
                        .child("!")
                        .with_variant(TagVariant::Warning)
                        .small()
                        .into_any_element(),
                ),
                SessionHealth::Working | SessionHealth::Healthy => None,
            }
        };

        let bg_color = if is_active {
            theme.sidebar_accent
        } else {
            gpui::transparent_black()
        };

        let border_color = if is_active {
            theme.border.opacity(0.4)
        } else {
            gpui::transparent_black()
        };

        let title_color = if is_active {
            theme.foreground
        } else {
            theme.sidebar_foreground
        };

        let work_dir = session.work_dir.clone();
        let session_id = session.id.clone();
        let model = self.model.clone();
        let context_work_dir = session.work_dir.clone();
        let context_session_id = session.id.clone();
        let context_model = self.model.clone();
        let copy_session_file = session.session_file.display().to_string();
        let export_log_source = session.session_file.clone();
        let export_trajectory_title = session.title.clone();
        let quick_settle_model = self.model.clone();
        let quick_settle_work_dir = session.work_dir.clone();
        let quick_settle_session_id = session.id.clone();
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

        let pr_info = session_pr_info(session, &self.model.read(cx).git_prs).cloned();

        let pr_meta = pr_info.map(|pr| {
            let state_upper = pr.state.to_uppercase();
            let is_merged = state_upper == "MERGED";
            let is_draft = pr.is_draft || state_upper == "DRAFT";
            let is_closed = state_upper == "CLOSED";
            let tooltip = pr_status_tooltip(&pr);

            let (pr_bg, pr_fg, pr_label) = if is_merged {
                (
                    theme.success.opacity(0.18),
                    theme.success,
                    format!("#{}", pr.number),
                )
            } else if is_draft {
                (
                    theme.secondary,
                    theme.muted_foreground,
                    format!("#{}", pr.number),
                )
            } else if is_closed {
                (
                    theme.danger.opacity(0.12),
                    theme.danger,
                    format!("#{}", pr.number),
                )
            } else {
                (
                    theme.primary.opacity(0.12),
                    theme.primary,
                    format!("#{}", pr.number),
                )
            };

            let ci_chip = if pr.failing_checks > 0 {
                Some(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.0))
                        .px_1()
                        .py(px(0.5))
                        .rounded(px(3.0))
                        .bg(theme.danger.opacity(0.12))
                        .text_color(theme.danger)
                        .child(IconName::Close)
                        .child(format!("{} fail", pr.failing_checks)),
                )
            } else if pr.pending_checks > 0 {
                Some(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.0))
                        .px_1()
                        .py(px(0.5))
                        .rounded(px(3.0))
                        .bg(theme.warning.opacity(0.12))
                        .text_color(theme.warning)
                        .child(IconName::Asterisk)
                        .child(format!("{}/{}", pr.passing_checks, pr.total_checks)),
                )
            } else if pr.total_checks > 0 {
                Some(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.0))
                        .px_1()
                        .py(px(0.5))
                        .rounded(px(3.0))
                        .bg(theme.success.opacity(0.12))
                        .text_color(theme.success)
                        .child(IconName::CircleCheck)
                        .child(format!("{}/{}", pr.passing_checks, pr.total_checks)),
                )
            } else {
                None
            };

            let comments_chip = (pr.comments_count > 0).then(|| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(2.0))
                    .px_1()
                    .py(px(0.5))
                    .rounded(px(3.0))
                    .bg(theme.secondary)
                    .text_color(theme.muted_foreground)
                    .child(
                        svg()
                            .path("icons/git/comments.svg")
                            .size(px(11.0))
                            .text_color(theme.muted_foreground),
                    )
                    .child(format!("{}", pr.comments_count))
            });

            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child(
                    Button::new(SharedString::from(format!(
                        "session-pr-{}-{}",
                        session.id, pr.number
                    )))
                    .icon(Icon::default().path("icons/git/compare.svg"))
                    .label(pr_label)
                    .tooltip(tooltip)
                    .ghost()
                    .xsmall()
                    .bg(pr_bg)
                    .text_color(pr_fg),
                )
                .children(ci_chip)
                .children(comments_chip)
        });

        let mut row2_items = Vec::new();
        row2_items.push(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .text_color(theme.muted_foreground)
                .child(
                    Icon::new(IconName::Folder)
                        .xsmall()
                        .text_color(theme.muted_foreground.opacity(0.6)),
                )
                .child(div().max_w(px(110.0)).truncate().child(project))
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

        if session.is_worktree {
            let (background, foreground, tooltip) = if session.worktree_available {
                (
                    theme.secondary,
                    theme.muted_foreground,
                    format!(
                        "Local worktree\nChecked out at {}",
                        session.runtime_work_dir.display()
                    ),
                )
            } else {
                (
                    theme.warning.opacity(0.12),
                    theme.warning,
                    format!(
                        "Worktree unavailable\nNot checked out locally\nRecorded path: {}\nSession history remains available",
                        session.runtime_work_dir.display()
                    ),
                )
            };
            row2_items.push(
                Button::new(SharedString::from(format!(
                    "session-worktree-{}",
                    session.id
                )))
                .icon(Icon::default().path("icons/git/branch.svg"))
                .tooltip(tooltip)
                .ghost()
                .xsmall()
                .bg(background)
                .text_color(foreground)
                .into_any_element(),
            );
        }

        div()
            .id(SharedString::from(format!("session-card-{}", session.id)))
            .group("session-card")
            .relative()
            .flex()
            .items_stretch()
            .w_full()
            .my(px(1.5))
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
            .cursor_pointer()
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
                        .left(px(0.0))
                        .top(px(4.0))
                        .bottom(px(4.0))
                        .w(px(2.5))
                        .rounded_r_full()
                        .bg(theme.primary),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .px_2p5()
                    .py_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(if is_active {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::MEDIUM
                                    })
                                    .text_color(title_color)
                                    .truncate()
                                    .child(session.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .children(status_indicator)
                                    .when(!is_working, |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .group_hover("session-card", |style| {
                                                    style.opacity(0.0)
                                                })
                                                .child(time_ago),
                                        )
                                    })
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "settle-session-{}",
                                            session.id
                                        )))
                                        .icon(IconName::Check)
                                        .ghost()
                                        .xsmall()
                                        .compact()
                                        .absolute()
                                        .right(px(10.0))
                                        .top(px(8.0))
                                        .opacity(0.0)
                                        .group_hover("session-card", |style| style.opacity(1.0))
                                        .tooltip("Archive session")
                                        .on_click(
                                            move |_event, _window, cx| {
                                                quick_settle_model.update(cx, |state, cx| {
                                                    controller::dispatch(
                                                        state,
                                                        AppAction::SettleSession {
                                                            work_dir: quick_settle_work_dir.clone(),
                                                            session_id: quick_settle_session_id
                                                                .clone(),
                                                        },
                                                    );
                                                    cx.notify();
                                                });
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
                            .overflow_hidden()
                            .children(row2_items),
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
                let remove_model = context_model.clone();
                let remove_work_dir = context_work_dir.clone();
                let remove_session_id = context_session_id.clone();

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
                            let result = build_diagnostic_export(
                                &source,
                                &session_id,
                                &title,
                                &work_dir,
                                runtime.as_deref(),
                                trajectory,
                                true,
                            )
                            .and_then(|value| {
                                serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())
                            })
                            .and_then(|bytes| {
                                std::fs::write(destination.path(), bytes)
                                    .map_err(|error| error.to_string())
                            });
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
                            let result = build_diagnostic_export(
                                &source,
                                &session_id,
                                &title,
                                &work_dir,
                                runtime.as_deref(),
                                trajectory,
                                false,
                            )
                            .and_then(|value| {
                                serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())
                            })
                            .and_then(|bytes| {
                                std::fs::write(destination.path(), bytes)
                                    .map_err(|error| error.to_string())
                            });
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
                        let model = settle_model.clone();
                        let work_dir = settle_work_dir.clone();
                        let session_id = settle_session_id.clone();
                        window.open_alert_dialog(cx, {
                            let model = model.clone();
                            let work_dir = work_dir.clone();
                            let session_id = session_id.clone();
                            move |alert, _window, _cx| {
                                let model = model.clone();
                                let work_dir = work_dir.clone();
                                let session_id = session_id.clone();
                                alert
                                    .title("Archive session?")
                                    .description(format!(
                                        "This removes session {session_id} from the active list."
                                    ))
                                    .show_cancel(true)
                                    .on_ok(move |_event, _window, cx| {
                                        model.update(cx, |state, cx| {
                                            controller::dispatch(
                                                state,
                                                AppAction::SettleSession {
                                                    work_dir: work_dir.clone(),
                                                    session_id: session_id.clone(),
                                                },
                                            );
                                            cx.notify();
                                        });
                                        true
                                    })
                            }
                        });
                    }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Remove Session").on_click(move |_event, window, cx| {
                        let model = remove_model.clone();
                        let work_dir = remove_work_dir.clone();
                        let session_id = remove_session_id.clone();
                        window.open_alert_dialog(cx, {
                            let model = model.clone();
                            let work_dir = work_dir.clone();
                            let session_id = session_id.clone();
                            move |alert, _window, _cx| {
                                let model = model.clone();
                                let work_dir = work_dir.clone();
                                let session_id = session_id.clone();
                                alert
                                    .title("Remove session?")
                                    .description(format!(
                                        "This permanently removes session {session_id}."
                                    ))
                                    .button_props(
                                        DialogButtonProps::default()
                                            .ok_text("Remove")
                                            .ok_variant(ButtonVariant::Danger)
                                            .show_cancel(true),
                                    )
                                    .on_ok(move |_event, _window, cx| {
                                        model.update(cx, |state, cx| {
                                            controller::dispatch(
                                                state,
                                                AppAction::RemoveSession {
                                                    work_dir: work_dir.clone(),
                                                    session_id: session_id.clone(),
                                                },
                                            );
                                            cx.notify();
                                        });
                                        true
                                    })
                            }
                        });
                    }),
                )
            })
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let github_model = self.model.clone();
        let settings_model = self.model.clone();
        let theme = cx.theme().colors;
        let github_selected =
            self.model.read(cx).workspace_page == crate::state::WorkspacePage::GitHub;

        div()
            .flex_none()
            .px_3()
            .py_2()
            .child(
                Button::new("sidebar-github")
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_start()
                            .gap_2()
                            .child(IconName::Github)
                            .child("GitHub"),
                    )
                    .ghost()
                    .selected(github_selected)
                    .w_full()
                    .justify_start()
                    .text_color(theme.muted_foreground)
                    .on_click(move |_event, _window, cx| {
                        github_model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::OpenGitHub);
                            cx.notify();
                        });
                    }),
            )
            .child(
                Button::new("sidebar-settings")
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

    /// Filter, group, and sort sessions for the history list. Only runs when
    /// `sidebar_fingerprint` changes; `render_history` otherwise reuses the
    /// cached result instead of cloning and sorting every row per frame.
    fn build_history_rows(&self, state: &AppState, query: &str, now: u64) -> Vec<HistoryRow> {
        // Grouping before sorting is equivalent to a global sort because
        // bucket insertion preserves scan order.
        let mut grouped: Vec<(DateGroup, Vec<SessionInfo>)> = vec![
            (DateGroup::Today, Vec::new()),
            (DateGroup::Yesterday, Vec::new()),
            (DateGroup::ThisWeek, Vec::new()),
            (DateGroup::Older, Vec::new()),
        ];
        let mut seen_session_ids = std::collections::HashSet::new();
        for session in state
            .projects
            .iter()
            .filter(|project| {
                state
                    .sidebar_project_filter
                    .as_ref()
                    .is_none_or(|selected| &project.work_dir == selected)
            })
            .flat_map(|project| project.sessions.iter())
        {
            if !seen_session_ids.insert(session.id.clone()) {
                continue;
            }
            if !query.is_empty()
                && !session.title.to_lowercase().contains(query)
                && !session.id.to_lowercase().contains(query)
            {
                continue;
            }
            let mut session = session.clone();
            if state.session_is_generating(&session.session_file) {
                session.health = SessionHealth::Working;
            }
            let group = get_date_group(session.updated_at, now);
            if let Some((_, entries)) = grouped
                .iter_mut()
                .find(|(candidate, _)| *candidate == group)
            {
                entries.push(session);
            }
        }
        for (_, entries) in grouped.iter_mut() {
            entries.sort_by(|left, right| {
                right
                    .updated_at
                    .cmp(&left.updated_at)
                    .then_with(|| left.title.cmp(&right.title))
            });
        }
        flatten_history_groups(grouped)
    }

    fn render_history_row(
        &mut self,
        index: usize,
        _window: &mut Window,
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
                .pt(if group == DateGroup::Today {
                    px(4.0)
                } else {
                    px(12.0)
                })
                .pb_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground.opacity(0.8))
                        .child(group.label()),
                )
                .child(div().h(px(1.0)).flex_1().bg(theme.border.opacity(0.35)))
                .into_any_element(),
            Some(HistoryRow::Session(session)) => {
                let state = self.model.read(cx);
                let is_active = state.active_work_dir.as_ref() == Some(&session.work_dir)
                    && state.active_session_id.as_deref() == Some(session.id.as_str());
                self.render_session_card(&session, is_active, cx)
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    fn render_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let model = self.model.clone();
        let state = self.model.read(cx);
        let query = state.search_query.trim().to_lowercase();
        let now = now_unix_secs();

        let fingerprint = sidebar_fingerprint(state, now);
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
                .child(if query.is_empty() {
                    "No tasks yet. Start your first task."
                } else {
                    "No matching tasks."
                })
                .children(query.is_empty().then(|| {
                    Button::new("empty-history-new-task")
                        .icon(IconName::Plus)
                        .label("New Task")
                        .ghost()
                        .small()
                        .on_click(move |_event, _window, cx| {
                            model.update(cx, |state, cx| {
                                controller::dispatch(state, AppAction::BeginNewTask);
                                cx.notify();
                            });
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
        flatten_history_groups, format_time_ago, pr_status_label, pr_status_tooltip,
        same_history_row_identity, session_pr_info, sidebar_session_fingerprint, DateGroup,
        HistoryRow,
    };
    use crate::state::{SessionHealth, SessionInfo};
    use std::collections::HashMap;
    use threadlane_git::GitHubPrInfo;

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

    #[test]
    fn history_rows_keep_group_headers_and_skip_empty_groups() {
        let rows = flatten_history_groups(vec![
            (DateGroup::Today, Vec::new()),
            (DateGroup::Yesterday, vec![session("one")]),
            (DateGroup::Older, vec![session("two")]),
        ]);

        assert!(matches!(rows[0], HistoryRow::Group(DateGroup::Yesterday)));
        assert!(matches!(&rows[1], HistoryRow::Session(item) if item.id == "one"));
        assert!(matches!(rows[2], HistoryRow::Group(DateGroup::Older)));
        assert!(matches!(&rows[3], HistoryRow::Session(item) if item.id == "two"));
        assert!(same_history_row_identity(
            &rows[1],
            &HistoryRow::Session(session("one"))
        ));
        assert!(!same_history_row_identity(&rows[1], &rows[3]));
    }

    #[test]
    fn recent_timestamps_use_stable_labels() {
        assert_eq!(format_time_ago(100, 100), "Just now");
        assert_eq!(format_time_ago(41, 100), "Just now");
        assert_eq!(format_time_ago(40, 100), "1m ago");
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
        let first = sidebar_session_fingerprint(&item);

        item.git_branch = Some("feature/two".into());

        assert_ne!(first, sidebar_session_fingerprint(&item));
    }
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
