use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::{InputEvent, InputState, TextareaState};
use gpui_component::resizable::ResizableState;
use gpui_component::scroll::Scrollbar;
use gpui_component::text::TextViewState;
use gpui_component::WindowExt;
use threadlane_git::{
    GitHubIssueDetail, GitHubIssueRef, GitHubPrInfo, GitHubRepository, PrFileViewedStatus,
};

use threadlane_ui_kit::github as kit_github;
use threadlane_ui_state::actions::AppAction;
use threadlane_ui_state::controller;
use threadlane_ui_state::{AppState, SessionInfo};

use super::diff::*;
use super::issue_dialog::*;
use super::timeline::*;
use super::types::*;

actions!(
    threadlane_github,
    [SelectPrevious, SelectNext, OpenSelected]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, Some(GITHUB_LIST_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(GITHUB_LIST_CONTEXT)),
        KeyBinding::new("enter", OpenSelected, Some(GITHUB_LIST_CONTEXT)),
        KeyBinding::new("left", SelectPrevious, Some(GITHUB_PR_TABS_CONTEXT)),
        KeyBinding::new("right", SelectNext, Some(GITHUB_PR_TABS_CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(GITHUB_PR_FILE_LIST_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(GITHUB_PR_FILE_LIST_CONTEXT)),
        KeyBinding::new("enter", OpenSelected, Some(GITHUB_PR_FILE_LIST_CONTEXT)),
    ]);
}

use threadlane_ui_kit::github::{pr_file_action_ix, PrFileAction};

use threadlane_ui_kit::github::pr_review_label;

fn github_server_query(query: &str) -> Option<&str> {
    let query = query.trim();
    (!query.is_empty()).then_some(query)
}

fn github_empty_message(tab: GitHubTab, state: GitHubStateFilter, query: &str) -> String {
    let items = tab.label().to_lowercase();
    if github_server_query(query).is_some() {
        format!(
            "No matching {items} in this scope. Try another search or state filter."
        )
    } else {
        format!("No {} {items} in this scope.", state.value())
    }
}

#[cfg(test)]
pub(crate) fn selected_issue_after_refresh(
    selected: Option<u64>,
    issues: &[threadlane_git::GitHubIssueSummary],
) -> Option<u64> {
    selected
        .filter(|selected| issues.iter().any(|issue| issue.issue.number == *selected))
        .or_else(|| issues.first().map(|issue| issue.issue.number))
}

fn same_issue(left: &GitHubIssueRef, right: &GitHubIssueRef) -> bool {
    left.host == right.host
        && left.owner == right.owner
        && left.repo == right.repo
        && left.number == right.number
}

#[cfg(test)]
pub(crate) fn linked_session_ids<'a>(
    sessions: &'a [SessionInfo],
    issue: &GitHubIssueRef,
) -> Vec<&'a str> {
    sessions
        .iter()
        .filter(|session| {
            session
                .github_issue
                .as_ref()
                .is_some_and(|linked| same_issue(linked, issue))
        })
        .map(|session| session.id.as_str())
        .collect()
}

fn linked_sessions_across_projects<'a>(
    projects: &[(&'a str, &'a [SessionInfo])],
    issue: &GitHubIssueRef,
) -> Vec<(&'a str, &'a SessionInfo)> {
    projects
        .iter()
        .flat_map(|(project_name, sessions)| {
            sessions.iter().filter_map(|session| {
                session
                    .github_issue
                    .as_ref()
                    .is_some_and(|linked| same_issue(linked, issue))
                    .then_some((*project_name, session))
            })
        })
        .collect()
}

pub(crate) fn linked_session_status(
    session: &SessionInfo,
    has_pending_permission: bool,
    is_generating: bool,
) -> &'static str {
    if has_pending_permission {
        "Needs permission"
    } else if !session.worktree_available {
        "Not checked out"
    } else if is_generating || session.health == threadlane_ui_state::SessionHealth::Working {
        "Working"
    } else if session.health == threadlane_ui_state::SessionHealth::Warning {
        "Needs attention"
    } else {
        "Ready"
    }
}

pub(crate) fn list_count_splice(
    old_count: usize,
    new_count: usize,
) -> Option<(Range<usize>, usize)> {
    match new_count.cmp(&old_count) {
        std::cmp::Ordering::Greater => Some((old_count..old_count, new_count - old_count)),
        std::cmp::Ordering::Less => Some((new_count..old_count, 0)),
        std::cmp::Ordering::Equal => None,
    }
}

fn reconcile_list_count(state: &ListState, old_count: usize, new_count: usize) {
    if let Some((range, replacement_count)) = list_count_splice(old_count, new_count) {
        state.splice(range, replacement_count);
    }
}

type GitHubLinkFingerprintRow<'a> = (
    &'a str,
    &'a std::path::Path,
    &'a SessionInfo,
    Option<&'a GitHubPrInfo>,
    bool,
    bool,
);

fn github_link_fingerprint_rows<'a>(
    active_session_id: Option<&str>,
    rows: impl IntoIterator<Item = GitHubLinkFingerprintRow<'a>>,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    active_session_id.hash(&mut hasher);
    for (project_name, project_work_dir, session, pr, pending_permission, is_generating) in rows {
        if session.github_issue.is_none() && session.git_branch.is_none() {
            continue;
        }
        project_name.hash(&mut hasher);
        project_work_dir.hash(&mut hasher);
        if let Some(issue) = session.github_issue.as_ref() {
            true.hash(&mut hasher);
            issue.host.hash(&mut hasher);
            issue.owner.hash(&mut hasher);
            issue.repo.hash(&mut hasher);
            issue.number.hash(&mut hasher);
        } else {
            false.hash(&mut hasher);
        }
        linked_session_fingerprint(session, pr).hash(&mut hasher);
        pending_permission.hash(&mut hasher);
        is_generating.hash(&mut hasher);
    }
    hasher.finish()
}

fn github_link_fingerprint(state: &AppState) -> u64 {
    let mut hasher = DefaultHasher::new();
    state.active_work_dir.hash(&mut hasher);
    for session in state.projects.iter().flat_map(|project| &project.sessions) {
        threadlane_ui_state::hash_session_identity(&mut hasher, session);
        session.work_dir.hash(&mut hasher);
        session.runtime_work_dir.hash(&mut hasher);
        state
            .git_statuses
            .get(&session.runtime_work_dir)
            .map(|status| (&status.branch, status.detached))
            .hash(&mut hasher);
    }
    github_link_fingerprint_rows(
        state.active_session_id.as_deref(),
        state.projects.iter().flat_map(|project| {
            project.sessions.iter().map(move |session| {
                let pr = session.git_branch.as_ref().and_then(|branch| {
                    state
                        .git_prs
                        .get(&(session.work_dir.clone(), branch.clone()))
                        .and_then(|pr| pr.as_ref())
                });
                (
                    project.name.as_str(),
                    project.work_dir.as_path(),
                    session,
                    pr,
                    state.pending_permissions.contains_key(&session.id),
                    state.session_is_generating(&session.session_file),
                )
            })
        }),
    )
    .hash(&mut hasher);
    hasher.finish()
}

fn linked_session_fingerprint(session: &SessionInfo, pr: Option<&GitHubPrInfo>) -> u64 {
    let mut hasher = DefaultHasher::new();
    threadlane_ui_state::hash_session_identity(&mut hasher, session);
    match pr {
        Some(pr) => {
            true.hash(&mut hasher);
            pr.number.hash(&mut hasher);
            pr.state.hash(&mut hasher);
            pr.is_draft.hash(&mut hasher);
            pr.head_ref.hash(&mut hasher);
            pr.base_ref.hash(&mut hasher);
        }
        None => false.hash(&mut hasher),
    }
    hasher.finish()
}



/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_since_epoch(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year.rem_euclid(400);
    let month_index = (month + 9).rem_euclid(12);
    let day_of_year = (153 * month_index + 2).div_euclid(5) + day - 1;
    let day_of_era =
        year_of_era * 365 + year_of_era.div_euclid(4) - year_of_era.div_euclid(100) + day_of_year;
    era * 146097 + day_of_era - 719468
}

fn parse_github_timestamp(value: &str) -> Option<u64> {
    let value = value.trim().trim_end_matches('Z');
    let (date, time) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts
        .next()
        .and_then(|part| part.split(['.', '+', '-']).next())
        .and_then(|part| part.parse().ok())?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_since_epoch(year, month, day);
    u64::try_from(days * 86400 + hour * 3600 + minute * 60 + second).ok()
}

fn github_now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Relative display for `gh` ISO-8601 timestamps; falls back to the raw value.
fn format_github_time(value: &str, now: u64) -> String {
    let Some(timestamp) = parse_github_timestamp(value) else {
        return value.to_owned();
    };
    let seconds = now.saturating_sub(timestamp);
    match seconds {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{}h ago", seconds / 3600),
        86400..=2592000 => format!("{}d ago", seconds / 86400),
        _ => value.split('T').next().unwrap_or(value).to_owned(),
    }
}

/// `owner/repo · project`, collapsing the project when the repo already names it.
fn scope_context_line(owner: &str, repo: &str, project_name: &str) -> String {
    let full = format!("{owner}/{repo}");
    if project_name.is_empty()
        || repo == project_name
        || full.ends_with(&format!("/{project_name}"))
    {
        full
    } else {
        format!("{full} · {project_name}")
    }
}

fn github_error_message(error: &str) -> String {
    let normalized = error.to_lowercase();
    // A missing binary fails at spawn, before gh can report anything itself.
    if normalized.contains("could not start gh") {
        "GitHub CLI (gh) isn’t installed or isn’t on PATH. Install it, then refresh.".into()
    } else if normalized.contains("rate limit")
        || normalized.contains("rate_limit")
        || normalized.contains("http 429")
    {
        "GitHub’s API limit has been reached. Wait before retrying.".into()
    } else if normalized.contains("auth") || normalized.contains("login") {
        "GitHub authentication is required. Sign in with gh and refresh.".into()
    } else if normalized.contains("remote") || normalized.contains("repository") {
        "This project does not have an accessible GitHub remote.".into()
    } else if normalized.contains("connect")
        || normalized.contains("network")
        || normalized.contains("resolve")
    {
        "GitHub is offline. Check your connection and refresh.".into()
    } else {
        "GitHub couldn’t load this view. Retry, or copy details to inspect the error.".into()
    }
}
pub struct GitHubView {
    model: Entity<AppState>,
    window_controls_inset: Option<Pixels>,
    project_work_dir: Option<PathBuf>,
    scope: GitHubScope,
    scope_initialized: bool,
    last_targets: Vec<PathBuf>,
    repository: Option<GitHubRepository>,
    /// Last applied navigation target, retained with the list/request state.
    /// AppState owns navigation so the sidebar and in-page tabs stay in sync.
    tab: GitHubTab,
    state_filter: GitHubStateFilter,
    query_input: Entity<InputState>,
    query_revision: u64,
    issues: Vec<ScopedIssue>,
    pull_requests: Vec<ScopedPr>,
    selected_issue: Option<GitHubItemKey>,
    selected_pr: Option<GitHubItemKey>,
    issue_detail: Option<GitHubIssueDetail>,
    pr_detail: Option<GitHubPrInfo>,
    detail_body: Entity<TextViewState>,
    comment_rows: Vec<(String, String, String)>,
    comment_list_state: ListState,
    pr_timeline_rows: Vec<PrTimelineRow>,
    pr_timeline_list_state: ListState,
    pr_conversation_scroll: ScrollHandle,
    pr_file_list_state: ListState,
    pr_diff_body: Entity<TextViewState>,
    pr_selections: PrWorkspaceSelections,
    pr_drafts: PrCommentDrafts,
    /// Per-PR Viewed marker lifecycle (personal reading state, kept per
    /// workspace key so delayed results can't cross repos/PRs/accounts).
    pr_viewed: PrViewedStates,
    /// Read/write seam for viewed markers; tests substitute delayed fakes.
    viewed_transport: PrViewedTransport,
    pr_comment_input: Entity<TextareaState>,
    pr_comment_input_key: Option<PrWorkspaceKey>,
    pr_reply_input: Entity<TextareaState>,
    pr_reply_input_key: Option<(PrWorkspaceKey, String)>,
    active_diff_request: Option<PrDiffRequest>,
    diff_revision: u64,
    diff_loading: bool,
    diff_error: Option<String>,
    list_loading: bool,
    detail_loading: bool,
    list_error: Option<String>,
    detail_error: Option<String>,
    issue_limit: usize,
    pr_limit: usize,
    issue_has_more: bool,
    pr_has_more: bool,
    active_list_request: Option<GitHubRequest>,
    list_cancelled: Arc<AtomicBool>,
    active_detail_request: Option<GitHubRequest>,
    issue_list_state: ListState,
    pr_list_state: ListState,
    detail_split_state: Entity<ResizableState>,
    list_focus: FocusHandle,
    pr_tabs_focus: FocusHandle,
    pr_file_focus: FocusHandle,
    debounce_task: Option<Task<()>>,
    issue_comment_draft: String,
    pr_review_draft: String,
    linked_sessions_fingerprint: u64,
    last_github_list_revision: u64,
    /// Feedback line for issue mutations (create/close/labels) and their
    /// in-flight state. Surfaced under the detail header buttons.
    issue_action_status: Option<String>,
    issue_action_pending: bool,
    /// Model-side completions (label suggestions) arrive from Tokio workers
    /// that cannot touch entities; the constructor pumps them into the view.
    issue_action_tx: tokio::sync::mpsc::UnboundedSender<IssueActionEvent>,
    _subscriptions: Vec<Subscription>,
}

/// Completion of a model-side issue action, pumped into the view.
enum IssueActionEvent {
    Finished(Result<String, String>),
}

impl GitHubView {
    pub fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search — e.g. is:open assignee:@me …")
        });
        let detail_body = cx.new(|cx| TextViewState::markdown("", cx));
        let pr_diff_body = cx.new(|cx| TextViewState::markdown("", cx));
        let pr_comment_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Add a comment…")
                .auto_grow(2, 6)
                .soft_wrap(true)
        });
        let pr_reply_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Write a reply…")
                .auto_grow(2, 6)
                .soft_wrap(true)
        });
        let detail_split_state = cx.new(|_| ResizableState::default());
        let linked_sessions_fingerprint = github_link_fingerprint(model.read(cx));
        let tab = model.read(cx).github_tab;

        let model_subscription = cx.observe(&model, |this, model, cx| {
            let state = model.read(cx);
            let linked_sessions_fingerprint = github_link_fingerprint(state);
            let github_list_revision = state.github_list_revision;
            let tab = state.github_tab;
            let attached: Vec<PathBuf> =
                state.projects.iter().map(|p| p.work_dir.clone()).collect();
            let scope_valid = match &this.scope {
                GitHubScope::All => true,
                GitHubScope::Project(work_dir) => attached.contains(work_dir),
            };
            if !this.scope_initialized {
                this.scope = GitHubScope::All;
                this.scope_initialized = true;
                this.project_work_dir = state.active_work_dir.clone();
                this.reset_list_state(cx);
                this.fetch_list(cx);
            } else if !scope_valid {
                this.select_scope(GitHubScope::All, cx);
            } else {
                // Scope is explicit and sticky: active chat session changes
                // must not silently retarget the GitHub list.
                this.project_work_dir = state.active_work_dir.clone();
                let current_targets: Vec<PathBuf> = this
                    .scope
                    .projects(
                        &state
                            .projects
                            .iter()
                            .map(|p| (p.name.clone(), p.work_dir.clone()))
                            .collect::<Vec<_>>(),
                    )
                    .into_iter()
                    .map(|(_, dir)| dir)
                    .collect();
                if current_targets != this.last_targets {
                    this.fetch_list(cx);
                }
            }
            if this.linked_sessions_fingerprint != linked_sessions_fingerprint {
                this.linked_sessions_fingerprint = linked_sessions_fingerprint;
                cx.notify();
            }
            // Out-of-band GitHub mutations (issue create dialog) bump the
            // revision; refetch the list to show the new row.
            if this.last_github_list_revision != github_list_revision {
                this.last_github_list_revision = github_list_revision;
                this.fetch_list(cx);
            }
            this.select_tab(tab, cx);
        });
        let input_subscription = cx.subscribe_in(
            &query_input,
            window,
            |this, input, event: &InputEvent, _window, cx| match event {
                InputEvent::Change => {
                    let query = input.read(cx).value().to_string();
                    this.query_revision = this.query_revision.saturating_add(1);
                    this.clear_selection();
                    this.invalidate_detail(cx);
                    this.schedule_query(query, cx);
                }
                InputEvent::PressEnter { .. } => {
                    this.debounce_task.take();
                    this.query_revision = this.query_revision.saturating_add(1);
                    this.clear_selection();
                    this.invalidate_detail(cx);
                    this.fetch_list(cx);
                }
                _ => {}
            },
        );
        let comment_draft_subscription =
            cx.subscribe(&pr_comment_input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some(key) = this.pr_comment_input_key.clone() {
                        let body = input.read(cx).value().to_string();
                        this.pr_drafts.set_body(key, body);
                        cx.notify();
                    }
                }
            });
        let reply_draft_subscription =
            cx.subscribe(&pr_reply_input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some((key, _)) = this.pr_reply_input_key.clone() {
                        let body = input.read(cx).value().to_string();
                        this.pr_drafts.set_reply_body(&key, body);
                        cx.notify();
                    }
                }
            });

        Self {
            model,
            window_controls_inset: None,
            project_work_dir: None,
            scope: GitHubScope::All,
            scope_initialized: false,
            last_targets: Vec::new(),
            repository: None,
            tab,
            state_filter: GitHubStateFilter::Open,
            query_input,
            query_revision: 0,
            issues: Vec::new(),
            pull_requests: Vec::new(),
            selected_issue: None,
            selected_pr: None,
            issue_detail: None,
            pr_detail: None,
            detail_body,
            comment_rows: Vec::new(),
            comment_list_state: ListState::new(0, ListAlignment::Top, window.rem_size() * 6.0),
            pr_timeline_rows: Vec::new(),
            pr_timeline_list_state: ListState::new(0, ListAlignment::Top, window.rem_size() * 7.0),
            pr_conversation_scroll: ScrollHandle::new(),
            pr_file_list_state: ListState::new(0, ListAlignment::Top, window.rem_size() * 3.25),
            pr_diff_body,
            pr_selections: PrWorkspaceSelections::default(),
            pr_drafts: PrCommentDrafts::default(),
            pr_viewed: PrViewedStates::default(),
            viewed_transport: PrViewedTransport::default(),
            pr_comment_input,
            pr_comment_input_key: None,
            pr_reply_input,
            pr_reply_input_key: None,
            active_diff_request: None,
            diff_revision: 0,
            diff_loading: false,
            diff_error: None,
            list_loading: false,
            detail_loading: false,
            list_error: None,
            detail_error: None,
            issue_limit: PAGE_SIZE,
            pr_limit: PAGE_SIZE,
            issue_has_more: false,
            pr_has_more: false,
            active_list_request: None,
            list_cancelled: Arc::new(AtomicBool::new(false)),
            active_detail_request: None,
            issue_list_state: ListState::new(0, ListAlignment::Top, window.rem_size() * 5.5),
            pr_list_state: ListState::new(0, ListAlignment::Top, window.rem_size() * 4.875),
            detail_split_state,
            list_focus: cx.focus_handle(),
            pr_tabs_focus: cx.focus_handle(),
            pr_file_focus: cx.focus_handle(),
            debounce_task: None,
            issue_comment_draft: String::new(),
            pr_review_draft: String::new(),
            linked_sessions_fingerprint,
            last_github_list_revision: 0,
            issue_action_status: None,
            issue_action_pending: false,
            issue_action_tx: {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                // Model-side completions arrive from Tokio workers that
                // cannot touch entities; pump them into the view here.
                cx.spawn(async move |this, cx| {
                    while let Some(event) = rx.recv().await {
                        let IssueActionEvent::Finished(outcome) = event;
                        let _ = this.update(cx, |this, cx| {
                            this.issue_action_pending = false;
                            match outcome {
                                Ok(note) => {
                                    this.issue_action_status = Some(note);
                                    this.fetch_list(cx);
                                    this.fetch_detail(cx);
                                }
                                Err(error) => {
                                    this.issue_action_status = Some(error);
                                }
                            }
                            cx.notify();
                        });
                    }
                })
                .detach();
                tx
            },
            _subscriptions: vec![
                model_subscription,
                input_subscription,
                comment_draft_subscription,
                reply_draft_subscription,
            ],
        }
    }

    pub fn sync_active_project(&mut self, cx: &mut Context<Self>) {
        if !self.scope_initialized {
            self.scope = GitHubScope::All;
            self.scope_initialized = true;
            self.project_work_dir = self.model.read(cx).active_work_dir.clone();
            self.reset_list_state(cx);
            self.fetch_list(cx);
        }
    }

    fn attached_projects(&self, cx: &App) -> Vec<(String, PathBuf)> {
        self.model
            .read(cx)
            .projects
            .iter()
            .map(|project| (project.name.clone(), project.work_dir.clone()))
            .collect()
    }

    fn scope_targets(&self, cx: &App) -> Vec<(String, PathBuf)> {
        self.scope.projects(&self.attached_projects(cx))
    }

    pub fn open_linked_task(
        &mut self,
        work_dir: PathBuf,
        number: u64,
        cx: &mut Context<Self>,
    ) {
        self.set_tab(GitHubTab::Issues, cx);
        self.project_work_dir = Some(work_dir.clone());
        self.scope = GitHubScope::Project(work_dir.clone());
        self.scope_initialized = true;
        self.reset_list_state(cx);
        self.selected_issue = Some(GitHubItemKey {
            project: work_dir,
            number,
        });
        self.fetch_list(cx);
    }

    fn select_scope(&mut self, scope: GitHubScope, cx: &mut Context<Self>) {
        if self.scope_initialized && self.scope == scope {
            return;
        }
        self.scope = scope;
        self.scope_initialized = true;
        self.reset_list_state(cx);
        self.fetch_list(cx);
    }
    fn reset_list_state(&mut self, cx: &mut Context<Self>) {
        self.list_cancelled.store(true, Ordering::Relaxed);
        self.repository = None;
        self.issues.clear();
        self.pull_requests.clear();
        self.selected_issue = None;
        self.selected_pr = None;
        self.issue_detail = None;
        self.pr_detail = None;
        self.comment_rows.clear();
        self.comment_list_state.reset(0);
        self.pr_timeline_rows.clear();
        self.pr_timeline_list_state.reset(0);
        self.pr_file_list_state.reset(0);
        self.active_diff_request = None;
        self.diff_loading = false;
        self.diff_error = None;
        self.list_error = None;
        self.detail_error = None;
        self.list_loading = false;
        self.detail_loading = false;
        self.issue_limit = PAGE_SIZE;
        self.pr_limit = PAGE_SIZE;
        self.issue_has_more = false;
        self.pr_has_more = false;
        self.active_list_request = None;
        self.active_detail_request = None;
        self.pr_comment_input_key = None;
        self.pr_reply_input_key = None;
        self.issue_comment_draft.clear();
        self.pr_review_draft.clear();
        self.issue_list_state.reset(0);
        self.pr_list_state.reset(0);
        self.detail_body
            .update(cx, |body, cx| body.set_text("", cx));
        self.reset_pr_diff_body(cx);
        self.query_revision = self.query_revision.saturating_add(1);
        cx.notify();
    }



    fn schedule_query(&mut self, _query: String, cx: &mut Context<Self>) {
        self.list_cancelled.store(true, Ordering::Relaxed);
        self.debounce_task.take();
        self.issue_limit = PAGE_SIZE;
        self.pr_limit = PAGE_SIZE;
        self.active_list_request = None;
        self.list_loading = !self.scope_targets(cx).is_empty();
        self.list_error = None;
        let revision = self.query_revision;
        self.debounce_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(750))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.query_revision == revision {
                    this.fetch_list(cx);
                }
            });
        }));
        cx.notify();
    }

    fn query(&self, cx: &App) -> String {
        self.query_input.read(cx).value().trim().to_string()
    }

    fn clear_selection(&mut self) {
        match self.tab {
            GitHubTab::Issues => self.selected_issue = None,
            GitHubTab::PullRequests => self.selected_pr = None,
        }
    }

    fn invalidate_detail(&mut self, cx: &mut Context<Self>) {
        self.active_detail_request = None;
        self.detail_loading = false;
        self.detail_error = None;
        self.issue_detail = None;
        self.pr_detail = None;
        self.comment_rows.clear();
        self.comment_list_state.reset(0);
        self.pr_timeline_rows.clear();
        self.pr_timeline_list_state.reset(0);
        self.pr_file_list_state.reset(0);
        self.active_diff_request = None;
        self.diff_loading = false;
        self.diff_error = None;
        self.detail_body
            .update(cx, |body, cx| body.set_text("", cx));
        self.reset_pr_diff_body(cx);
    }

    fn reset_pr_diff_body(&mut self, cx: &mut Context<Self>) {
        self.pr_diff_body = cx.new(|cx| TextViewState::markdown("", cx));
    }

    fn fetch_list(&mut self, cx: &mut Context<Self>) {
        self.list_cancelled.store(true, Ordering::Relaxed);
        self.list_cancelled = Arc::new(AtomicBool::new(false));
        let cancelled = self.list_cancelled.clone();
        let targets = self.scope_targets(cx);
        self.last_targets = targets.iter().map(|(_, dir)| dir.clone()).collect();
        if targets.is_empty() {
            self.list_loading = false;
            self.list_error = None;
            cx.notify();
            return;
        }
        let tab = self.tab;
        let state = self.state_filter.value().to_owned();
        let query = self.query(cx);
        let limit = match tab {
            GitHubTab::Issues => self.issue_limit,
            GitHubTab::PullRequests => self.pr_limit,
        };
        let scope = self.scope.clone();
        let request = GitHubRequest {
            scope: scope.clone(),
            tab,
            query_revision: self.query_revision,
            item: None,
        };
        self.invalidate_detail(cx);
        self.active_list_request = Some(request.clone());
        self.list_loading = true;
        self.list_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result =
                cx.background_executor()
                    .spawn(async move {
                        let server_query = github_server_query(&query);
                        // Multiple attached checkouts can refer to one GitHub repository.
                        // Coalesce before issuing any list request, retaining the first
                        // checkout as the path used for subsequent item actions.
                        let mut seen = std::collections::HashSet::new();
                        let targets: Vec<_> = targets
                            .into_iter()
                            .filter(|(_, dir)| {
                                seen.insert(threadlane_git::github_list_repository_key(dir))
                            })
                            .collect();
                        match tab {
                            GitHubTab::Issues => {
                                let mut rows = Vec::new();
                                let mut errors = Vec::new();
                                let mut has_more = false;
                                for (project_name, work_dir) in &targets {
                                    if cancelled.load(Ordering::Relaxed) {
                                        break;
                                    }
                                    match threadlane_git::list_github_issues(
                                        work_dir,
                                        &state,
                                        server_query,
                                        limit,
                                    ) {
                                        Ok(summaries) => {
                                            has_more = has_more || summaries.len() == limit;
                                            rows.extend(summaries.into_iter().map(|summary| {
                                                ScopedIssue {
                                                    project: work_dir.clone(),
                                                    project_name: project_name.clone(),
                                                    summary,
                                                }
                                            }));
                                        }
                                        Err(error) => errors
                                            .push(format!("{}: {}", project_name, error.message)),
                                    }
                                }
                                GitHubListResult::Issues(ScopedIssueList {
                                    rows: merge_scoped_issues(rows, limit),
                                    errors,
                                    has_more,
                                })
                            }
                            GitHubTab::PullRequests => {
                                let mut rows = Vec::new();
                                let mut errors = Vec::new();
                                let mut has_more = false;
                                for (project_name, work_dir) in &targets {
                                    if cancelled.load(Ordering::Relaxed) {
                                        break;
                                    }
                                    match threadlane_git::list_github_pull_requests(
                                        work_dir,
                                        &state,
                                        server_query,
                                        limit,
                                    ) {
                                        Ok(summaries) => {
                                            has_more = has_more || summaries.len() == limit;
                                            rows.extend(summaries.into_iter().map(|summary| {
                                                ScopedPr {
                                                    project: work_dir.clone(),
                                                    project_name: project_name.clone(),
                                                    summary,
                                                }
                                            }));
                                        }
                                        Err(error) => errors
                                            .push(format!("{}: {}", project_name, error.message)),
                                    }
                                }
                                GitHubListResult::PullRequests(ScopedPrList {
                                    rows: merge_scoped_prs(rows, limit),
                                    errors,
                                    has_more,
                                })
                            }
                        }
                    })
                    .await;
            let _ = this.update(cx, |this, cx| {
                if !this
                    .active_list_request
                    .as_ref()
                    .is_some_and(|current| github_result_matches_request(&request, current))
                {
                    return;
                }
                this.list_loading = false;
                match result {
                    GitHubListResult::Issues(list) => {
                        let previous_selected = this.selected_issue.clone();
                        let old_count = this.issues.len() + usize::from(this.issue_has_more);
                        this.issue_has_more = list.has_more;
                        this.repository = list.rows.first().map(|row| GitHubRepository {
                            host: row.summary.issue.host.clone(),
                            owner: row.summary.issue.owner.clone(),
                            repo: row.summary.issue.repo.clone(),
                        });
                        this.issues = list.rows;
                        this.selected_issue = selected_scoped_issue_after_refresh(
                            this.selected_issue.clone(),
                            &this.issues,
                        );
                        this.list_error = scoped_list_error(&list.errors);
                        let new_count = this.issues.len() + usize::from(this.issue_has_more);
                        reconcile_list_count(&this.issue_list_state, old_count, new_count);
                        if previous_selected != this.selected_issue {
                            if let Some(ix) = this.selected_ix() {
                                this.issue_list_state.scroll_to_reveal_item(ix);
                            }
                        }
                        this.fetch_detail(cx);
                    }
                    GitHubListResult::PullRequests(list) => {
                        let previous_selected = this.selected_pr.clone();
                        let old_count = this.pull_requests.len() + usize::from(this.pr_has_more);
                        this.pr_has_more = list.has_more;
                        this.repository =
                            list.rows.first().map(|row| row.summary.repository.clone());
                        this.pull_requests = list.rows;
                        this.selected_pr = selected_scoped_pr_after_refresh(
                            this.selected_pr.clone(),
                            &this.pull_requests,
                        );
                        this.list_error = scoped_list_error(&list.errors);
                        let new_count = this.pull_requests.len() + usize::from(this.pr_has_more);
                        reconcile_list_count(&this.pr_list_state, old_count, new_count);
                        if previous_selected != this.selected_pr {
                            if let Some(ix) = this.selected_ix() {
                                this.pr_list_state.scroll_to_reveal_item(ix);
                            }
                        }
                        this.fetch_detail(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn fetch_detail(&mut self, cx: &mut Context<Self>) {
        let selected = match self.tab {
            GitHubTab::Issues => self.selected_issue.clone(),
            GitHubTab::PullRequests => self.selected_pr.clone(),
        };
        let Some(selected) = selected else {
            self.detail_loading = false;
            return;
        };
        let tab = self.tab;
        let scope = self.scope.clone();
        let request = GitHubRequest {
            scope,
            tab,
            query_revision: self.query_revision,
            item: Some(selected.clone()),
        };
        self.active_detail_request = Some(request.clone());
        self.detail_loading = true;
        self.detail_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match tab {
                        GitHubTab::Issues => GitHubDetailResult::Issue(
                            threadlane_git::inspect_github_issue(
                                &selected.project,
                                selected.number,
                            )
                            .map_err(|error| error.message),
                        ),
                        GitHubTab::PullRequests => GitHubDetailResult::PullRequest(
                            threadlane_git::inspect_pr_number(&selected.project, selected.number)
                                .map_err(|error| error.message),
                        ),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let detail_matches = this
                    .active_detail_request
                    .as_ref()
                    .is_some_and(|current| github_result_matches_request(&request, current));
                let list_matches = this.active_list_request.as_ref().is_some_and(|list| {
                    detail_result_matches_list(&request, list, this.selected_key())
                });
                if !detail_matches || !list_matches {
                    return;
                }
                this.detail_loading = false;
                match result {
                    GitHubDetailResult::Issue(Ok(detail)) => {
                        this.comment_rows = detail
                            .comments
                            .iter()
                            .map(|comment| {
                                (
                                    comment.author.clone(),
                                    comment.created_at.clone(),
                                    comment.body.clone(),
                                )
                            })
                            .collect();
                        this.comment_list_state.reset(this.comment_rows.len());
                        this.detail_body
                            .update(cx, |body, cx| body.set_text(&detail.body, cx));
                        this.issue_detail = Some(detail);
                    }
                    GitHubDetailResult::PullRequest(Ok(detail)) => {
                        let key = PrWorkspaceKey {
                            project: request
                                .item
                                .as_ref()
                                .map(|item| item.project.clone())
                                .unwrap_or_default(),
                            number: detail.number,
                        };
                        let previous_file =
                            this.pr_selections.selected_file(&key).map(str::to_owned);
                        this.pr_timeline_rows = merge_pr_timeline(&detail);
                        this.pr_timeline_list_state.reset(detail.commits.len());
                        this.pr_selections.reconcile_files(&key, &detail.files);
                        if previous_file.as_deref() != this.pr_selections.selected_file(&key) {
                            this.reset_pr_diff_body(cx);
                        }
                        this.pr_file_list_state.reset(detail.files.len());
                        this.detail_body
                            .update(cx, |body, cx| body.set_text(&detail.body, cx));
                        this.pr_viewed.observe_head(&key, &detail.head_oid);
                        this.pr_detail = Some(detail);
                        if this.pr_selections.tab(&key) == PrDetailTab::Code {
                            this.load_selected_diff(cx);
                            this.refresh_pr_viewed(cx);
                        }
                    }
                    GitHubDetailResult::Issue(Err(error))
                    | GitHubDetailResult::PullRequest(Err(error)) => {
                        this.detail_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_tab(&mut self, tab: GitHubTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.state_filter = github_state_for_tab(self.state_filter, tab);
        if self.model.read(cx).github_tab != tab {
            self.model.update(cx, |state, cx| {
                state.github_tab = tab;
                cx.notify();
            });
        }
    }

    fn select_tab(&mut self, tab: GitHubTab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.set_tab(tab, cx);
        self.query_revision = self.query_revision.saturating_add(1);
        self.clear_selection();
        self.fetch_list(cx);
    }

    fn select_state(&mut self, state: GitHubStateFilter, cx: &mut Context<Self>) {
        if self.state_filter == state {
            return;
        }
        self.state_filter = state;
        self.query_revision = self.query_revision.saturating_add(1);
        self.clear_selection();
        self.fetch_list(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.list_loading {
            return;
        }
        self.debounce_task.take();
        self.query_revision = self.query_revision.saturating_add(1);
        for (_, work_dir) in self.scope_targets(cx) {
            threadlane_git::invalidate_github_list_cache(&work_dir);
        }
        if let Some(selected) = self.selected_key() {
            threadlane_git::invalidate_github_detail_cache(&selected.project, selected.number);
        }
        self.fetch_list(cx);
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.list_loading {
            return;
        }
        match self.tab {
            GitHubTab::Issues => self.issue_limit += PAGE_SIZE,
            GitHubTab::PullRequests => self.pr_limit += PAGE_SIZE,
        }
        self.query_revision = self.query_revision.saturating_add(1);
        self.fetch_list(cx);
    }

    fn selected_ix(&self) -> Option<usize> {
        match self.tab {
            GitHubTab::Issues => self.selected_issue.as_ref().and_then(|selected| {
                self.issues.iter().position(|row| {
                    row.project == selected.project && row.summary.issue.number == selected.number
                })
            }),
            GitHubTab::PullRequests => self.selected_pr.as_ref().and_then(|selected| {
                self.pull_requests.iter().position(|row| {
                    row.project == selected.project && row.summary.number == selected.number
                })
            }),
        }
    }



    fn selected_key(&self) -> Option<GitHubItemKey> {
        match self.tab {
            GitHubTab::Issues => self.selected_issue.clone(),
            GitHubTab::PullRequests => self.selected_pr.clone(),
        }
    }

    fn current_pr_key(&self) -> Option<PrWorkspaceKey> {
        if self.tab != GitHubTab::PullRequests {
            return None;
        }
        let selected = self.selected_pr.clone()?;
        Some(PrWorkspaceKey {
            project: selected.project,
            number: selected.number,
        })
    }

    fn current_pr_tab(&self) -> PrDetailTab {
        self.current_pr_key()
            .map(|key| self.pr_selections.tab(&key))
            .unwrap_or_default()
    }

    fn current_pr_file(&self) -> Option<&str> {
        let key = self.current_pr_key()?;
        self.pr_selections.selected_file(&key)
    }

    fn current_pr_draft_value(&self) -> (Option<PrWorkspaceKey>, String) {
        let key = self.current_pr_key();
        let body = key
            .as_ref()
            .and_then(|key| self.pr_drafts.get(key))
            .map(|draft| draft.body.clone())
            .unwrap_or_default();
        (key, body)
    }

    fn current_pr_reply_draft_value(&self) -> (Option<(PrWorkspaceKey, String)>, String) {
        let key = self.current_pr_key();
        let reply = key
            .as_ref()
            .and_then(|key| self.pr_drafts.get(key))
            .and_then(|draft| draft.reply.as_ref());
        (
            key.zip(reply.map(|reply| reply.target.remote_id.clone())),
            reply.map(|reply| reply.body.clone()).unwrap_or_default(),
        )
    }

    fn pr_draft_inputs_match(&self, cx: &App) -> bool {
        let (key, body) = self.current_pr_draft_value();
        let (reply_key, reply_body) = self.current_pr_reply_draft_value();
        self.pr_comment_input_key == key
            && self.pr_comment_input.read(cx).value().as_str() == body
            && self.pr_reply_input_key == reply_key
            && self.pr_reply_input.read(cx).value().as_str() == reply_body
    }

    fn sync_pr_draft_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (key, body) = self.current_pr_draft_value();
        self.pr_comment_input_key = key;
        if self.pr_comment_input.read(cx).value().as_str() != body {
            self.pr_comment_input
                .update(cx, |input, cx| input.set_value(&body, window, cx));
        }
        let (reply_key, reply_body) = self.current_pr_reply_draft_value();
        self.pr_reply_input_key = reply_key;
        if self.pr_reply_input.read(cx).value().as_str() != reply_body {
            self.pr_reply_input
                .update(cx, |input, cx| input.set_value(&reply_body, window, cx));
        }
    }

    fn clear_pr_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        self.pr_drafts.clear(&key);
        self.sync_pr_draft_inputs(window, cx);
        cx.notify();
    }

    fn select_pr_reply_target(
        &mut self,
        target: PrReplyTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        self.pr_drafts.select_reply_target(key, target);
        self.sync_pr_draft_inputs(window, cx);
        kit_github::focus_github_reply(
            &self.pr_reply_input,
            &self.pr_conversation_scroll,
            window,
            cx,
        );
        cx.notify();
    }

    fn clear_pr_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        self.pr_drafts.clear_reply(&key);
        self.sync_pr_draft_inputs(window, cx);
        cx.notify();
    }

    fn publish_pr_comment(&mut self, cx: &mut Context<Self>) {
        self.publish_pr_conversation(false, cx);
    }

    fn publish_pr_reply(&mut self, cx: &mut Context<Self>) {
        self.publish_pr_conversation(true, cx);
    }

    fn publish_pr_conversation(&mut self, reply: bool, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        let Some(detail) = self
            .pr_detail
            .as_ref()
            .filter(|detail| detail.number == key.number)
        else {
            return;
        };
        let ids = if reply {
            detail
                .review_comments
                .iter()
                .map(|comment| comment.remote_id.clone())
                .collect()
        } else {
            detail
                .issue_comments
                .iter()
                .map(|comment| comment.remote_id.clone())
                .collect()
        };
        let attempt = if reply {
            self.pr_drafts
                .begin_reply(&key, detail.url.clone(), ids)
                .ok()
                .flatten()
        } else {
            self.pr_drafts.begin(&key, detail.url.clone(), ids)
        };
        let Some(attempt) = attempt else {
            return;
        };
        cx.notify();

        cx.spawn(async move |this, cx| {
            let project = attempt.key.project.clone();
            let number = attempt.key.number;
            let body = attempt.body.clone();
            let target = attempt.target.clone();
            let pr_url = attempt.pr_url.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    match target {
                        PrCommentTarget::PullRequest => {
                            threadlane_git::comment_on_pull_request(&project, number, &body)
                        }
                        PrCommentTarget::Reply(_, comment_id) => {
                            threadlane_git::reply_to_pull_request_review_comment(
                                &project, &pr_url, comment_id, &body,
                            )
                        }
                    }
                    .map_err(|error| error.message)
                })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => {
                    if this.pr_drafts.complete_success(&attempt)
                        && pr_publish_refresh_matches_selection(
                            &attempt,
                            this.current_pr_key().as_ref(),
                        )
                    {
                        this.fetch_detail(cx);
                    }
                    cx.notify();
                }
                Err(error) => {
                    if this.pr_drafts.mark_checking(&attempt, error.clone()) {
                        cx.notify();
                        this.check_pr_comment_attempt(attempt.clone(), error, cx);
                    }
                }
            });
        })
        .detach();
    }

    fn check_pr_comment_attempt(
        &mut self,
        attempt: PrCommentAttempt,
        write_error: String,
        cx: &mut Context<Self>,
    ) {
        let project = attempt.key.project.clone();
        let number = attempt.key.number;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    threadlane_git::inspect_pr_number(&project, number)
                        .map_err(|error| error.message)
                })
                .await;
            let outcome = classify_pr_readback(&attempt, result.as_ref().map_err(String::as_str));
            let error = result
                .as_ref()
                .err()
                .map(|check_error| format!("{write_error} GitHub check failed: {check_error}"))
                .unwrap_or(write_error);
            let _ = this.update(cx, |this, cx| {
                if !this.pr_drafts.complete_readback(&attempt, outcome, error) {
                    return;
                }
                if let Ok(detail) = result {
                    if pr_publish_refresh_matches_selection(
                        &attempt,
                        this.current_pr_key().as_ref(),
                    ) {
                        this.pr_timeline_rows = merge_pr_timeline(&detail);
                        this.pr_timeline_list_state.reset(detail.commits.len());
                        this.pr_detail = Some(detail);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn check_pr_comment_again(&mut self, cx: &mut Context<Self>) {
        self.check_pr_conversation_again(false, cx);
    }

    fn check_pr_reply_again(&mut self, cx: &mut Context<Self>) {
        self.check_pr_conversation_again(true, cx);
    }

    fn check_pr_conversation_again(&mut self, reply: bool, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        let write_error = self
            .pr_drafts
            .get(&key)
            .and_then(|draft| {
                if reply {
                    draft.reply.as_ref()?.publish.error.clone()
                } else {
                    draft.publish.error.clone()
                }
            })
            .unwrap_or_else(|| "GitHub did not confirm the earlier write.".into());
        let attempt = if reply {
            self.pr_drafts.begin_reply_recheck(&key)
        } else {
            self.pr_drafts.begin_recheck(&key)
        };
        let Some(attempt) = attempt else {
            return;
        };
        cx.notify();
        self.check_pr_comment_attempt(attempt, write_error, cx);
    }

    fn select_pr_tab(&mut self, tab: PrDetailTab, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        self.pr_selections.select_tab(key, tab);
        if tab == PrDetailTab::Code {
            self.load_selected_diff(cx);
            self.refresh_pr_viewed(cx);
        }
        cx.notify();
    }

    fn select_previous_pr_tab(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_pr_tab(self.current_pr_tab().adjacent(-1), cx);
    }

    fn select_next_pr_tab(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.select_pr_tab(self.current_pr_tab().adjacent(1), cx);
    }

    fn select_pr_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        if self.pr_selections.select_file(key, path) {
            self.reset_pr_diff_body(cx);
        }
        self.load_selected_diff(cx);
        cx.notify();
    }

    fn selected_pr_file_ix(&self) -> Option<usize> {
        let selected = self.current_pr_file()?;
        self.pr_detail
            .as_ref()?
            .files
            .iter()
            .position(|file| file.path == selected)
    }

    fn apply_pr_file_action(&mut self, action: PrFileAction, cx: &mut Context<Self>) {
        let Some(files) = self.pr_detail.as_ref().map(|detail| &detail.files) else {
            return;
        };
        let Some(ix) = pr_file_action_ix(self.selected_pr_file_ix(), files.len(), action) else {
            return;
        };
        let path = files[ix].path.clone();
        self.pr_file_list_state.scroll_to_reveal_item(ix);
        self.select_pr_file(path, cx);
    }

    fn select_previous_pr_file(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_pr_file_action(PrFileAction::Previous, cx);
    }

    fn select_next_pr_file(
        &mut self,
        _: &SelectNext,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_pr_file_action(PrFileAction::Next, cx);
    }

    fn open_selected_pr_file(
        &mut self,
        _: &OpenSelected,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_pr_file_action(PrFileAction::Open, cx);
    }

    fn load_selected_diff(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        let Some(path) = self.pr_selections.selected_file(&key).map(str::to_owned) else {
            return;
        };
        self.diff_revision = self.diff_revision.saturating_add(1);
        let request = PrDiffRequest {
            key: key.clone(),
            path: path.clone(),
            revision: self.diff_revision,
        };
        self.active_diff_request = Some(request.clone());
        self.diff_loading = true;
        self.diff_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let work_dir = key.project.clone();
            let number = key.number;
            let selected_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    threadlane_git::pull_request_diff(&work_dir, number)
                        .map_err(|error| error.message)
                        .and_then(|raw| {
                            prepare_selected_diff(&raw, &selected_path).ok_or_else(|| {
                                format!("{selected_path} was not present in the pull request diff")
                            })
                        })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let selected_key = this.current_pr_key();
                let selected_path = selected_key
                    .as_ref()
                    .and_then(|key| this.pr_selections.selected_file(key));
                if !pr_diff_result_matches_request(
                    &request,
                    this.active_diff_request.as_ref(),
                    selected_key.as_ref(),
                    selected_path,
                ) {
                    return;
                }
                this.diff_loading = false;
                match result {
                    Ok(markdown) => {
                        this.pr_diff_body
                            .update(cx, |body, cx| body.set_text(&markdown, cx));
                    }
                    Err(error) => {
                        this.diff_error = Some(format!("Couldn’t load diff: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The loaded detail's URL when it belongs to the selected list row.
    /// Same-number PRs in other repositories share the number, so the
    /// URL check is what keeps one repo's detail out of another's writes.
    fn selected_pr_detail_url(&self, key: &PrWorkspaceKey) -> Option<String> {
        let detail = self.pr_detail.as_ref()?;
        let selected = self
            .pull_requests
            .iter()
            .find(|row| row.project == key.project && row.summary.number == key.number)?;
        (detail.number == key.number && detail.url == selected.summary.url)
            .then(|| detail.url.clone())
    }

    /// Reads the signed-in account's Viewed markers for the selected PR.
    /// Called on Code-tab entry, explicit Refresh (via the detail refetch),
    /// and after writes — never on selection or render.
    fn refresh_pr_viewed(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        let Some(url) = self.selected_pr_detail_url(&key) else {
            return;
        };
        self.refresh_pr_viewed_for(key, url, cx);
    }

    /// Readback for a specific PR — used after writes so the snapshot that
    /// just changed is the one re-read, even if selection moved on.
    fn refresh_pr_viewed_for(&mut self, key: PrWorkspaceKey, url: String, cx: &mut Context<Self>) {
        if !self.pr_viewed.refresh_allowed(&key) {
            return;
        }
        let token = self.pr_viewed.begin_read(&key);
        let transport = self.viewed_transport.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let work_dir = key.project.clone();
            let result = cx
                .background_executor()
                .spawn(async move { (transport.read)(work_dir, url) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let applied = match result {
                    Ok(state) => {
                        let snapshot = PrViewedSnapshot {
                            pull_request_id: state.pull_request_id,
                            head_oid: state.head_oid,
                            viewer: state.viewer,
                            files: state
                                .files
                                .into_iter()
                                .map(|file| (file.path, file.status))
                                .collect(),
                            complete: state.complete,
                        };
                        this.pr_viewed.complete_read(&key, token, snapshot)
                    }
                    Err(error) => this.pr_viewed.fail_read(&key, token, error),
                };
                if applied {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Writes one file's Viewed marker for the signed-in account. No dialog;
    /// the marker stays pending until the readback agrees.
    fn set_pr_file_viewed(&mut self, path: String, viewed: bool, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        let Some(url) = self.selected_pr_detail_url(&key) else {
            return;
        };
        let Some(pull_request_id) = self
            .pr_viewed
            .get(&key)
            .and_then(|state| state.snapshot.as_ref())
            .map(|snapshot| snapshot.pull_request_id.clone())
        else {
            return;
        };
        let Some(token) = self.pr_viewed.begin_write(&key, path.clone(), viewed) else {
            return;
        };
        let transport = self.viewed_transport.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let work_dir = key.project.clone();
            let write_url = url.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    (transport.write)(work_dir, write_url, pull_request_id, path, viewed)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let applied = this.pr_viewed.complete_write(&key, token, result.clone());
                if applied {
                    cx.notify();
                }
                if applied && result.is_ok() {
                    // A confirmed write is only displayed once the readback
                    // agrees; an unconfirmed write stays blocked until a
                    // refresh settles it. The readback targets the PR this
                    // write landed on, not whatever is now selected.
                    this.refresh_pr_viewed_for(key, url, cx);
                }
            });
        })
        .detach();
    }

    /// Selects and reveals the next file without a confirmed Viewed marker.
    /// Navigation never marks a file; the trigger keeps its focus.
    fn select_next_unviewed(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.current_pr_key() else {
            return;
        };
        let Some(detail) = self
            .pr_detail
            .as_ref()
            .filter(|detail| detail.number == key.number)
        else {
            return;
        };
        let snapshot = self
            .pr_viewed
            .get(&key)
            .and_then(|state| state.snapshot.as_ref());
        let Some(target) = next_unviewed_file(
            snapshot,
            &detail.files,
            self.pr_selections.selected_file(&key),
        ) else {
            return;
        };
        let path = target.to_owned();
        if let Some(ix) = detail.files.iter().position(|file| file.path == path) {
            self.pr_file_list_state.scroll_to_reveal_item(ix);
        }
        self.select_pr_file(path, cx);
    }

    fn linked_pr_task(
        &self,
        project_work_dir: &PathBuf,
        head_ref: &str,
        cx: &App,
    ) -> Option<SessionInfo> {
        self.model
            .read(cx)
            .linked_pr_session(project_work_dir, head_ref)
            .cloned()
    }

    fn select_ix(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.selected_ix() == Some(ix)
            && (self.detail_loading || self.selected_detail_is_loaded())
        {
            return;
        }
        match self.tab {
            GitHubTab::Issues => {
                let Some(row) = self.issues.get(ix) else {
                    return;
                };
                self.selected_issue = Some(GitHubItemKey {
                    project: row.project.clone(),
                    number: row.summary.issue.number,
                });
                self.issue_list_state.scroll_to_reveal_item(ix);
            }
            GitHubTab::PullRequests => {
                let Some(row) = self.pull_requests.get(ix) else {
                    return;
                };
                self.selected_pr = Some(GitHubItemKey {
                    project: row.project.clone(),
                    number: row.summary.number,
                });
                self.pr_list_state.scroll_to_reveal_item(ix);
            }
        }
        self.fetch_detail(cx);
        cx.notify();
    }

    /// Runs an issue mutation on the background executor, then refreshes
    /// the list and detail. `describe` names the action for status feedback.
    fn run_issue_mutation(
        &mut self,
        _number: u64,
        describe: String,
        action: impl FnOnce(PathBuf) -> Result<String, threadlane_git::GitError> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.issue_action_pending {
            return;
        }
        let Some(project) = self
            .selected_issue
            .as_ref()
            .map(|key| key.project.clone())
            .or_else(|| self.project_work_dir.clone())
        else {
            self.issue_action_status = Some("Select an issue first.".into());
            cx.notify();
            return;
        };
        self.issue_action_pending = true;
        self.issue_action_status = Some(format!("{describe}…"));
        cx.notify();
        let view = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { action(project).map_err(|error| error.message) })
                .await;
            let _ = view.update(cx, |this, cx| {
                this.issue_action_pending = false;
                match outcome {
                    Ok(note) => {
                        this.issue_action_status = Some(note);
                        this.fetch_list(cx);
                        this.fetch_detail(cx);
                    }
                    Err(error) => {
                        this.issue_action_status = Some(format!("{describe} failed: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close_or_reopen_selected_issue(&mut self, close: bool, cx: &mut Context<Self>) {
        let Some(number) = self.selected_issue.as_ref().map(|key| key.number) else {
            return;
        };
        let describe = if close { "Closing issue" } else { "Reopening issue" }.to_string();
        let note = if close {
            format!("Closed issue #{number}.")
        } else {
            format!("Reopened issue #{number}.")
        };
        self.run_issue_mutation(
            number,
            describe,
            move |project| {
                threadlane_git::set_github_issue_state(&project, number, close)?;
                Ok(note)
            },
            cx,
        );
    }

    fn delete_selected_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.issue_action_pending {
            return;
        }
        let Some(key) = self.selected_issue.clone() else {
            return;
        };
        let Some(row) = self
            .issues
            .iter()
            .find(|row| row.project == key.project && row.summary.issue.number == key.number)
        else {
            return;
        };
        let identity = format!(
            "{}/{} · #{} · {}",
            row.summary.issue.owner, row.summary.issue.repo, key.number, row.project_name
        );
        let title = row.summary.title.clone();
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let key = key.clone();
            kit_github::github_issue_delete_dialog(
                alert,
                identity.clone(),
                title.clone(),
                move |_, cx| {
                    view.update(cx, |this, cx| {
                        if this.issue_action_pending {
                            this.issue_action_status = Some("Another issue action is still running. Wait for it to finish, then open the delete confirmation again.".into());
                            cx.notify();
                            return true;
                        }
                        if this.selected_issue.as_ref() != Some(&key) {
                            this.issue_action_status = Some(
                                "Issue selection changed. Open the delete confirmation again."
                                    .into(),
                            );
                            cx.notify();
                            return true;
                        }
                        let number = key.number;
                        this.run_issue_mutation(
                            number,
                            "Deleting issue".into(),
                            move |project| {
                                threadlane_git::delete_github_issue(&project, number)?;
                                Ok(format!("Deleted issue #{number}."))
                            },
                            cx,
                        );
                        true
                    })
                    .unwrap_or(true)
                },
            )
        });
    }

    fn suggest_issue_labels(&mut self, cx: &mut Context<Self>) {
        let (Some(key), Some(detail)) = (self.selected_issue.clone(), self.issue_detail.clone())
        else {
            return;
        };
        if detail.summary.issue.number != key.number {
            return;
        }
        let number = key.number;
        let title = detail.summary.title.clone();
        let body = detail.body.clone();
        let model = self.model.read(cx).selected_model.clone();
        if model.is_empty() {
            self.issue_action_status = Some("Select a model in chat before suggesting labels.".into());
            cx.notify();
            return;
        }
        let (api_key, account_id) =
            threadlane_coding_agent::credentials::provider_credentials(&model);
        let Ok(executor) = threadlane_ui_state::chat::executor() else {
            self.issue_action_status = Some("Unable to start the model runtime.".into());
            cx.notify();
            return;
        };
        self.issue_action_pending = true;
        self.issue_action_status = Some("Suggesting labels…".into());
        cx.notify();
        let tx = self.issue_action_tx.clone();
        executor.spawn(async move {
            let outcome = async {
                let labels = threadlane_git::list_github_labels(&key.project)
                    .map_err(|error| error.message)?;
                let available: Vec<String> =
                    labels.iter().map(|label| label.name.clone()).collect();
                let picked =
                    threadlane_coding_agent::credentials::provider_client_for(api_key, account_id)
                        .generate_issue_labels(&model, &title, &body, &available)
                        .await?;
                threadlane_git::edit_github_issue_labels(&key.project, number, &picked, &[])
                    .map_err(|error| error.message)?;
                Ok(format!("Applied labels: {}.", picked.join(", ")))
            }
            .await;
            let _ = tx.send(IssueActionEvent::Finished(outcome));
        });
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {        let len = match self.tab {
            GitHubTab::Issues => self.issues.len(),
            GitHubTab::PullRequests => self.pull_requests.len(),
        };
        if len == 0 {
            return;
        }
        let current = self.selected_ix().unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(len - 1);
        self.select_ix(next, cx);
    }

    fn select_previous(
        &mut self,
        _: &SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(-1, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn selected_detail_is_loaded(&self) -> bool {
        self.detail_error.is_none()
            && match self.tab {
                GitHubTab::Issues => self.issue_detail.as_ref().is_some_and(|detail| {
                    self.selected_issue
                        .as_ref()
                        .is_some_and(|selected| selected.number == detail.summary.issue.number)
                }),
                GitHubTab::PullRequests => self.pr_detail.as_ref().is_some_and(|detail| {
                    self.selected_pr
                        .as_ref()
                        .is_some_and(|selected| selected.number == detail.number)
                }),
            }
    }

    fn open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == GitHubTab::PullRequests && self.selected_detail_is_loaded() {
            self.pr_tabs_focus.focus(window, cx);
            cx.notify();
            return;
        }
        if let Some(ix) = self.selected_ix() {
            self.select_ix(ix, cx);
        }
    }

    fn linked_sessions(&self, issue: &GitHubIssueRef, cx: &App) -> Vec<LinkedSession> {
        let state = self.model.read(cx);
        let projects = state
            .projects
            .iter()
            .map(|project| (project.name.as_str(), project.sessions.as_slice()))
            .collect::<Vec<_>>();
        linked_sessions_across_projects(&projects, issue)
            .into_iter()
            .map(|(project_name, session)| LinkedSession {
                project_name: project_name.to_owned(),
                status: linked_session_status(
                    session,
                    state.pending_permissions.contains_key(&session.id),
                    state.session_is_generating(&session.session_file),
                ),
                branch: session.git_branch.clone(),
                pr_number: session.git_branch.as_ref().and_then(|branch| {
                    state
                        .git_prs
                        .get(&(session.work_dir.clone(), branch.clone()))
                        .and_then(|pr| pr.as_ref())
                        .map(|pr| pr.number)
                }),
                session: session.clone(),
            })
            .collect()
    }

    pub fn set_window_controls_inset(
        &mut self,
        inset: Option<Pixels>,
        cx: &mut Context<Self>,
    ) {
        // The workspace forwards its layout inset every frame; only a change redraws.
        if self.window_controls_inset != inset {
            self.window_controls_inset = inset;
            cx.notify();
        }
    }

    fn apply_list_action(
        &mut self,
        action: kit_github::GitHubListAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use kit_github::GitHubListAction as Action;
        match action {
            Action::State(state) => self.select_state(state, cx),
            Action::Refresh => self.refresh(cx),
            Action::LoadMore => self.load_more(cx),
            Action::NewIssue => {
                let targets = self.scope_targets(cx);
                if targets.len() == 1 {
                    open_issue_create_dialog(self.model.clone(), targets[0].1.clone(), window, cx);
                }
            }
            Action::Settings => self.model.update(cx, |state, cx| {
                controller::dispatch(state, AppAction::OpenSettings);
                cx.notify();
            }),
            Action::AttachProject => {
                let model = self.model.clone();
                cx.spawn(async move |_, cx| {
                    let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                        return;
                    };
                    let _ = model.update(cx, |state, cx| {
                        controller::dispatch(
                            state,
                            AppAction::AttachProject(folder.path().to_path_buf()),
                        );
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let model = self.model.clone();
        kit_github::github_toolbar(
            self.tab.label(),
            self.window_controls_inset,
            move |_, _, cx| {
                model.update(cx, |state, cx| {
                    controller::dispatch(state, AppAction::CloseGitHub);
                    cx.notify();
                });
            },
            cx,
        )
        .when(
            self.tab == GitHubTab::PullRequests && self.current_pr_tab() == PrDetailTab::Code,
            |toolbar| {
                toolbar.child(kit_github::github_back_to_pr_list().on_click(cx.listener(
                    |view, _, window, cx| {
                        view.select_pr_tab(PrDetailTab::Summary, cx);
                        view.list_focus.focus(window, cx);
                    },
                )))
            },
        )
    }

    fn render_scope_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let projects = self.attached_projects(cx);
        let mut counts = std::collections::HashMap::<String, usize>::new();
        for (name, _) in &projects {
            *counts.entry(name.clone()).or_default() += 1;
        }
        let label = self.scope.label(&projects);
        let selected = match &self.scope {
            GitHubScope::All => None,
            GitHubScope::Project(dir) => Some(dir.clone()),
        };
        let mut choices = vec![(None, "All projects".into())];
        choices.extend(projects.into_iter().map(|(name, dir)| {
            let label = if counts.get(&name).is_some_and(|count| *count > 1) {
                let parent = dir
                    .parent()
                    .and_then(|parent| parent.file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or("…");
                format!("{name} · …/{parent}")
            } else {
                name
            };
            (Some(dir), label)
        }));
        let owner = cx.entity().downgrade();
        kit_github::github_scope_row(
            label,
            choices,
            selected,
            move |id, _, cx| {
                let scope = id.map(GitHubScope::Project).unwrap_or(GitHubScope::All);
                let _ = owner.update(cx, |view, cx| view.select_scope(scope, cx));
            },
            cx,
        )
        .into_any_element()
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        let targets = self.scope_targets(cx);
        let controls = kit_github::GitHubListControls::new(self.state_filter)
            .pull_requests(self.tab == GitHubTab::PullRequests)
            .loading(self.list_loading)
            .has_results(!self.issues.is_empty() || !self.pull_requests.is_empty())
            .has_scope(!targets.is_empty())
            .single_project(targets.len() == 1);
        let owner = cx.entity().downgrade();
        kit_github::github_filters(
            &self.query_input,
            &controls,
            move |action, window, cx| {
                let _ = owner.update(cx, |view, cx| view.apply_list_action(action, window, cx));
            },
            cx,
        )
        .into_any_element()
    }

    fn render_issue_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        if ix == self.issues.len() && self.issue_has_more {
            return self.render_load_more(cx);
        }
        let Some(row) = self.issues.get(ix).cloned() else {
            return div().into_any_element();
        };
        let issue = row.summary;
        let number = issue.issue.number;
        let selected = self
            .selected_issue
            .as_ref()
            .is_some_and(|key| key.project == row.project && key.number == number);
        let linked = self.linked_sessions(&issue.issue, cx).len();
        let mut metadata: Vec<_> = issue
            .labels
            .iter()
            .take(2)
            .map(|label| (label.name.clone(), false))
            .collect();
        if issue.labels.len() > 2 {
            metadata.push((format!("+{}", issue.labels.len() - 2), false));
        }
        if linked > 0 {
            metadata.push((
                format!("{linked} linked task{}", if linked == 1 { "" } else { "s" }),
                false,
            ));
        }
        let kind = if issue.state.eq_ignore_ascii_case("closed") {
            kit_github::GitHubRowKind::ClosedIssue
        } else {
            kit_github::GitHubRowKind::OpenIssue
        };
        let context = scope_context_line(&issue.issue.owner, &issue.issue.repo, &row.project_name);
        let snapshot = kit_github::GitHubListRow::new(
            format!("github-issue-{}-{number}", row.project.display()),
            kind,
            issue.title,
            format!("{context} · #{number}"),
            format!(
                "@{} · {}",
                issue.author,
                format_github_time(&issue.updated_at, github_now_unix())
            ),
        )
        .selected(selected)
        .metadata(metadata)
        .suffix((issue.comments_count > 0).then(|| {
            format!(
                "{} comment{}",
                issue.comments_count,
                if issue.comments_count == 1 { "" } else { "s" }
            )
        }));
        kit_github::github_list_row(snapshot, cx)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.list_focus.focus(window, cx);
                if let Some(ix) = this.issues.iter().position(|item| {
                    item.project == row.project && item.summary.issue.number == number
                }) {
                    this.select_ix(ix, cx);
                }
            }))
            .into_any_element()
    }

    fn render_pr_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        if ix == self.pull_requests.len() && self.pr_has_more {
            return self.render_load_more(cx);
        }
        let Some(row) = self.pull_requests.get(ix).cloned() else {
            return div().into_any_element();
        };
        let pr = row.summary;
        let number = pr.number;
        let selected = self
            .selected_pr
            .as_ref()
            .is_some_and(|key| key.project == row.project && key.number == number);
        let linked = self
            .linked_pr_task(&row.project, &pr.head_ref, cx)
            .is_some();
        let checks = pr_check_label(&pr.checks);
        let mut metadata = vec![(checks.to_string(), checks.contains("failing"))];
        if let Some(label) = pr.review_decision.as_deref().and_then(pr_review_label) {
            metadata.push((label.into(), false));
        }
        if linked {
            metadata.push(("Linked task".into(), false));
        }
        let context =
            scope_context_line(&pr.repository.owner, &pr.repository.repo, &row.project_name);
        let snapshot = kit_github::GitHubListRow::new(
            format!("github-pr-{}-{number}", row.project.display()),
            kit_github::GitHubRowKind::PullRequest,
            pr.title,
            format!("{context} · #{number}"),
            format!(
                "@{} · {}",
                pr.author,
                format_github_time(&pr.updated_at, github_now_unix())
            ),
        )
        .selected(selected)
        .metadata(metadata)
        .suffix(pr.is_draft.then(|| "Draft".into()));
        kit_github::github_list_row(snapshot, cx)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.list_focus.focus(window, cx);
                if let Some(ix) = this
                    .pull_requests
                    .iter()
                    .position(|item| item.project == row.project && item.summary.number == number)
                {
                    this.select_ix(ix, cx);
                }
            }))
            .into_any_element()
    }

    fn render_load_more(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                kit_github::github_load_more(self.list_loading)
                    .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
            )
            .into_any_element()
    }

    fn render_list(&mut self, _window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.scope_targets(cx).is_empty() {
            return self.render_no_project(cx);
        }
        let row_count = match self.tab {
            GitHubTab::Issues => self.issues.len(),
            GitHubTab::PullRequests => self.pull_requests.len(),
        };
        let has_more = match self.tab {
            GitHubTab::Issues => self.issue_has_more,
            GitHubTab::PullRequests => self.pr_has_more,
        };
        if row_count == 0 && !has_more && !self.list_loading {
            if let Some(error) = self.list_error.clone() {
                return self.render_error("list", &error, cx);
            }
            return self.render_empty(
                &github_empty_message(self.tab, self.state_filter, &self.query(cx)),
                cx,
            );
        }
        let list_state = match self.tab {
            GitHubTab::Issues => self.issue_list_state.clone(),
            GitHubTab::PullRequests => self.pr_list_state.clone(),
        };
        let tab = self.tab;
        let content = kit_github::github_list_surface(cx)
            .track_focus(&self.list_focus)
            .key_context(GITHUB_LIST_CONTEXT)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::open_selected))
            .child(
                list(
                    list_state.clone(),
                    cx.processor(move |this, ix, _window, cx| match tab {
                        GitHubTab::Issues => this.render_issue_row(ix, cx),
                        GitHubTab::PullRequests => this.render_pr_row(ix, cx),
                    }),
                )
                .size_full()
                .with_sizing_behavior(ListSizingBehavior::Infer),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(Scrollbar::vertical(&list_state)),
            )
            .into_any_element();
        if let Some(error) = self.list_error.clone() {
            // Partial failure (some projects loaded): a slim banner so the
            // list keeps its space. Full card only when nothing loaded.
            let banner = if row_count > 0 || has_more {
                self.render_list_warning(&error, cx)
            } else {
                self.render_error("list", &error, cx)
            };
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(banner)
                .child(div().flex_1().min_h_0().child(content))
                .into_any_element();
        }
        content
    }

    fn render_list_warning(&self, error: &str, cx: &mut Context<Self>) -> AnyElement {
        kit_github::github_list_warning(
            error.to_owned(),
            self.list_loading,
            cx.listener(|this, _, _, cx| this.refresh(cx)),
            cx,
        )
        .into_any_element()
    }

    fn render_error(&self, id: &'static str, error: &str, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.entity().downgrade();
        kit_github::github_error(
            id,
            github_error_message(error),
            error.to_owned(),
            self.list_loading,
            move |action, window, cx| {
                let _ = owner.update(cx, |view, cx| view.apply_list_action(action, window, cx));
            },
            cx,
        )
        .into_any_element()
    }

    fn render_empty(&self, message: &str, cx: &mut Context<Self>) -> AnyElement {
        kit_github::github_empty(message.to_owned(), self.list_loading, cx).into_any_element()
    }

    fn render_no_project(&self, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.entity().downgrade();
        kit_github::github_no_project(
            move |action, window, cx| {
                let _ = owner.update(cx, |view, cx| view.apply_list_action(action, window, cx));
            },
            cx,
        )
        .into_any_element()
    }

    fn render_comment_row(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some((author, time, body)) = self.comment_rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let remote_id = self
            .issue_detail
            .as_ref()
            .and_then(|detail| detail.comments.get(ix))
            .map(|comment| comment.remote_id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| {
                // Older snapshots can omit remote IDs; keep their content identity stable on reorder.
                let mut hash = DefaultHasher::new();
                (&author, &time, &body).hash(&mut hash);
                format!("legacy-{}", hash.finish())
            });
        let owner = self
            .selected_issue
            .as_ref()
            .map(|key| format!("{}-{}", key.project.display(), key.number))
            .unwrap_or_default();
        kit_github::github_comment(
            format!("github-issue-comment-{owner}-{remote_id}"),
            author,
            time,
            body,
            cx,
        )
        .into_any_element()
    }

    fn handoff_pr_reply(&self, head_ref: &str, prompt: String, cx: &mut Context<Self>) {
        let model = self.model.clone();
        let selected_project = self.selected_pr.clone().map(|key| key.project);
        let head_ref = head_ref.to_owned();
        model.update(cx, |state, cx| {
            let target = selected_project
                .as_ref()
                .and_then(|project| state.linked_pr_session(project, &head_ref))
                .map(|session| (session.work_dir.clone(), session.id.clone()));
            let Some((work_dir, session_id)) = target else {
                state.session_status =
                    Some("No active task is linked to this pull request branch.".into());
                cx.notify();
                return;
            };
            if state.active_work_dir.as_ref() != Some(&work_dir)
                || state.active_session_id.as_deref() != Some(session_id.as_str())
            {
                controller::dispatch(
                    state,
                    AppAction::SelectSession { work_dir, session_id },
                );
            }
            state.request_composer_prompt(prompt);
            controller::dispatch(state, AppAction::CloseGitHub);
            cx.notify();
        });
    }

    fn address_all_pr_reviews(&self, cx: &mut Context<Self>) {
        let Some(pr) = self.pr_detail.clone() else {
            return;
        };
        let head_ref = pr.head_ref.clone();
        let Some(work_dir) = self.selected_pr.as_ref().map(|key| key.project.clone()) else {
            return;
        };
        let model = self.model.clone();
        model.update(cx, |state, cx| {
            match state.address_pr_reviews_manual(work_dir, head_ref.clone(), &pr) {
                Ok(_) => {
                    controller::dispatch(state, AppAction::CloseGitHub);
                    state.session_status = Some("Addressing PR review feedback…".into());
                    cx.notify();
                }
                Err(error) => {
                    state.session_status = Some(error);
                    cx.notify();
                }
            }
        });
    }

    fn render_pr_comment_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // The reply variant degrades to None; a missing comment composer
        // renders nothing instead of panicking the paint.
        self.render_pr_conversation_editor(false, cx)
            .unwrap_or_else(|| div().into_any_element())
    }

    fn render_pr_reply_editor(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.render_pr_conversation_editor(true, cx)
    }

    fn render_pr_conversation_editor(
        &mut self,
        reply: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let draft = self
            .current_pr_key()
            .as_ref()
            .and_then(|key| self.pr_drafts.get(key))
            .cloned()
            .unwrap_or_default();
        let reply_draft = reply.then(|| draft.reply.clone()).flatten();
        if reply && reply_draft.is_none() {
            return None;
        }
        let (body, publish, target, blocked) = if let Some(reply) = reply_draft {
            (reply.body, reply.publish, Some(reply.target), reply.blocked)
        } else {
            (draft.body, draft.publish, None, false)
        };
        let control = pr_publish_control(&body, &publish);
        let invalid_target = target.as_ref().and_then(|target| target.comment_id().err());
        let status = match publish.phase {
            PrCommentPhase::Idle => None,
            PrCommentPhase::Publishing => Some("Publishing…"),
            PrCommentPhase::Checking => Some("Checking GitHub…"),
            PrCommentPhase::Present if reply => Some("A matching new review comment was found; this check did not establish its reply relationship. Draft retained."),
            PrCommentPhase::Present => Some("A matching new GitHub comment was found for the submitted draft. Draft retained."),
            PrCommentPhase::Absent => Some("No matching new GitHub comment was found for the submitted draft. Draft retained."),
            PrCommentPhase::Unknown => Some("GitHub could not confirm whether the submitted draft was published. Draft retained."),
        };
        let action_kind = match control {
            PrCommentControl::Post => kit_github::GitHubDraftAction::Post,
            PrCommentControl::Retry => kit_github::GitHubDraftAction::Retry,
            PrCommentControl::PostNewDraft => kit_github::GitHubDraftAction::PostNewDraft,
            PrCommentControl::CheckAgain => kit_github::GitHubDraftAction::CheckAgain,
            PrCommentControl::ClearDraft => kit_github::GitHubDraftAction::ClearDraft,
        };
        let disabled =
            matches!(
                control,
                PrCommentControl::Post | PrCommentControl::Retry | PrCommentControl::PostNewDraft
            ) && (publish.is_active() || body.trim().is_empty() || invalid_target.is_some());
        let action = kit_github::github_draft_action(
            reply,
            action_kind,
            disabled,
            cx.listener(move |this, _, window, cx| match control {
                PrCommentControl::Post
                | PrCommentControl::Retry
                | PrCommentControl::PostNewDraft => {
                    if reply {
                        this.publish_pr_reply(cx)
                    } else {
                        this.publish_pr_comment(cx)
                    }
                }
                PrCommentControl::CheckAgain => {
                    if reply {
                        this.check_pr_reply_again(cx)
                    } else {
                        this.check_pr_comment_again(cx)
                    }
                }
                PrCommentControl::ClearDraft => {
                    if reply {
                        this.clear_pr_reply(window, cx)
                    } else {
                        this.clear_pr_draft(window, cx)
                    }
                }
            }),
        )
        .into_any_element();
        let mut controls_before_editor = publish
            .attempt
            .as_ref()
            .filter(|_| publish.phase == PrCommentPhase::Present)
            .map(|attempt| pr_present_recovery_action(reply, attempt, cx).into_any_element())
            .into_iter()
            .collect::<Vec<_>>();
        let mut controls_after_editor = Vec::new();
        if matches!(
            control,
            PrCommentControl::ClearDraft | PrCommentControl::Retry | PrCommentControl::CheckAgain
        ) {
            controls_before_editor.push(action);
        } else {
            controls_after_editor.push(action);
        }
        let validation = blocked
            .then_some("Post or clear this reply draft before replying to another comment.")
            .into_iter()
            .chain(invalid_target)
            .map(str::to_owned)
            .collect();
        let mut editor = kit_github::GitHubCommentEditor::new(
            if reply {
                self.pr_reply_input.clone()
            } else {
                self.pr_comment_input.clone()
            },
            reply,
        )
        .controls(controls_before_editor, controls_after_editor)
        .validation(validation)
        .status(
            status.map(str::to_owned),
            publish.is_active(),
            matches!(
                publish.phase,
                PrCommentPhase::Absent | PrCommentPhase::Unknown
            ),
        )
        .error(publish.error);
        if let Some(target) = target {
            let location = target.path.map(|path| {
                format!(
                    "{path}{}",
                    target
                        .line
                        .map(|line| format!(":{line}"))
                        .unwrap_or_default()
                )
            });
            editor = editor.target(target.author, target.body, location);
        }
        Some(editor.render(cx).into_any_element())
    }

    fn render_pr_timeline_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.pr_timeline_rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let Some(pr) = self.pr_detail.as_ref() else {
            return div().into_any_element();
        };
        let prompt = draft_reply_prompt(&row);
        let head_ref = pr.head_ref.clone();
        let target = row.reply_target();
        let identity = if row.remote_id.is_empty() {
            let mut hash = DefaultHasher::new();
            (&row.author, &row.timestamp, &row.body, &row.path, row.line).hash(&mut hash);
            format!("{:x}", hash.finish())
        } else {
            row.remote_id.clone()
        };
        let entry = kit_github::GitHubConversationEntry::new(
            format!("{}-{}-{identity}", pr.url, row.kind.label()),
            row.label(),
            row.author.clone(),
            format_github_time(&row.timestamp, github_now_unix()),
            row.body.clone(),
        )
        .location(row.location())
        .review(row.kind == PrTimelineKind::Review)
        .reply_author(target.as_ref().map(|target| target.author.clone()));
        let owner = cx.entity().downgrade();
        kit_github::github_conversation_row(
            entry,
            move |action, window, cx| {
                let _ = owner.update(cx, |this, cx| match action {
                    kit_github::GitHubConversationAction::Reply => {
                        if let Some(target) = &target {
                            this.select_pr_reply_target(target.clone(), window, cx);
                        }
                    }
                    kit_github::GitHubConversationAction::AskAgent => {
                        this.handoff_pr_reply(&head_ref, prompt.clone(), cx)
                    }
                });
            },
            cx,
        )
        .into_any_element()
    }

    fn render_pr_file_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(file) = self
            .pr_detail
            .as_ref()
            .and_then(|detail| detail.files.get(ix))
        else {
            return Empty.into_any_element();
        };
        let selected = self.current_pr_file() == Some(file.path.as_str());
        let marker = self
            .current_pr_key()
            .and_then(|key| self.pr_viewed.get(&key))
            .and_then(|state| state.marker_label(&file.path));
        let path = file.path.clone();
        let owner = cx.entity().downgrade();
        kit_github::github_pr_file_row(
            file,
            selected,
            marker,
            move |_, window, cx| {
                let _ = owner.update(cx, |this, cx| {
                    this.pr_file_focus.focus(window, cx);
                    this.select_pr_file(path.clone(), cx);
                });
            },
            cx,
        )
        .into_any_element()
    }

    fn render_pr_summary(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(detail) = self.pr_detail.as_ref().filter(|detail| {
            self.selected_pr
                .as_ref()
                .is_some_and(|selected| selected.number == detail.number)
        }) else {
            return self.render_empty("Loading details…", cx);
        };
        kit_github::github_pr_summary(detail, &self.detail_body, cx).into_any_element()
    }

    fn render_pr_conversation(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let comments = (0..self.pr_timeline_rows.len())
            .map(|ix| self.render_pr_timeline_row(ix, cx))
            .collect();
        let editor = self.render_pr_comment_editor(cx);
        let reply = self.render_pr_reply_editor(cx);
        kit_github::github_pr_conversation(comments, editor, reply, &self.pr_conversation_scroll, cx)
            .into_any_element()
    }

    fn render_pr_commit_row(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(commit) = self
            .pr_detail
            .as_ref()
            .and_then(|detail| detail.commits.get(ix))
        else {
            return div().into_any_element();
        };
        kit_github::github_pr_commit_row(commit, cx).into_any_element()
    }

    fn render_pr_timeline(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let count = self
            .pr_detail
            .as_ref()
            .map(|detail| detail.commits.len())
            .unwrap_or_default();
        let list = (count > 0).then(|| {
            div()
                .relative()
                .size_full()
                .child(
                    list(
                        self.pr_timeline_list_state.clone(),
                        cx.processor(Self::render_pr_commit_row),
                    )
                    .size_full()
                    .with_sizing_behavior(ListSizingBehavior::Infer),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .child(Scrollbar::vertical(&self.pr_timeline_list_state)),
                )
                .into_any_element()
        });
        kit_github::github_pr_commits(count, list, cx).into_any_element()
    }

    fn render_pr_code(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let file_count = self
            .pr_detail
            .as_ref()
            .map(|detail| detail.files.len())
            .unwrap_or_default();
        if file_count == 0 {
            return self.render_empty("No changed files reported.", cx);
        }
        let key = self.current_pr_key();
        let listed_files = self
            .pr_detail
            .as_ref()
            .map(|detail| detail.files.as_slice())
            .unwrap_or(&[]);
        // The displayed detail may still belong to another same-numbered PR;
        // viewed markers are per-repo state, so they only show when the URL
        // proves the detail is the selected row's pull request.
        let detail_matches = key
            .as_ref()
            .is_some_and(|key| self.selected_pr_detail_url(key).is_some());
        let viewed_state = if detail_matches {
            key.as_ref().and_then(|key| self.pr_viewed.get(key))
        } else {
            None
        };
        let snapshot = viewed_state.and_then(|state| state.snapshot.as_ref());
        let loading = viewed_state.is_some_and(|state| state.loading);
        let (viewed_count, listed_count, all_viewed) = pr_viewed_progress(snapshot, listed_files);
        let progress_label = if (loading && snapshot.is_none()) || !detail_matches {
            "Loading viewed status…".to_owned()
        } else if all_viewed {
            "All listed files viewed".to_owned()
        } else {
            let mut label = format!("{viewed_count} of {listed_count} listed files viewed");
            if snapshot.is_some_and(|snap| !snap.complete) {
                label.push_str(" · status partial");
            }
            if loading {
                label.push_str(" · refreshing…");
            }
            label
        };
        let selected_path = key
            .as_ref()
            .and_then(|key| self.pr_selections.selected_file(key))
            .map(str::to_owned);
        let viewed_control = selected_path.clone().map(|path| {
            let pending = viewed_state
                .and_then(|state| state.pending_write.as_ref())
                .filter(|write| write.path == path);
            let checked = pending.map(|write| write.viewed).unwrap_or_else(|| {
                snapshot.map(|snap| snap.status(&path)) == Some(PrFileViewedStatus::Viewed)
            });
            let enabled = viewed_state.is_some_and(|state| state.write_allowed(&path))
                && !self.diff_loading
                && self.diff_error.is_none();
            let help: SharedString = if let Some(write) = pending {
                if write.uncertain {
                    format!("{path}: couldn't confirm — use Refresh to settle").into()
                } else {
                    format!("{path}: saving to GitHub…").into()
                }
            } else if enabled {
                format!("{path}: saved to GitHub for your signed-in account").into()
            } else if loading {
                format!("{path}: loading viewed status…").into()
            } else if self.diff_loading || self.diff_error.is_some() {
                format!("{path}: load the diff before marking viewed").into()
            } else {
                format!("{path}: viewed status unavailable — refresh to retry").into()
            };
            (checked, enabled, help)
        });
        let next_target = next_unviewed_file(snapshot, listed_files, selected_path.as_deref());
        let next_help = next_target.map(|_| "Select the next file without a viewed marker".into());
        let mut controls = kit_github::GitHubPrFilesControls::new(progress_label, next_help)
            .error(viewed_state.and_then(|state| state.error.clone()));
        if let Some((checked, enabled, help)) = viewed_control {
            controls = controls.viewed(checked, enabled, help);
        }
        let owner = cx.entity().downgrade();
        let toolbar = controls.render(
            move |action, _, cx| {
                let _ = owner.update(cx, |this, cx| match action {
                    kit_github::GitHubFileReviewAction::SetViewed(checked) => {
                        if let Some(path) = &selected_path {
                            this.set_pr_file_viewed(path.clone(), checked, cx);
                        }
                    }
                    kit_github::GitHubFileReviewAction::NextUnviewed => {
                        this.select_next_unviewed(cx)
                    }
                });
            },
            cx,
        );
        let files = kit_github::github_pr_file_list(&self.pr_file_focus, cx)
            .key_context(GITHUB_PR_FILE_LIST_CONTEXT)
            .on_action(cx.listener(Self::select_previous_pr_file))
            .on_action(cx.listener(Self::select_next_pr_file))
            .on_action(cx.listener(Self::open_selected_pr_file))
            .child(
                list(
                    self.pr_file_list_state.clone(),
                    cx.processor(|this, ix, _, cx| this.render_pr_file_row(ix, cx)),
                )
                .size_full()
                .with_sizing_behavior(ListSizingBehavior::Infer),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(Scrollbar::vertical(&self.pr_file_list_state)),
            );
        let owner = cx.entity().downgrade();
        let diff = kit_github::github_pr_diff(
            &self.pr_diff_body,
            self.diff_loading,
            self.diff_error.clone(),
            move |_, _, cx| {
                let _ = owner.update(cx, |this, cx| this.load_selected_diff(cx));
            },
            cx,
        );
        kit_github::github_pr_files(
            toolbar.into_any_element(),
            files.into_any_element(),
            diff.into_any_element(),
        )
        .into_any_element()
    }

    fn render_pr_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.pr_draft_inputs_match(cx) {
            cx.defer_in(window, |this, window, cx| {
                if !this.pr_draft_inputs_match(cx) {
                    this.sync_pr_draft_inputs(window, cx);
                }
            });
        }
        let (number, title, url, state, author, head_ref, base_ref, updated_at) = {
            let Some(detail) = self.pr_detail.as_ref().filter(|detail| {
                self.selected_pr
                    .as_ref()
                    .is_some_and(|selected| selected.number == detail.number)
            }) else {
                return self.render_empty("Loading details…", cx);
            };
            (
                detail.number,
                detail.title.clone(),
                detail.url.clone(),
                detail.state.clone(),
                detail.author.clone(),
                detail.head_ref.clone(),
                detail.base_ref.clone(),
                detail.updated_at.clone(),
            )
        };
        let repository_label = self
            .selected_pr
            .as_ref()
            .and_then(|selected| {
                self.pull_requests.iter().find(|row| {
                    row.project == selected.project && row.summary.number == selected.number
                })
            })
            .map(|row| {
                format!(
                    "{}/{}",
                    row.summary.repository.owner, row.summary.repository.repo
                )
            })
            .unwrap_or_default();
        let linked = self
            .selected_pr
            .as_ref()
            .and_then(|selected| self.linked_pr_task(&selected.project, &head_ref, cx));
        let has_actionable_reviews = self.pr_detail.as_ref().is_some_and(|pr| {
            pr.state.eq_ignore_ascii_case("open")
                && !threadlane_git::collect_actionable_pr_feedback(pr).is_empty()
        });
        let tab = self.current_pr_tab();
        let owner = cx.entity().downgrade();
        let model = self.model.clone();
        let linked_title = linked.as_ref().map(|session| session.title.clone());
        let actions = kit_github::github_pr_actions(
            url,
            linked_title,
            has_actionable_reviews,
            move |action, _, cx| match action {
                kit_github::GitHubDetailAction::OpenTask => {
                    if let Some(session) = &linked {
                        model.update(cx, |state, cx| {
                            controller::dispatch(
                                state,
                                AppAction::SelectSession {
                                    work_dir: session.work_dir.clone(),
                                    session_id: session.id.clone(),
                                },
                            );
                            controller::dispatch(state, AppAction::CloseGitHub);
                            cx.notify();
                        });
                    }
                }
                kit_github::GitHubDetailAction::AddressReviews => {
                    let _ = owner.update(cx, |view, cx| view.address_all_pr_reviews(cx));
                }
                _ => {}
            },
        );
        let owner = cx.entity().downgrade();
        let tabs_focus = self.pr_tabs_focus.clone();
        let tabs = div()
            .id("github-pr-detail-tabs-focus")
            .role(Role::TabList)
            .track_focus(&self.pr_tabs_focus)
            .key_context(GITHUB_PR_TABS_CONTEXT)
            .on_action(cx.listener(Self::select_previous_pr_tab))
            .on_action(cx.listener(Self::select_next_pr_tab))
            .child(kit_github::github_pr_tabs(tab, move |tab, window, cx| {
                tabs_focus.focus(window, cx);
                let _ = owner.update(cx, |view, cx| view.select_pr_tab(tab, cx));
            }));
        let header = kit_github::GitHubDetailHeader::new(
            title,
            format!(
                "{repository_label} · #{number} · {state} · @{author} · {}",
                format_github_time(&updated_at, github_now_unix())
            ),
            actions.into_any_element(),
        )
        .branch(format!("{head_ref} → {base_ref}"))
        .tabs(tabs.into_any_element())
        .render(cx);
        let body = match tab {
            PrDetailTab::Summary => self.render_pr_summary(cx),
            PrDetailTab::Conversation => self.render_pr_conversation(cx),
            PrDetailTab::Timeline => self.render_pr_timeline(cx),
            PrDetailTab::Code => self.render_pr_code(cx),
        };
        kit_github::github_detail_surface(header, body).into_any_element()
    }

    fn render_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.selected_ix().is_none() {
            let nothing_loaded = match self.tab {
                GitHubTab::Issues => self.issues.is_empty(),
                GitHubTab::PullRequests => self.pull_requests.is_empty(),
            };
            // A failed, empty list offers nothing to select; don't ask for it.
            let message = if self.list_error.is_some() && nothing_loaded {
                "Details appear here once the list loads."
            } else {
                "Select an item to see details."
            };
            return self.render_empty(message, cx);
        }
        if let Some(error) = &self.detail_error {
            return self.render_error("detail", error, cx);
        }
        if self.tab == GitHubTab::PullRequests {
            return self.render_pr_detail(window, cx);
        }
        let Some(detail) = self.issue_detail.as_ref().filter(|detail| {
            self.selected_issue
                .as_ref()
                .is_some_and(|selected| selected.number == detail.summary.issue.number)
        }) else {
            return self.render_empty("Loading details…", cx);
        };
        let issue = detail.summary.issue.clone();
        let title = detail.summary.title.clone();
        let linked = self.linked_sessions(&issue, cx);
        let has_linked = !linked.is_empty();
        let work_dir = self
            .selected_issue
            .as_ref()
            .map(|key| key.project.clone())
            .or_else(|| self.project_work_dir.clone());
        let model = self.model.clone();
        let owner = cx.entity().downgrade();
        let controls = kit_github::GitHubIssueActions::new(issue.url.clone())
            .linked_task(has_linked)
            .closed(detail.summary.state.eq_ignore_ascii_case("closed"))
            .pending(self.issue_action_pending);
        let start_title = title.clone();
        let actions =
            kit_github::github_issue_actions(controls, move |action, window, cx| match action {
                kit_github::GitHubDetailAction::StartTask => {
                    if let Some(work_dir) = work_dir.clone() {
                        open_issue_start_dialog(
                            model.clone(),
                            work_dir,
                            issue.clone(),
                            start_title.clone(),
                            has_linked,
                            window,
                            cx,
                        );
                    }
                }
                kit_github::GitHubDetailAction::SetIssueClosed(close) => {
                    let _ = owner.update(cx, |view, cx| {
                        view.close_or_reopen_selected_issue(close, cx)
                    });
                }
                kit_github::GitHubDetailAction::SuggestLabels => {
                    let _ = owner.update(cx, |view, cx| view.suggest_issue_labels(cx));
                }
                kit_github::GitHubDetailAction::DeleteIssue => {
                    let _ = owner.update(cx, |view, cx| view.delete_selected_issue(window, cx));
                }
                _ => {}
            });
        let summary = &detail.summary;
        let header = kit_github::GitHubDetailHeader::new(
            title,
            format!(
                "{}/{} · #{} · {} · @{} · {}",
                summary.issue.owner,
                summary.issue.repo,
                summary.issue.number,
                summary.state,
                summary.author,
                format_github_time(&summary.updated_at, github_now_unix())
            ),
            actions.into_any_element(),
        )
        .status(self.issue_action_status.clone())
        .render(cx);
        let tasks = linked
            .into_iter()
            .map(|linked| {
                let model = self.model.clone();
                let session = linked.session;
                let task = kit_github::GitHubLinkedTask::new(
                    format!("open-linked-task-{}", session.id),
                    session.title.clone(),
                    linked.project_name,
                    linked.status.into(),
                )
                .worktree(session.is_worktree)
                .branch(linked.branch)
                .pr_number(linked.pr_number);
                kit_github::github_linked_task(
                    task,
                    move |_, _, cx| {
                        model.update(cx, |state, cx| {
                            controller::dispatch(
                                state,
                                AppAction::SelectSession {
                                    work_dir: session.work_dir.clone(),
                                    session_id: session.id.clone(),
                                },
                            );
                            controller::dispatch(state, AppAction::CloseGitHub);
                            cx.notify();
                        });
                    },
                    cx,
                )
                .into_any_element()
            })
            .collect();
        let comments = (!self.comment_rows.is_empty()).then(|| {
            (
                self.comment_rows.len(),
                div()
                    .size_full()
                    .relative()
                    .child(
                        list(
                            self.comment_list_state.clone(),
                            cx.processor(Self::render_comment_row),
                        )
                        .size_full()
                        .with_sizing_behavior(ListSizingBehavior::Infer),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .child(Scrollbar::vertical(&self.comment_list_state)),
                    )
                    .into_any_element(),
            )
        });
        let body = kit_github::github_issue_body(
            detail,
            &self.detail_body,
            tasks,
            comments,
            self.detail_loading,
            cx,
        );
        kit_github::github_detail_surface(header, body.into_any_element()).into_any_element()
    }

    /// List summary for the workspace status bar while this page is shown.
    pub fn status_text(&self, cx: &App) -> String {
        let count = match self.tab {
            GitHubTab::Issues => self.issues.len(),
            GitHubTab::PullRequests => self.pull_requests.len(),
        };
        let has_draft = match self.tab {
            GitHubTab::Issues => !self.issue_comment_draft.is_empty(),
            GitHubTab::PullRequests => !self.pr_review_draft.is_empty(),
        };
        let projects = self.attached_projects(cx);
        kit_github::github_status_text(count, self.tab.label(), self.scope.label(&projects), self.state_filter, has_draft)
    }
}

impl Render for GitHubView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let detail = self.render_detail(window, cx);
        let content =
            if self.tab == GitHubTab::PullRequests && self.current_pr_tab() == PrDetailTab::Code {
                detail
            } else {
                let master = kit_github::github_master(
                    self.render_scope_row(cx),
                    self.render_filters(cx),
                    self.render_list(window, cx),
                );
                kit_github::github_master_detail(
                    master.into_any_element(),
                    detail,
                    &self.detail_split_state,
                    window,
                    cx,
                )
            };

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(content))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detail_result_matches_list, draft_reply_prompt, github_empty_message,
        github_link_fingerprint_rows, github_result_matches_request, github_server_query,
        github_state_for_tab, issue_start_activation, issue_start_confirmation,
        issue_start_dialog_result, linked_session_fingerprint,
        linked_session_ids, linked_session_status, linked_sessions_across_projects,
        list_count_splice, merge_pr_timeline, pr_check_label, pr_diff_result_matches_request,
        pr_file_action_ix, pr_publish_control, pr_publish_refresh_matches_selection,
        next_unviewed_file, prepare_selected_diff, pr_viewed_progress, selected_file_diff,
        selected_issue_after_refresh, GitHubItemKey, GitHubRequest, GitHubScope, GitHubStateFilter,
        GitHubTab, GitHubView, PrCommentControl, PrCommentDrafts, PrCommentPhase, PrDetailTab,
        PrDiffRequest, PrFileAction, PrReadback, PrReplyTarget, PrTimelineKind, PrViewedSnapshot,
        PrViewedStates, PrViewedTransport, PrWorkspaceKey, PrWorkspaceSelections, ScopedIssue,
        ScopedPr,
    };
    use threadlane_ui_state::{
        AppState, SessionCompletionSummary, SessionHealth, SessionInfo,
    };
    use gpui::{AppContext as _, Focusable as _};
    use std::path::PathBuf;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use threadlane_git::{
        GitHubIssueRef, GitHubIssueSummary, GitHubPrFile, GitHubPrFileViewed, GitHubPrInfo,
        GitHubPrViewedState, GitHubPullRequestSummary, PrCheckStatus, PrConversationComment,
        PrFileViewedStatus, PrReview, PrReviewComment,
    };

    #[gpui::test]
    fn issue_delete_confirmation_rejects_same_number_in_another_project(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui_component::WindowExt;
        cx.update(gpui_component::init);
        let mut captured = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| {
                let mut state = AppState::default();
                state.projects.clear();
                state.active_work_dir = None;
                state
            });
            let view = cx.new(|cx| GitHubView::new(model, window, cx));
            captured = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view = captured.unwrap();
        cx.run_until_parked();
        let original = GitHubItemKey {
            project: PathBuf::from("/projects/original"),
            number: 42,
        };
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.issues = vec![ScopedIssue {
                    project: original.project.clone(),
                    project_name: "Original project".into(),
                    summary: issue(42),
                }];
                view.selected_issue = Some(original.clone());
                view.issue_list_state.reset(1);
                view.delete_selected_issue(window, cx);
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds("github-issue-delete-identity").is_some());
        cx.simulate_keystrokes("enter");
        assert!(
            cx.update(|window, cx| window.has_active_dialog(cx)),
            "bare Enter never deletes"
        );
        view.update(cx, |view, _| {
            view.selected_issue = Some(GitHubItemKey {
                project: PathBuf::from("/projects/other"),
                number: 42,
            })
        });
        let confirm = cx.debug_bounds("confirm-delete-issue").unwrap();
        cx.simulate_click(confirm.center(), gpui::Modifiers::default());
        view.read_with(cx, |view, _| {
            assert!(!view.issue_action_pending, "no mutation was dispatched");
            assert_eq!(
                view.issue_action_status.as_deref(),
                Some("Issue selection changed. Open the delete confirmation again.")
            );
        });
        assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    }

    fn issue(number: u64) -> GitHubIssueSummary {
        GitHubIssueSummary {
            issue: GitHubIssueRef {
                host: "github.com".into(),
                owner: "threadlane".into(),
                repo: "app".into(),
                number,
                url: format!("https://github.com/threadlane/app/issues/{number}"),
            },
            title: "Fix linked task browser".into(),
            author: "octocat".into(),
            labels: vec![threadlane_git::GitHubLabel {
                name: "desktop".into(),
                ..Default::default()
            }],
            assignees: vec!["maintainer".into()],
            ..Default::default()
        }
    }

    fn session(id: &str, github_issue: Option<GitHubIssueRef>) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            title: id.into(),
            work_dir: "/project".into(),
            runtime_work_dir: "/project".into(),
            session_file: format!("/project/{id}.jsonl").into(),
            updated_at: 0,
            health: SessionHealth::Healthy,
            git_branch: None,
            github_issue,
            is_worktree: false,
            worktree_available: true,
            completion_summary: SessionCompletionSummary::Unknown,
        }
    }

    fn configure_pr_workspace(view: &mut GitHubView, cx: &mut gpui::Context<GitHubView>) {
        let project = PathBuf::from("/projects/app");
        let key = PrWorkspaceKey {
            project: project.clone(),
            number: 42,
        };
        let files = vec![
            GitHubPrFile {
                path: "src/lib.rs".into(),
                ..Default::default()
            },
            GitHubPrFile {
                path: "src/view.rs".into(),
                ..Default::default()
            },
        ];
        view.model.update(cx, |model, _cx| {
            model.projects.clear();
            model.github_tab = GitHubTab::PullRequests;
            threadlane_ui_state::activate_test_session(model, "app", &project.join("fixture.jsonl"));
        });
        view.project_work_dir = Some(project.clone());
        view.scope = GitHubScope::Project(project.clone());
        view.scope_initialized = true;
        view.tab = GitHubTab::PullRequests;
        view.selected_pr = Some(GitHubItemKey {
            project: project.clone(),
            number: key.number,
        });
        view.pull_requests = vec![ScopedPr {
            project,
            project_name: "app".into(),
            summary: GitHubPullRequestSummary {
                number: key.number,
                title: "Inspect PR".into(),
                url: "https://github.com/threadlane/app/pull/42".into(),
                ..Default::default()
            },
        }];
        view.pr_detail = Some(GitHubPrInfo {
            number: key.number,
            title: "Inspect PR".into(),
            url: "https://github.com/threadlane/app/pull/42".into(),
            head_oid: "head-42".into(),
            files: files.clone(),
            ..Default::default()
        });
        view.pr_selections.reconcile_files(&key, &files);
        view.pr_list_state.reset(1);
        view.pr_file_list_state.reset(files.len());
        cx.notify();
    }

    #[gpui::test]
    fn github_pr_reply_handoff_stays_in_project_and_tracks_live_branch(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            GitHubView::new(model, window, cx)
        });
        view.update(cx, |view, cx| {
            configure_pr_workspace(view, cx);
            let project = view.selected_pr.as_ref().unwrap().project.clone();
            let before = super::github_link_fingerprint(view.model.read(cx));
            view.model.update(cx, |state, _| {
                let checkout = state.projects[0].sessions[0].runtime_work_dir.clone();
                state.git_statuses.insert(
                    checkout,
                    threadlane_git::GitStatus {
                        branch: Some("feature/review".into()),
                        ..Default::default()
                    },
                );
            });
            assert_ne!(before, super::github_link_fingerprint(view.model.read(cx)));
            assert!(
                view.linked_pr_task(&project, "feature/review", cx)
                    .is_some()
            );
            view.handoff_pr_reply("feature/review", "draft this reply".into(), cx);
            assert_eq!(
                view.model.read(cx).requested_composer_prompt.as_deref(),
                Some("draft this reply")
            );
            view.model.update(cx, |state, _| {
                state.requested_composer_prompt = None;
            });
            view.selected_pr.as_mut().unwrap().project = PathBuf::from("/projects/other");
            let active = view.model.read(cx).active_session_id.clone();
            view.handoff_pr_reply("feature/review", "wrong project".into(), cx);
            assert!(view.model.read(cx).requested_composer_prompt.is_none());
            assert_eq!(view.model.read(cx).active_session_id, active);
        });
    }

    #[test]
    fn github_pr_merged_filter_uses_merged_state() {
        assert_eq!(GitHubStateFilter::Merged.value(), "merged");
    }

    #[gpui::test]
    fn github_sidebar_navigation_and_page_tabs_stay_synchronized(cx: &mut gpui::TestAppContext) {
        use threadlane_ui_state::{actions::AppAction, controller, WorkspacePage};

        cx.update(gpui_component::init);
        let mut github = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| {
                let mut state = AppState::default();
                state.projects.clear();
                state.active_session_id = Some("keep-current-chat".into());
                state
            });
            let view = cx.new(|cx| GitHubView::new(model, window, cx));
            github = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let view = github.unwrap();
        let model = view.read_with(cx, |view, _| view.model.clone());

        for tab in [GitHubTab::Issues, GitHubTab::PullRequests] {
            model.update(cx, |state, cx| {
                controller::dispatch(state, AppAction::OpenGitHubTab(tab));
                cx.notify();
            });
            cx.run_until_parked();
            cx.update(|window, cx| { window.refresh(); window.draw(cx).clear(cx); });
            let heading = cx.debug_bounds("github-page-heading").expect("named page heading");
            // The heading shares Chat's header row: inside the window-controls band.
            assert!(heading.top() > gpui::px(0.)
                    && heading.bottom() <= threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE,
                "{tab:?} heading must sit in the shared window-controls header row: {heading:?}");
        }

        model.update(cx, |state, cx| {
            controller::dispatch(state, AppAction::OpenGitHubTab(GitHubTab::PullRequests));
            cx.notify();
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.tab, GitHubTab::PullRequests);
            assert_eq!(view.scope, GitHubScope::All);
        });
        view.update(cx, |view, cx| {
            view.state_filter = GitHubStateFilter::Merged;
            view.select_tab(GitHubTab::Issues, cx);
            assert_eq!(view.state_filter, GitHubStateFilter::Open);
        });
        model.read_with(cx, |state, _| {
            assert_eq!(state.workspace_page, WorkspacePage::GitHub);
            assert_eq!(state.github_tab, GitHubTab::Issues);
            assert_eq!(state.active_session_id.as_deref(), Some("keep-current-chat"));
        });
        view.update(cx, |view, cx| view.select_tab(GitHubTab::PullRequests, cx));
        model.update(cx, |state, cx| {
            controller::dispatch(state, AppAction::CloseGitHub);
            controller::dispatch(state, AppAction::OpenGitHub);
            cx.notify();
        });
        assert_eq!(model.read_with(cx, |state, _| state.github_tab), GitHubTab::PullRequests);
        assert_eq!(view.read_with(cx, |view, _| view.tab), GitHubTab::PullRequests);
    }

    #[gpui::test]
    fn github_errors_preserve_search_space_cached_results_and_raw_details(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::*;

        struct Harness(Entity<GitHubView>);
        impl Render for Harness {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                self.0.update(cx, |view, cx| {
                    div()
                        .w(rems(32.5))
                        .h(rems(31.25))
                        .flex()
                        .flex_col()
                        .child(view.render_filters(cx))
                        .child(div().flex_1().min_h_0().child(view.render_list(window, cx)))
                })
            }
        }

        let raw = format!(
            "GraphQL: API rate limit exceeded for user ID 123456.\n{}",
            "diagnostic detail\n".repeat(100)
        );
        assert_eq!(
            super::github_error_message(&raw),
            "GitHub’s API limit has been reached. Wait before retrying."
        );
        assert!(super::github_error_message("HTTP 429 from GitHub").contains("API limit"));
        assert!(super::github_error_message(
            "/repo: could not start gh: No such file or directory (os error 2)"
        )
        .contains("isn’t installed"));
        assert!(super::github_error_message(&"unknown provider body ".repeat(100)).len() < 120);
        cx.update(gpui_component::init);
        let (harness, cx) = cx.add_window_view(|window, cx| {
            Harness(cx.new(|cx| {
                let model = cx.new(|_| AppState::default());
                let mut view = GitHubView::new(model, window, cx);
                configure_pr_workspace(&mut view, cx);
                view
            }))
        });
        let view = harness.read_with(cx, |harness, _| harness.0.clone());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let search_width = cx.debug_bounds("github-search-field").unwrap().size.width;
        assert!(search_width > px(450.0), "Search owns a full row instead of competing with state filters");

        view.update(cx, |view, cx| {
            view.list_error = Some(raw.clone());
            cx.notify();
        });
        harness.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            cx.debug_bounds("github-search-field").unwrap().size.width,
            search_width
        );
        let error = cx.debug_bounds("github-list-warning").unwrap();
        let results = cx.debug_bounds("github-result-list").unwrap();
        assert!(error.bottom() <= results.top());
        assert!(error.size.height < px(200.0));
        assert!(cx.debug_bounds("github-list-warning-retry").is_some());
        let copy = cx.debug_bounds("github-list-warning-copy").unwrap();
        cx.simulate_click(copy.center(), Modifiers::default());
        cx.update(|_, cx| assert_eq!(cx.read_from_clipboard().unwrap().text(), Some(raw.clone())));

        view.update(cx, |view, cx| {
            view.pull_requests.clear();
            view.pr_list_state.reset(0);
            cx.notify();
        });
        harness.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("github-list-error-copy").is_some());
        assert!(cx.debug_bounds("github-result-list").is_none());
        assert_eq!(
            cx.debug_bounds("github-search-field").unwrap().size.width,
            search_width
        );
    }

    #[test]
    fn github_issues_reset_the_pr_only_merged_filter() {
        assert_eq!(
            github_state_for_tab(GitHubStateFilter::Merged, GitHubTab::Issues),
            GitHubStateFilter::Open
        );
    }

    fn pr_key(project: &str, number: u64) -> PrWorkspaceKey {
        PrWorkspaceKey {
            project: PathBuf::from(project),
            number,
        }
    }

    fn reply_target(remote_id: &str, author: &str) -> PrReplyTarget {
        PrReplyTarget {
            remote_id: remote_id.into(),
            reply_to_remote_id: None,
            author: author.into(),
            body: "remote review context".into(),
            path: Some("src/lib.rs".into()),
            line: Some(17),
        }
    }

    fn begin_reply(
        drafts: &mut PrCommentDrafts,
        key: &PrWorkspaceKey,
        target: PrReplyTarget,
        body: &str,
        ids: &[&str],
    ) -> super::PrCommentAttempt {
        drafts.select_reply_target(key.clone(), target);
        drafts.set_reply_body(key, body.into());
        drafts
            .begin_reply(
                key,
                format!("https://github.com/threadlane/app/pull/{}", key.number),
                ids.iter().map(|id| (*id).into()).collect(),
            )
            .unwrap()
            .unwrap()
    }

    #[test]
    fn github_pr_draft_switching_prs_and_projects_retains_exact_bodies() {
        let mut drafts = PrCommentDrafts::default();
        let first = pr_key("/projects/app", 42);
        let second = pr_key("/projects/app", 43);
        let other_project = pr_key("/projects/other", 42);
        drafts.set_body(first.clone(), " first comment \n".into());
        drafts.set_body(second.clone(), "second comment".into());
        drafts.set_body(other_project.clone(), "other project".into());
        assert_eq!(drafts.get(&first).unwrap().body, " first comment \n");
        assert_eq!(drafts.get(&second).unwrap().body, "second comment");
        assert_eq!(drafts.get(&other_project).unwrap().body, "other project");
    }

    #[test]
    fn github_pr_reply_drafts_are_per_pr_and_guard_target_switches() {
        let first = pr_key("/projects/app", 42);
        let second = pr_key("/projects/app", 43);
        let mut drafts = PrCommentDrafts::default();
        assert!(drafts.select_reply_target(first.clone(), reply_target("101", "alice")));
        drafts.set_reply_body(&first, " first reply \n".into());
        assert!(drafts.select_reply_target(second.clone(), reply_target("202", "bob")));
        drafts.set_reply_body(&second, "second reply".into());
        assert!(!drafts.select_reply_target(first.clone(), reply_target("303", "carol")));
        let retained = drafts.get(&first).unwrap().reply.as_ref().unwrap();
        assert_eq!(
            (&retained.target.remote_id, retained.body.as_str()),
            (&"101".into(), " first reply \n")
        );
        assert!(retained.blocked);
        assert_eq!(
            drafts.get(&second).unwrap().reply.as_ref().unwrap().body,
            "second reply"
        );
        drafts.set_reply_body(&first, String::new());
        assert!(drafts.select_reply_target(first.clone(), reply_target("303", "carol")));
    }

    #[test]
    fn github_pr_reply_rejects_invalid_target_before_an_attempt_can_begin() {
        for invalid in ["", "not-a-number", "0", "-1"] {
            let key = pr_key("/projects/app", 42);
            let mut drafts = PrCommentDrafts::default();
            drafts.select_reply_target(key.clone(), reply_target(invalid, "alice"));
            drafts.set_reply_body(&key, "reply".into());
            assert_eq!(
                drafts
                    .begin_reply(
                        &key,
                        "https://github.com/threadlane/app/pull/42".into(),
                        Default::default(),
                    )
                    .unwrap_err(),
                "This review comment can’t be replied to because GitHub returned an invalid comment ID."
            );
            assert!(drafts
                .get(&key)
                .unwrap()
                .reply
                .as_ref()
                .unwrap()
                .publish
                .attempt
                .is_none());
        }

        let key = pr_key("/projects/app", 42);
        let mut drafts = PrCommentDrafts::default();
        let mut nested = reply_target("101", "alice");
        nested.reply_to_remote_id = Some("invalid-parent".into());
        drafts.select_reply_target(key.clone(), nested);
        drafts.set_reply_body(&key, "reply".into());
        assert_eq!(
            drafts
                .begin_reply(
                    &key,
                    "https://example.com/pull/42".into(),
                    Default::default()
                )
                .unwrap_err(),
            super::INVALID_PR_REPLY_TARGET
        );
    }

    #[test]
    fn github_pr_reply_success_is_snapshot_gated_and_stale_identity_is_ignored() {
        let key = pr_key("/projects/app", 42);
        let target = reply_target("101", "alice");
        let mut drafts = PrCommentDrafts::default();
        let attempt = begin_reply(&mut drafts, &key, target.clone(), "submitted reply", &[]);
        let mut exact = drafts.clone();
        assert!(exact.complete_success(&attempt));
        assert!(exact.get(&key).unwrap().reply.is_none());
        drafts.set_reply_body(&key, "newer local edit".into());
        let mut stale = attempt.clone();
        stale.token = stale.token.saturating_add(1);
        let mut wrong_key = attempt.clone();
        wrong_key.key = pr_key("/projects/app", 43);
        assert!(!drafts.complete_success(&stale));
        assert!(!drafts.complete_readback(&wrong_key, PrReadback::Absent, "ignored".into()));

        let mut newer_target = drafts.clone();
        newer_target
            .by_pr
            .get_mut(&key)
            .unwrap()
            .reply
            .as_mut()
            .unwrap()
            .target
            .author = "updated remote context".into();
        assert!(!newer_target.complete_success(&attempt));
        assert!(newer_target.get(&key).unwrap().reply.is_some());

        assert!(drafts.complete_success(&attempt));
        assert_eq!(
            drafts.get(&key).unwrap().reply.as_ref().unwrap().body,
            "newer local edit"
        );
    }

    #[test]
    fn github_pr_reply_unresolved_attempt_blocks_an_empty_body_target_switch() {
        let key = pr_key("/projects/app", 42);
        let target = reply_target("101", "alice");
        let mut drafts = PrCommentDrafts::default();
        let attempt = begin_reply(&mut drafts, &key, target.clone(), "submitted reply", &[]);

        drafts.set_reply_body(&key, String::new());
        assert!(!drafts.select_reply_target(key.clone(), reply_target("303", "carol")));
        assert_eq!(
            drafts.get(&key).unwrap().reply.as_ref().unwrap().target,
            target
        );
        assert!(drafts.mark_checking(&attempt, "ambiguous".into()));
        assert!(!drafts.select_reply_target(key.clone(), reply_target("303", "carol")));
        assert!(drafts.complete_readback(&attempt, PrReadback::Absent, "not found".into()));
        assert!(!drafts.select_reply_target(key, reply_target("303", "carol")));
    }

    #[test]
    fn github_pr_reply_readback_requires_a_new_review_comment_id_and_retains_context() {
        let key = pr_key("/projects/app", 42);
        let target = reply_target("101", "alice");
        let mut drafts = PrCommentDrafts::default();
        let attempt = begin_reply(
            &mut drafts,
            &key,
            target.clone(),
            " exact\nreply ",
            &["old"],
        );
        let old_only = GitHubPrInfo {
            number: 42,
            review_comments_complete: true,
            review_comments: vec![PrReviewComment {
                remote_id: "old".into(),
                body: "exact reply".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            super::classify_pr_readback(&attempt, Ok(&old_only)),
            PrReadback::Absent
        );
        let with_new = GitHubPrInfo {
            review_comments: vec![
                old_only.review_comments[0].clone(),
                PrReviewComment {
                    remote_id: "new".into(),
                    body: "exact  reply".into(),
                    ..Default::default()
                },
            ],
            ..old_only
        };
        assert_eq!(
            super::classify_pr_readback(&attempt, Ok(&with_new)),
            PrReadback::Present
        );

        for (outcome, phase) in [
            (PrReadback::Present, PrCommentPhase::Present),
            (PrReadback::Absent, PrCommentPhase::Absent),
            (PrReadback::Unknown, PrCommentPhase::Unknown),
        ] {
            let mut terminal = drafts.clone();
            terminal.mark_checking(&attempt, "ambiguous".into());
            assert!(terminal.complete_readback(&attempt, outcome, "ambiguous".into()));
            let retained = terminal.get(&key).unwrap().reply.as_ref().unwrap();
            assert_eq!(
                (
                    &retained.target,
                    retained.body.as_str(),
                    retained.publish.phase
                ),
                (&target, " exact\nreply ", phase)
            );
            terminal.set_reply_body(&key, String::new());
            assert!(!terminal.select_reply_target(key.clone(), reply_target("303", "carol")));
            terminal.set_reply_body(&key, "newer B".into());
            assert_eq!(
                {
                    let reply = terminal.get(&key).unwrap().reply.as_ref().unwrap();
                    pr_publish_control(&reply.body, &reply.publish)
                },
                PrCommentControl::PostNewDraft
            );
            assert!(
                terminal
                    .begin_reply(&key, attempt.pr_url.clone(), Default::default())
                    .unwrap()
                    .unwrap()
                    .token
                    > attempt.token
            );
        }
    }

    #[test]
    fn github_pr_reply_readback_is_unknown_until_review_comments_are_complete() {
        let key = pr_key("/projects/app", 42);
        let mut drafts = PrCommentDrafts::default();
        let attempt = begin_reply(
            &mut drafts,
            &key,
            reply_target("101", "alice"),
            "submitted reply",
            &[],
        );
        let incomplete = GitHubPrInfo {
            number: 42,
            ..Default::default()
        };
        assert_eq!(
            super::classify_pr_readback(&attempt, Ok(&incomplete)),
            PrReadback::Unknown
        );
        assert_eq!(
            super::classify_pr_readback(
                &attempt,
                Ok(&GitHubPrInfo {
                    review_comments_complete: true,
                    ..incomplete
                })
            ),
            PrReadback::Absent
        );
    }

    #[test]
    fn github_pr_reply_target_exists_only_for_inline_review_comments() {
        let row = |kind| super::PrTimelineRow {
            remote_id: "101".into(),
            kind,
            author: "alice".into(),
            body: "remote review context".into(),
            timestamp: "2026-08-30T12:00:00Z".into(),
            url: "https://github.com/threadlane/app/pull/42".into(),
            review_state: None,
            path: Some("src/lib.rs".into()),
            line: Some(17),
            in_reply_to_id: None,
        };

        assert_eq!(
            row(PrTimelineKind::InlineReviewComment).reply_target(),
            Some(reply_target("101", "alice"))
        );
        assert!(row(PrTimelineKind::IssueComment).reply_target().is_none());
        assert!(row(PrTimelineKind::Review).reply_target().is_none());
    }

    #[test]
    fn github_pr_reply_to_reply_posts_to_the_top_level_parent() {
        let rows = merge_pr_timeline(&GitHubPrInfo {
            url: "https://github.com/threadlane/app/pull/42".into(),
            review_comments: vec![PrReviewComment {
                remote_id: "101".into(),
                in_reply_to_id: Some("41".into()),
                author: "alice".into(),
                body: "clicked reply context".into(),
                path: Some("src/lib.rs".into()),
                line: Some(17),
                ..Default::default()
            }],
            ..Default::default()
        });
        let target = rows[0].reply_target().unwrap();

        assert_eq!(target.remote_id, "101");
        assert_eq!(target.body, "clicked reply context");
        assert_eq!(target.comment_id(), Ok(41));
        let mut drafts = PrCommentDrafts::default();
        let attempt = begin_reply(
            &mut drafts,
            &pr_key("/projects/app", 42),
            target,
            "reply",
            &[],
        );
        assert!(matches!(
            attempt.target,
            super::PrCommentTarget::Reply(_, 41)
        ));
    }

    #[gpui::test]
    fn github_pr_reply_editor_reaches_present_recovery_before_the_textarea(
        cx: &mut gpui::TestAppContext,
    ) {
        struct ReplyEditor(gpui::Entity<GitHubView>);

        impl gpui::Render for ReplyEditor {
            fn render(
                &mut self,
                _window: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                self.0
                    .update(cx, |view, cx| view.render_pr_reply_editor(cx).unwrap())
            }
        }

        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let key = pr_key("/projects/app", 42);
        let mut drafts = PrCommentDrafts::default();
        let attempt = begin_reply(
            &mut drafts,
            &key,
            reply_target("101", "alice"),
            "submitted reply",
            &[],
        );
        let other_project_attempt = begin_reply(
            &mut PrCommentDrafts::default(),
            &pr_key("/projects/other", 42),
            reply_target("101", "alice"),
            "submitted reply",
            &[],
        );
        assert_ne!(
            super::pr_present_action_id(false, &attempt),
            super::pr_present_action_id(true, &attempt)
        );
        assert_ne!(
            super::pr_present_action_id(true, &attempt),
            super::pr_present_action_id(true, &other_project_attempt)
        );
        let expected_url = attempt.pr_url.clone();
        let (editor, cx) = cx.add_window_view(move |window, cx| {
            let model = cx.new(|_| AppState::default());
            let view = cx.new(|cx| {
                let mut view = GitHubView::new(model, window, cx);
                configure_pr_workspace(&mut view, cx);
                view.pr_drafts = drafts;
                assert!(view
                    .pr_drafts
                    .mark_checking(&attempt, "ambiguous write".into()));
                assert!(view.pr_drafts.complete_readback(
                    &attempt,
                    PrReadback::Present,
                    "relationship unknown".into()
                ));
                view
            });
            ReplyEditor(view)
        });
        let reply_input_focus = editor.read_with(cx, |editor, cx| {
            editor.0.read(cx).pr_reply_input.read(cx).focus_handle(cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            window.focus_next(cx);
            assert_ne!(window.focused(cx).as_ref(), Some(&reply_input_focus));
            window.draw(cx).clear(cx);
        });
        let keystroke = gpui::Keystroke::parse("enter").unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });

        assert_eq!(cx.opened_url().as_deref(), Some(expected_url.as_str()));
    }

    #[test]
    fn github_pr_conversation_readback_outcomes_retain_exact_body() {
        for (outcome, phase) in [
            (PrReadback::Present, PrCommentPhase::Present),
            (PrReadback::Absent, PrCommentPhase::Absent),
            (PrReadback::Unknown, PrCommentPhase::Unknown),
        ] {
            let mut drafts = PrCommentDrafts::default();
            let key = pr_key("/projects/app", 42);
            drafts.set_body(key.clone(), "  exact comment\n\n".into());
            let attempt = drafts
                .begin(
                    &key,
                    "https://github.com/threadlane/app/pull/42".into(),
                    ["old".to_string()].into_iter().collect(),
                )
                .unwrap();
            drafts.mark_checking(&attempt, "POST result was ambiguous".into());
            drafts.complete_readback(&attempt, outcome, "POST failed".into());
            let draft = drafts.get(&key).unwrap();
            assert_eq!(draft.body, "  exact comment\n\n");
            assert_eq!(draft.publish.phase, phase);
        }
    }

    #[test]
    fn github_pr_conversation_newer_draft_is_postable_without_discarding_old_evidence() {
        for (outcome, phase, old_control) in [
            (
                PrReadback::Present,
                PrCommentPhase::Present,
                PrCommentControl::ClearDraft,
            ),
            (
                PrReadback::Absent,
                PrCommentPhase::Absent,
                PrCommentControl::Retry,
            ),
            (
                PrReadback::Unknown,
                PrCommentPhase::Unknown,
                PrCommentControl::CheckAgain,
            ),
        ] {
            let key = pr_key("/projects/app", 42);
            let mut drafts = PrCommentDrafts::default();
            drafts.set_body(key.clone(), "submitted A".into());
            let attempt = drafts
                .begin(
                    &key,
                    "https://github.com/threadlane/app/pull/42".into(),
                    Default::default(),
                )
                .unwrap();
            drafts.mark_checking(&attempt, "ambiguous".into());

            let mut unchanged = drafts.clone();
            unchanged.complete_readback(&attempt, outcome, "ambiguous".into());
            assert_eq!(
                {
                    let draft = unchanged.get(&key).unwrap();
                    pr_publish_control(&draft.body, &draft.publish)
                },
                old_control
            );

            drafts.set_body(key.clone(), "newer B".into());
            drafts.complete_readback(&attempt, outcome, "ambiguous".into());
            let draft = drafts.get(&key).unwrap();
            assert_eq!(draft.body, "newer B");
            assert_eq!(draft.publish.phase, phase);
            assert_eq!(draft.publish.attempt.as_ref().unwrap().body, "submitted A");
            assert_eq!(
                draft.publish.attempt.as_ref().unwrap().pr_url,
                "https://github.com/threadlane/app/pull/42"
            );
            assert_eq!(
                pr_publish_control(&draft.body, &draft.publish),
                PrCommentControl::PostNewDraft
            );

            let mut stale_completion = drafts.clone();
            assert!(stale_completion.complete_success(&attempt));
            assert_eq!(stale_completion.get(&key).unwrap().body, "newer B");

            let next = drafts
                .begin(
                    &key,
                    "https://github.com/threadlane/app/pull/42".into(),
                    Default::default(),
                )
                .unwrap();
            assert!(next.token > attempt.token);
            assert_eq!(next.body, "newer B");
        }
    }

    #[test]
    fn github_pr_conversation_trusted_success_clears_only_matching_published_snapshot() {
        let key = pr_key("/projects/app", 42);
        let mut exact = PrCommentDrafts::default();
        exact.set_body(key.clone(), "publish me".into());
        let exact_attempt = exact
            .begin(
                &key,
                "https://github.com/threadlane/app/pull/42".into(),
                Default::default(),
            )
            .unwrap();
        assert!(exact.complete_success(&exact_attempt));
        assert_eq!(exact.get(&key).unwrap().body, "");

        let mut edited = PrCommentDrafts::default();
        edited.set_body(key.clone(), "published snapshot".into());
        let old_attempt = edited
            .begin(
                &key,
                "https://github.com/threadlane/app/pull/42".into(),
                Default::default(),
            )
            .unwrap();
        edited.set_body(key.clone(), "newer local edit".into());
        assert!(edited.complete_success(&old_attempt));
        assert_eq!(edited.get(&key).unwrap().body, "newer local edit");
    }

    #[test]
    fn github_pr_conversation_preexisting_same_body_is_not_present_without_new_remote_id() {
        let key = pr_key("/projects/app", 42);
        let mut drafts = PrCommentDrafts::default();
        drafts.set_body(key.clone(), " same\nbody ".into());
        let attempt = drafts
            .begin(
                &key,
                "https://github.com/threadlane/app/pull/42".into(),
                ["old".to_string()].into_iter().collect(),
            )
            .unwrap();
        let old_only = GitHubPrInfo {
            number: 42,
            issue_comments: vec![PrConversationComment {
                remote_id: "old".into(),
                body: "same body".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            super::classify_pr_readback(&attempt, Ok(&old_only)),
            PrReadback::Absent
        );

        let with_new = GitHubPrInfo {
            issue_comments: vec![
                old_only.issue_comments[0].clone(),
                PrConversationComment {
                    remote_id: "new".into(),
                    body: "same\n body".into(),
                    ..Default::default()
                },
            ],
            ..old_only
        };
        assert_eq!(
            super::classify_pr_readback(&attempt, Ok(&with_new)),
            PrReadback::Present
        );
    }

    #[test]
    fn github_pr_conversation_stale_attempt_cannot_mutate_newer_state() {
        let key = pr_key("/projects/app", 42);
        let mut drafts = PrCommentDrafts::default();
        drafts.set_body(key.clone(), "keep me".into());
        let old = drafts
            .begin(
                &key,
                "https://github.com/threadlane/app/pull/42".into(),
                Default::default(),
            )
            .unwrap();
        drafts.mark_checking(&old, "ambiguous".into());
        drafts.complete_readback(&old, PrReadback::Unknown, "ambiguous".into());
        let current = drafts.begin_recheck(&key).unwrap();

        assert!(current.token > old.token);
        assert!(!drafts.complete_success(&old));
        assert_eq!(drafts.get(&key).unwrap().body, "keep me");
        assert_eq!(
            drafts
                .get(&key)
                .unwrap()
                .publish
                .attempt
                .as_ref()
                .unwrap()
                .token,
            current.token
        );
    }

    #[test]
    fn github_pr_conversation_completion_targets_captured_key_not_visible_selection() {
        let first = pr_key("/projects/app", 42);
        let visible = pr_key("/projects/app", 43);
        let mut drafts = PrCommentDrafts::default();
        drafts.set_body(first.clone(), "first body".into());
        drafts.set_body(visible.clone(), "visible body".into());
        let attempt = drafts
            .begin(
                &first,
                "https://github.com/threadlane/app/pull/42".into(),
                Default::default(),
            )
            .unwrap();

        assert!(!pr_publish_refresh_matches_selection(
            &attempt,
            Some(&visible)
        ));
        assert!(drafts.complete_success(&attempt));
        assert_eq!(drafts.get(&visible).unwrap().body, "visible body");
        assert_eq!(drafts.get(&first).unwrap().body, "");
    }

    #[gpui::test]
    fn github_pr_conversation_completion_does_not_match_hidden_pr_on_issues_tab(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            GitHubView::new(model, window, cx)
        });
        let attempt = view.update_in(cx, |view, _, cx| {
            configure_pr_workspace(view, cx);
            let key = pr_key("/projects/app", 42);
            view.pr_drafts.set_body(key.clone(), "submitted".into());
            let attempt = view
                .pr_drafts
                .begin(
                    &key,
                    "https://example.com/pull/42".into(),
                    Default::default(),
                )
                .unwrap();
            view.tab = GitHubTab::Issues;
            view.model.update(cx, |state, _| state.github_tab = GitHubTab::Issues);
            attempt
        });

        assert!(view.read_with(cx, |view, _| view.current_pr_key().is_none()));
        assert!(
            !view.read_with(cx, |view, _| pr_publish_refresh_matches_selection(
                &attempt,
                view.current_pr_key().as_ref(),
            ))
        );
    }

    #[gpui::test]
    fn github_pr_draft_inputs_mirror_the_active_pr_without_overwriting_other_keys(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            GitHubView::new(model, window, cx)
        });
        view.update_in(cx, |view, window, cx| {
            configure_pr_workspace(view, cx);
            let first = pr_key("/projects/app", 42);
            view.pr_drafts
                .set_body(first.clone(), " first comment\n".into());
            view.pr_drafts
                .select_reply_target(first.clone(), reply_target("101", "alice"));
            view.pr_drafts
                .set_reply_body(&first, " first reply\n".into());
            view.sync_pr_draft_inputs(window, cx);
        });
        assert_eq!(
            view.read_with(cx, |view, cx| view.pr_comment_input.read(cx).value()),
            " first comment\n"
        );
        assert_eq!(
            view.read_with(cx, |view, cx| view.pr_reply_input.read(cx).value()),
            " first reply\n"
        );
        view.update_in(cx, |view, window, cx| {
            let second = pr_key("/projects/app", 43);
            view.selected_pr = Some(GitHubItemKey {
                project: PathBuf::from("/projects/app"),
                number: 43,
            });
            view.pr_drafts
                .set_body(second.clone(), "second comment".into());
            view.pr_drafts
                .select_reply_target(second.clone(), reply_target("202", "bob"));
            view.pr_drafts
                .set_reply_body(&second, "second reply".into());
            view.sync_pr_draft_inputs(window, cx);
        });
        assert_eq!(
            view.read_with(cx, |view, cx| view.pr_comment_input.read(cx).value()),
            "second comment"
        );
        assert_eq!(
            view.read_with(cx, |view, cx| view.pr_reply_input.read(cx).value()),
            "second reply"
        );
        assert_eq!(
            view.read_with(cx, |view, _| {
                view.pr_drafts
                    .get(&pr_key("/projects/app", 42))
                    .unwrap()
                    .body
                    .clone()
            }),
            " first comment\n"
        );
        view.update_in(cx, |view, window, cx| {
            view.project_work_dir = Some("/projects/other".into());
            view.selected_pr = Some(GitHubItemKey {
                project: PathBuf::from("/projects/other"),
                number: 42,
            });
            let other = pr_key("/projects/other", 42);
            view.pr_drafts
                .select_reply_target(other.clone(), reply_target("303", "carol"));
            view.pr_drafts.set_reply_body(&other, "other reply".into());
            view.sync_pr_draft_inputs(window, cx);
        });
        assert_eq!(
            view.read_with(cx, |view, cx| view.pr_reply_input.read(cx).value()),
            "other reply"
        );
        view.update_in(cx, |view, window, cx| {
            view.project_work_dir = Some("/projects/app".into());
            view.selected_pr = Some(GitHubItemKey {
                project: PathBuf::from("/projects/app"),
                number: 42,
            });
            view.sync_pr_draft_inputs(window, cx);
        });
        assert_eq!(
            view.read_with(cx, |view, cx| view.pr_reply_input.read(cx).value()),
            " first reply\n"
        );
    }

    #[test]
    fn github_result_matches_request_rejects_stale_project_tab_query_revision() {
        let current = GitHubRequest {
            scope: GitHubScope::Project(PathBuf::from("/projects/current")),
            tab: GitHubTab::Issues,
            query_revision: 4,
            item: Some(GitHubItemKey {
                project: PathBuf::from("/projects/current"),
                number: 12,
            }),
        };

        assert!(github_result_matches_request(&current, &current));
        for stale in [
            GitHubRequest {
                scope: GitHubScope::Project(PathBuf::from("/projects/old")),
                ..current.clone()
            },
            GitHubRequest {
                tab: GitHubTab::PullRequests,
                ..current.clone()
            },
            GitHubRequest {
                query_revision: 3,
                ..current.clone()
            },
            GitHubRequest {
                item: Some(GitHubItemKey {
                    project: PathBuf::from("/projects/current"),
                    number: 13,
                }),
                ..current.clone()
            },
            GitHubRequest {
                scope: GitHubScope::All,
                ..current.clone()
            },
        ] {
            assert!(!github_result_matches_request(&stale, &current));
        }
    }

    #[test]
    fn stale_detail_is_rejected_after_a_new_list_request() {
        let old_detail = GitHubRequest {
            scope: GitHubScope::Project(PathBuf::from("/projects/current")),
            tab: GitHubTab::Issues,
            query_revision: 4,
            item: Some(GitHubItemKey {
                project: PathBuf::from("/projects/current"),
                number: 42,
            }),
        };
        let new_list = GitHubRequest {
            query_revision: 5,
            item: None,
            ..old_detail.clone()
        };

        assert!(!detail_result_matches_list(
            &old_detail,
            &new_list,
            Some(GitHubItemKey {
                project: PathBuf::from("/projects/current"),
                number: 42,
            })
        ));
        assert!(!detail_result_matches_list(&old_detail, &new_list, None));
    }

    #[test]
    fn github_search_sends_plain_text_and_qualifiers_to_the_repository() {
        for query in [
            "linked task",
            "42",
            "label:desktop",
            "is:open linked",
            "-author:octocat",
        ] {
            assert_eq!(github_server_query(query), Some(query));
        }
        assert_eq!(github_server_query("  linked task  "), Some("linked task"));
        for query in ["", "  ", "\n"] {
            assert_eq!(github_server_query(query), None);
        }
        assert_eq!(
            github_empty_message(GitHubTab::Issues, GitHubStateFilter::Open, ""),
            "No open issues in this scope."
        );
        assert_eq!(
            github_empty_message(
                GitHubTab::PullRequests,
                GitHubStateFilter::Merged,
                "older fix"
            ),
            "No matching pull requests in this scope. Try another search or state filter."
        );
    }

    #[gpui::test]
    fn github_search_invalidates_old_results_before_debounce(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            GitHubView::new(model, window, cx)
        });
        view.update(cx, |view, cx| {
            configure_pr_workspace(view, cx);
            view.issue_limit = 100;
            view.pr_limit = 150;
            view.active_list_request = Some(GitHubRequest {
                scope: view.scope.clone(),
                tab: GitHubTab::PullRequests,
                query_revision: view.query_revision,
                item: None,
            });
            view.list_cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let previous_batch = view.list_cancelled.clone();
            view.query_revision += 1;
            view.schedule_query("older fix".into(), cx);
            assert!(previous_batch.load(std::sync::atomic::Ordering::Relaxed));
            assert!(view.active_list_request.is_none());
            assert!(view.list_loading);
            assert_eq!(view.issue_limit, super::PAGE_SIZE);
            assert_eq!(view.pr_limit, super::PAGE_SIZE);
            let revision = view.query_revision;
            view.load_more(cx);
            view.refresh(cx);
            assert_eq!(view.pr_limit, super::PAGE_SIZE);
            assert_eq!(view.query_revision, revision);
            view.debounce_task.take();
        });
    }

    #[test]
    fn selected_issue_survives_same_item_refresh() {
        assert_eq!(
            selected_issue_after_refresh(Some(42), &[issue(41), issue(42)]),
            Some(42)
        );
        assert_eq!(
            selected_issue_after_refresh(Some(42), &[issue(41)]),
            Some(41)
        );
        assert_eq!(selected_issue_after_refresh(Some(42), &[]), None);
    }

    #[test]
    fn scoped_issues_merge_sort_and_disambiguate_same_number() {
        let older = ScopedIssue {
            project: PathBuf::from("/projects/a"),
            project_name: "a".into(),
            summary: GitHubIssueSummary {
                updated_at: "2026-08-30T12:00:00Z".into(),
                ..issue(42)
            },
        };
        let newer = ScopedIssue {
            project: PathBuf::from("/projects/b"),
            project_name: "b".into(),
            summary: GitHubIssueSummary {
                updated_at: "2026-09-01T12:00:00Z".into(),
                ..issue(42)
            },
        };
        let merged = super::merge_scoped_issues(vec![older.clone(), newer.clone()], 10);
        assert_eq!(merged[0].project_name, "b");
        assert_eq!(merged[1].project_name, "a");
        let truncated = super::merge_scoped_issues(vec![older, newer], 1);
        assert_eq!(truncated.len(), 1);

        let rows = vec![ScopedIssue {
            project: PathBuf::from("/projects/b"),
            project_name: "b".into(),
            summary: issue(7),
        }];
        let selected = GitHubItemKey {
            project: PathBuf::from("/projects/b"),
            number: 7,
        };
        assert_eq!(
            super::selected_scoped_issue_after_refresh(Some(selected.clone()), &rows),
            Some(selected)
        );
        assert_eq!(
            super::selected_scoped_issue_after_refresh(
                Some(GitHubItemKey {
                    project: PathBuf::from("/projects/a"),
                    number: 7,
                }),
                &rows,
            ),
            Some(GitHubItemKey {
                project: PathBuf::from("/projects/b"),
                number: 7,
            })
        );
    }

    #[test]
    fn scoped_prs_merge_by_updated_desc() {
        let first = ScopedPr {
            project: PathBuf::from("/projects/a"),
            project_name: "a".into(),
            summary: GitHubPullRequestSummary {
                updated_at: "2026-08-30T12:00:00Z".into(),
                number: 1,
                ..Default::default()
            },
        };
        let second = ScopedPr {
            project: PathBuf::from("/projects/b"),
            project_name: "b".into(),
            summary: GitHubPullRequestSummary {
                updated_at: "2026-09-02T12:00:00Z".into(),
                number: 2,
                ..Default::default()
            },
        };
        let merged = super::merge_scoped_prs(vec![first, second.clone()], 10);
        assert_eq!(merged[0], second);
        assert_eq!(
            GitHubScope::All.label(&[
                ("a".into(), PathBuf::from("/projects/a")),
                ("b".into(), PathBuf::from("/projects/b")),
            ]),
            "All projects"
        );
        assert_eq!(
            GitHubScope::Project(PathBuf::from("/projects/b")).label(&[
                ("a".into(), PathBuf::from("/projects/a")),
                ("b".into(), PathBuf::from("/projects/b")),
            ]),
            "b"
        );
    }

    #[test]
    fn github_timestamps_render_relative_and_fall_back_to_raw() {
        // 2026-09-08T15:17:31Z == 1788880651.
        assert_eq!(
            super::parse_github_timestamp("2026-09-08T15:17:31Z"),
            Some(1788880651)
        );
        assert_eq!(super::parse_github_timestamp("not a time"), None);
        assert_eq!(
            super::format_github_time("2026-09-08T15:17:31Z", 1788880651 + 30),
            "just now"
        );
        assert_eq!(
            super::format_github_time("2026-09-08T15:17:31Z", 1788880651 + 5 * 3600),
            "5h ago"
        );
        assert_eq!(
            super::format_github_time("2026-09-08T15:17:31Z", 1788880651 + 3 * 86400),
            "3d ago"
        );
        assert_eq!(
            super::format_github_time("2026-07-25T04:42:52Z", 1788880651),
            "2026-07-25"
        );
        assert_eq!(super::format_github_time("garbage", 1788880651), "garbage");
    }

    #[test]
    fn scope_context_line_collapses_redundant_project() {
        assert_eq!(
            super::scope_context_line(
                "troescorpteam",
                "bom_price_management_system",
                "bom_price_management_system"
            ),
            "troescorpteam/bom_price_management_system"
        );
        assert_eq!(
            super::scope_context_line("wheregmis", "threadlane", "mypi"),
            "wheregmis/threadlane · mypi"
        );
    }

    #[test]
    fn linked_sessions_match_repository_qualified_issue_only() {
        let target = issue(42).issue;
        let other_repo = GitHubIssueRef {
            repo: "other".into(),
            ..target.clone()
        };
        let sessions = vec![
            session("match", Some(target.clone())),
            session("other-repo", Some(other_repo)),
            session(
                "other-number",
                Some(GitHubIssueRef {
                    number: 43,
                    ..target
                }),
            ),
            session("unlinked", None),
        ];

        assert_eq!(
            linked_session_ids(&sessions, &issue(42).issue),
            vec!["match"]
        );
    }

    #[test]
    fn linked_tasks_scan_all_projects_and_expose_live_status() {
        let target = issue(42).issue;
        let first_project = vec![session("unlinked", None)];
        let second_project = vec![session("match", Some(target.clone()))];

        assert_eq!(
            linked_sessions_across_projects(
                &[
                    ("first", first_project.as_slice()),
                    ("second", second_project.as_slice()),
                ],
                &target,
            )
            .into_iter()
            .map(|(project, session)| (project, session.id.as_str()))
            .collect::<Vec<_>>(),
            vec![("second", "match")]
        );

        let mut linked = second_project[0].clone();
        assert_eq!(linked_session_status(&linked, false, false), "Ready");
        linked.health = SessionHealth::Working;
        assert_eq!(linked_session_status(&linked, false, false), "Working");
        assert_eq!(
            linked_session_status(&linked, true, true),
            "Needs permission"
        );
        linked.health = SessionHealth::Warning;
        assert_eq!(
            linked_session_status(&linked, false, false),
            "Needs attention"
        );
        linked.worktree_available = false;
        assert_eq!(
            linked_session_status(&linked, false, false),
            "Not checked out"
        );
    }

    #[test]
    fn issue_start_confirmation_disables_non_git_projects_and_uses_a_safe_preview() {
        let issue = issue(42).issue;
        let confirmation = issue_start_confirmation(
            &issue,
            "Fix linked task browser",
            "gpt-5.6",
            "High",
            false,
            false,
        );

        assert!(!confirmation.start_enabled);
        assert_eq!(
            confirmation.start_disabled_reason.as_deref(),
            Some("This project is not a Git repository.")
        );
        assert_eq!(
            confirmation.branch_preview,
            "issue/42-fix-linked-task-browser-xxxxxx"
        );
        assert!(confirmation
            .copy
            .contains("pushes to origin"));
        assert!(confirmation.copy.contains("draft PR on GitHub"));
        assert_eq!(confirmation.start_label, "Start task");
        assert_eq!(
            confirmation.branch_disclosure,
            "A unique six-character suffix is assigned when the task starts."
        );
    }

    #[test]
    fn issue_start_confirmation_offers_open_and_another_for_linked_tasks() {
        let confirmation = issue_start_confirmation(
            &issue(42).issue,
            "Fix linked task browser",
            "gpt-5.6",
            "High",
            true,
            true,
        );

        assert!(confirmation.show_open_task);
        assert_eq!(
            confirmation.start_label,
            "Start another task"
        );
    }

    #[test]
    fn issue_start_activation_runs_only_when_enabled_and_reports_its_outcome() {
        assert_eq!(
            issue_start_activation(false, || -> Result<(), String> { panic!("must not start") }),
            Ok(false)
        );
        assert_eq!(issue_start_activation(true, || Ok(())), Ok(true));
        assert_eq!(
            issue_start_activation(true, || Err("worktree failed".into())),
            Err("worktree failed".into())
        );
    }

    #[test]
    fn disabled_issue_start_dialog_keeps_the_confirmation_open() {
        assert!(!issue_start_dialog_result(Ok(false), |_| {}));
    }

    #[test]
    fn linked_session_fingerprint_tracks_rendered_worktree_branch_and_pr_status() {
        let mut linked = session("linked", Some(issue(42).issue));
        linked.git_branch = Some("issue/42-fix-xxxxxx".into());
        let pr = GitHubPrInfo {
            number: 42,
            state: "OPEN".into(),
            head_ref: linked.git_branch.clone().unwrap(),
            base_ref: "main".into(),
            ..Default::default()
        };
        let first = linked_session_fingerprint(&linked, Some(&pr));

        linked.is_worktree = true;
        assert_ne!(first, linked_session_fingerprint(&linked, Some(&pr)));

        linked.is_worktree = false;
        linked.git_branch = Some("issue/42-other-xxxxxx".into());
        assert_ne!(first, linked_session_fingerprint(&linked, Some(&pr)));

        linked.git_branch = Some("issue/42-fix-xxxxxx".into());
        let closed = GitHubPrInfo {
            state: "CLOSED".into(),
            ..pr
        };
        assert_ne!(first, linked_session_fingerprint(&linked, Some(&closed)));
    }

    #[test]
    fn list_count_reconciliation_appends_without_resetting_the_scroll_anchor() {
        assert_eq!(list_count_splice(50, 101), Some((50..50, 51)));
        assert_eq!(list_count_splice(101, 40), Some((40..101, 0)));
        assert_eq!(list_count_splice(40, 40), None);
    }

    #[test]
    fn github_pr_timeline_keeps_remote_ids_and_stable_timestamp_ties() {
        let pr = GitHubPrInfo {
            url: "https://github.com/threadlane/app/pull/42".into(),
            issue_comments: vec![
                PrConversationComment {
                    remote_id: "issue-later".into(),
                    created_at: "2026-08-30T12:02:00Z".into(),
                    url: "https://github.com/threadlane/app/pull/42#issuecomment-2".into(),
                    ..Default::default()
                },
                PrConversationComment {
                    remote_id: "issue-tie".into(),
                    created_at: "2026-08-30T12:03:00Z".into(),
                    ..Default::default()
                },
            ],
            reviews: vec![
                PrReview {
                    remote_id: "review-first".into(),
                    state: "APPROVED".into(),
                    submitted_at: "2026-08-30T12:01:00Z".into(),
                    ..Default::default()
                },
                PrReview {
                    remote_id: "review-tie".into(),
                    state: "CHANGES_REQUESTED".into(),
                    submitted_at: "2026-08-30T12:03:00Z".into(),
                    ..Default::default()
                },
            ],
            review_comments: vec![PrReviewComment {
                remote_id: "inline-tie".into(),
                created_at: "2026-08-30T12:03:00Z".into(),
                path: Some("src/lib.rs".into()),
                line: Some(17),
                ..Default::default()
            }],
            ..Default::default()
        };

        let rows = merge_pr_timeline(&pr);

        assert_eq!(
            rows.iter()
                .map(|row| row.remote_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "review-first",
                "issue-later",
                "issue-tie",
                "review-tie",
                "inline-tie",
            ]
        );
        assert_eq!(rows[0].kind, PrTimelineKind::Review);
        assert_eq!(rows[0].label(), "Approved");
        assert_eq!(rows[3].label(), "Changes requested");
        assert_eq!(rows[4].kind, PrTimelineKind::InlineReviewComment);
        assert_eq!(rows[4].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(rows[4].line, Some(17));
        assert_eq!(rows[4].location().as_deref(), Some("src/lib.rs:17"));
        assert_eq!(rows[0].url, "https://github.com/threadlane/app/pull/42");
        assert_eq!(rows[2].url, "https://github.com/threadlane/app/pull/42");

        let review_prompt = draft_reply_prompt(&rows[0]);
        assert!(review_prompt.contains("Review state: Approved"));
        let inline_prompt = draft_reply_prompt(&rows[4]);
        assert!(inline_prompt.contains("Location: src/lib.rs:17"));
    }

    #[test]
    fn github_pr_timeline_preserves_every_review_state_in_labels_and_prompts() {
        let pr = GitHubPrInfo {
            url: "https://github.com/threadlane/app/pull/42".into(),
            reviews: ["COMMENTED", "DISMISSED", "pending", "NEEDS_TRIAGE"]
                .into_iter()
                .enumerate()
                .map(|(ix, state)| PrReview {
                    remote_id: format!("review-{ix}"),
                    state: state.into(),
                    submitted_at: format!("2026-08-30T12:0{ix}:00Z"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };

        let rows = merge_pr_timeline(&pr);
        for (row, expected) in
            rows.iter()
                .zip(["Commented", "Dismissed", "Pending", "Needs triage"])
        {
            assert_eq!(row.label(), expected);
            assert!(
                draft_reply_prompt(row).contains(&format!("Review state: {expected}")),
                "prompt omitted review state {expected}"
            );
        }
        assert_eq!(
            rows.iter()
                .map(|row| row.review_state.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["COMMENTED", "DISMISSED", "pending", "NEEDS_TRIAGE"]
        );
    }

    #[test]
    fn github_pr_link_fingerprint_tracks_branch_only_sessions_and_active_task() {
        let project = PathBuf::from("/projects/app");
        let mut first = session("first", None);
        first.git_branch = Some("feature/first".into());
        let mut second = session("second", None);
        second.git_branch = Some("feature/second".into());

        let fingerprint = |active: Option<&str>, first: &SessionInfo, second: &SessionInfo| {
            github_link_fingerprint_rows(
                active,
                [
                    ("App", project.as_path(), first, None, false, false),
                    ("App", project.as_path(), second, None, false, false),
                ],
            )
        };

        let baseline = fingerprint(Some("first"), &first, &second);
        first.git_branch = Some("feature/renamed".into());
        assert_ne!(baseline, fingerprint(Some("first"), &first, &second));
        first.git_branch = Some("feature/first".into());
        assert_ne!(baseline, fingerprint(Some("second"), &first, &second));
    }

    #[test]
    fn github_pr_check_rollup_matches_git_status_classification() {
        let check = |status: &str, conclusion: Option<&str>| PrCheckStatus {
            name: status.into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_owned),
            details_url: None,
        };

        assert_eq!(
            pr_check_label(&[
                check("PENDING", Some("PENDING")),
                check("EXPECTED", Some("EXPECTED")),
            ]),
            "2 pending"
        );
        assert_eq!(
            pr_check_label(&[check("COMPLETED", Some("SUCCESS"))]),
            "1 passing"
        );
        assert_eq!(
            pr_check_label(&[check("COMPLETED", Some("FAILURE"))]),
            "1 failing"
        );
    }

    #[gpui::test]
    fn github_pr_checks_expose_results_and_keyboard_accessible_logs(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            GitHubView::new(model, window, cx)
        });
        view.update(cx, |view, cx| {
            configure_pr_workspace(view, cx);
            let detail = view.pr_detail.as_mut().unwrap();
            detail.checks = vec![
                PrCheckStatus {
                    name: "Build".into(),
                    status: "COMPLETED".into(),
                    conclusion: Some("TIMED_OUT".into()),
                    details_url: Some("https://github.com/threadlane/app/actions/runs/123".into()),
                },
                PrCheckStatus {
                    name: "Lint".into(),
                    status: "IN_PROGRESS".into(),
                    conclusion: Some("".into()),
                    details_url: None,
                },
                PrCheckStatus {
                    name: "Untrusted".into(),
                    status: "COMPLETED".into(),
                    conclusion: Some("SUCCESS".into()),
                    details_url: Some("file:///private/tmp/check.log".into()),
                },
            ];
            detail.total_checks = 3;
            detail.failing_checks = 1;
            detail.pending_checks = 1;
            detail.passing_checks = 1;
            assert_eq!(super::pr_check_status_label(&detail.checks[0]), "Timed out");
            assert_eq!(
                super::pr_check_status_label(&detail.checks[1]),
                "In progress"
            );
            assert_eq!(
                super::pr_check_status_label(&PrCheckStatus::default()),
                "Unknown"
            );
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("github-pr-check-Lint").is_some());
        assert!(cx.debug_bounds("github-pr-check-log-Lint").is_none());
        assert!(cx.debug_bounds("github-pr-check-log-Untrusted").is_none());
        assert_eq!(
            cx.debug_bounds("github-pr-check-status-Build")
                .unwrap()
                .right(),
            cx.debug_bounds("github-pr-check-status-Lint")
                .unwrap()
                .right()
        );
        let log = cx.debug_bounds("github-pr-check-log-Build").unwrap();
        cx.simulate_click(log.center(), gpui::Modifiers::default());
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://github.com/threadlane/app/actions/runs/123")
        );

        cx.update(|window, cx| {
            assert!(window.focused(cx).is_some());
            cx.open_url("https://example.invalid/keyboard-test-marker");
            window.draw(cx).clear(cx);
        });
        let keystroke = gpui::Keystroke::parse("enter").unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://github.com/threadlane/app/actions/runs/123")
        );
    }

    #[test]
    fn github_pr_tab_arrows_and_file_actions_keep_bounded_selection() {
        assert_eq!(PrDetailTab::Summary.adjacent(1), PrDetailTab::Conversation);
        assert_eq!(PrDetailTab::Conversation.adjacent(1), PrDetailTab::Timeline);
        assert_eq!(PrDetailTab::Timeline.adjacent(1), PrDetailTab::Code);
        assert_eq!(PrDetailTab::Code.adjacent(1), PrDetailTab::Code);
        assert_eq!(PrDetailTab::Code.adjacent(-1), PrDetailTab::Timeline);

        assert_eq!(
            pr_file_action_ix(Some(1), 3, PrFileAction::Previous),
            Some(0)
        );
        assert_eq!(pr_file_action_ix(Some(1), 3, PrFileAction::Next), Some(2));
        assert_eq!(pr_file_action_ix(Some(2), 3, PrFileAction::Next), Some(2));
        assert_eq!(pr_file_action_ix(Some(1), 3, PrFileAction::Open), Some(1));
        assert_eq!(pr_file_action_ix(None, 0, PrFileAction::Open), None);
    }

    #[gpui::test]
    fn github_pr_keyboard_actions_follow_rendered_focus_contexts(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            GitHubView::new(model, window, cx)
        });
        view.update(cx, configure_pr_workspace);
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let tabs_focus = view.read_with(cx, |view, _| view.pr_tabs_focus.clone());
        let list_focus = view.read_with(cx, |view, _| view.list_focus.clone());
        cx.update(|window, cx| {
            window.focus(&list_focus, cx);
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("up enter");
        cx.update(|window, cx| {
            assert!(tabs_focus.is_focused(window));
            window.draw(cx).clear(cx);
        });
        assert!(view.read_with(cx, |view, _| view.active_detail_request.is_none()));
        cx.simulate_keystrokes("right");
        assert_eq!(
            view.read_with(cx, |view, _| view.current_pr_tab()),
            PrDetailTab::Conversation
        );

        view.update(cx, |view, cx| {
            let key = view.current_pr_key().unwrap();
            view.pr_selections.select_tab(key, PrDetailTab::Code);
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let files_focus = view.read_with(cx, |view, _| view.pr_file_focus.clone());
        cx.update(|window, cx| {
            window.focus(&files_focus, cx);
            window.draw(cx).clear(cx);
        });
        cx.simulate_keystrokes("down");
        assert_eq!(
            view.read_with(cx, |view, _| view.current_pr_file().map(str::to_owned)),
            Some("src/view.rs".into())
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("github-result-list").is_none(),
            "File review should use the work area rather than squeeze in a third column");
        let back = cx.debug_bounds("github-back-to-pr-list").unwrap();
        cx.simulate_click(back.center(), gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(view.read_with(cx, |view, _| view.current_pr_tab()), PrDetailTab::Summary);
        assert!(cx.debug_bounds("github-result-list").is_some());
        assert_eq!(view.read_with(cx, |view, _| view.current_pr_file().map(str::to_owned)),
            Some("src/view.rs".into()), "Returning to the list retains the chosen file");
        view.update(cx, |view, cx| {
            view.detail_loading = true;
            view.select_ix(0, cx);
            assert!(
                view.active_detail_request.is_none(),
                "An in-flight detail must not be restarted"
            );
            view.detail_loading = false;
            view.detail_error = Some("Check your connection".into());
            assert!(
                !view.selected_detail_is_loaded(),
                "A failed detail remains retryable"
            );
        });
    }

    #[gpui::test]
    fn github_pr_same_file_reload_preserves_retained_diff_state(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            GitHubView::new(model, window, cx)
        });
        let original_body = view.update(cx, |view, cx| {
            configure_pr_workspace(view, cx);
            view.pr_diff_body
                .update(cx, |body, cx| body.set_text("retained diff", cx));
            view.pr_diff_body.update(cx, |body, cx| body.select_all(cx));
            view.pr_diff_body.clone()
        });
        assert_eq!(
            original_body.read_with(cx, |body, _| body.selected_text()),
            "retained diff\n"
        );

        view.update(cx, |view, cx| view.select_pr_file("src/lib.rs".into(), cx));
        assert_eq!(
            view.read_with(cx, |view, _| view.pr_diff_body.entity_id()),
            original_body.entity_id()
        );
        assert_eq!(
            original_body.read_with(cx, |body, _| body.selected_text()),
            "retained diff\n",
            "same-file refresh must not clear retained text or its view state"
        );

        view.update(cx, |view, cx| view.select_pr_file("src/view.rs".into(), cx));
        assert_ne!(
            view.read_with(cx, |view, _| view.pr_diff_body.entity_id()),
            original_body.entity_id(),
            "a new selected-file identity must reset the retained TextView"
        );
    }

    #[test]
    fn github_pr_diff_rejects_stale_project_pr_path_and_revision() {
        let current = PrDiffRequest {
            key: PrWorkspaceKey {
                project: PathBuf::from("/projects/current"),
                number: 42,
            },
            path: "src/lib.rs".into(),
            revision: 7,
        };

        assert!(pr_diff_result_matches_request(
            &current,
            Some(&current),
            Some(&current.key),
            Some("src/lib.rs"),
        ));
        for stale in [
            PrDiffRequest {
                key: PrWorkspaceKey {
                    project: PathBuf::from("/projects/old"),
                    ..current.key.clone()
                },
                ..current.clone()
            },
            PrDiffRequest {
                key: PrWorkspaceKey {
                    number: 43,
                    ..current.key.clone()
                },
                ..current.clone()
            },
            PrDiffRequest {
                path: "src/other.rs".into(),
                ..current.clone()
            },
            PrDiffRequest {
                revision: 6,
                ..current.clone()
            },
        ] {
            assert!(!pr_diff_result_matches_request(
                &stale,
                Some(&current),
                Some(&current.key),
                Some("src/lib.rs"),
            ));
        }
        assert!(!pr_diff_result_matches_request(
            &current,
            Some(&current),
            Some(&current.key),
            Some("src/other.rs"),
        ));
    }

    #[test]
    fn github_pr_tabs_and_files_are_retained_per_pull_request() {
        let first = PrWorkspaceKey {
            project: PathBuf::from("/projects/app"),
            number: 41,
        };
        let second = PrWorkspaceKey {
            number: 42,
            ..first.clone()
        };
        let files = vec![
            GitHubPrFile {
                path: "src/lib.rs".into(),
                ..Default::default()
            },
            GitHubPrFile {
                path: "src/view.rs".into(),
                ..Default::default()
            },
        ];
        let mut selections = PrWorkspaceSelections::default();

        selections.select_tab(first.clone(), PrDetailTab::Code);
        selections.reconcile_files(&first, &files);
        selections.select_file(first.clone(), "src/view.rs".into());
        selections.select_tab(second.clone(), PrDetailTab::Timeline);
        selections.reconcile_files(&second, &files[..1]);

        assert_eq!(selections.tab(&first), PrDetailTab::Code);
        assert_eq!(selections.selected_file(&first), Some("src/view.rs"));
        assert_eq!(selections.tab(&second), PrDetailTab::Timeline);
        assert_eq!(selections.selected_file(&second), Some("src/lib.rs"));
        selections.reconcile_files(&first, &files[..1]);
        assert_eq!(selections.selected_file(&first), Some("src/lib.rs"));
    }

    #[test]
    fn github_pr_reply_prompt_is_bounded_and_keeps_the_publish_boundary() {
        let instruction = "Return an editable reply draft; do not publish it.";
        let prompt = draft_reply_prompt(&super::PrTimelineRow {
            remote_id: "7".into(),
            kind: PrTimelineKind::InlineReviewComment,
            author: "reviewer".into(),
            body: "context ".repeat(1_000),
            timestamp: "2026-08-30T12:03:00Z".into(),
            url: "https://github.com/threadlane/app/pull/42#discussion_r7".into(),
            review_state: None,
            path: Some("src/lib.rs".into()),
            line: Some(7),
            in_reply_to_id: None,
        });

        assert!(prompt.contains("https://github.com/threadlane/app/pull/42#discussion_r7"));
        assert!(prompt.contains("> context"));
        assert_eq!(prompt.matches(instruction).count(), 1);
        assert!(prompt.chars().count() < 1_700);
    }

    #[test]
    fn github_pr_selected_diff_contains_only_the_requested_file() {
        let raw = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n+first\n+diff --git a/not-a-header b/not-a-header\n trailing context\ndiff --git a/src/view.rs b/src/view.rs\n--- a/src/view.rs\n+++ b/src/view.rs\n+second\n";

        let selected = selected_file_diff(raw, "src/view.rs").expect("selected diff");

        assert!(selected.contains("src/view.rs"));
        assert!(selected.contains("+second"));
        assert!(!selected.contains("src/lib.rs"));
        assert!(!selected.contains("+first"));
        assert!(selected_file_diff(raw, "src/missing.rs").is_none());

        let first = selected_file_diff(raw, "src/lib.rs").expect("first diff");
        assert!(first.contains("+diff --git a/not-a-header b/not-a-header"));
        assert!(first.contains(" trailing context"));
    }

    #[test]
    fn github_pr_diff_handles_git_quoted_unicode_rename_and_binary_sections() {
        let raw = concat!(
            "diff --git \"a/src/caf\\303\\251 file.rs\" \"b/src/caf\\303\\251 file.rs\"\n",
            "--- \"a/src/caf\\303\\251 file.rs\"\n",
            "+++ \"b/src/caf\\303\\251 file.rs\"\n",
            "+unicode\n",
            "diff --git \"a/old name.bin\" \"b/new name.bin\"\n",
            "similarity index 100%\n",
            "rename from old name.bin\n",
            "rename to new name.bin\n",
            "Binary files a/old name.bin and b/new name.bin differ\n",
        );

        let unicode = selected_file_diff(raw, "src/café file.rs").expect("unicode diff");
        assert!(unicode.starts_with("diff --git \"a/src/caf\\303\\251 file.rs\""));
        assert!(unicode.ends_with("+unicode\n"));
        let renamed = selected_file_diff(raw, "new name.bin").expect("renamed binary diff");
        assert!(renamed.contains("rename from old name.bin"));
        assert!(renamed.ends_with("Binary files a/old name.bin and b/new name.bin differ\n"));
    }

    #[test]
    fn github_pr_diff_preparation_preserves_patch_bytes_and_uses_a_safe_fence() {
        let raw = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n+let ticks = ````;\n";
        let selected = selected_file_diff(raw, "src/lib.rs").expect("selected diff");

        let prepared = prepare_selected_diff(raw, "src/lib.rs").expect("prepared diff");

        assert_eq!(selected, raw);
        assert!(prepared.starts_with("`````diff\n"));
        assert!(prepared.contains(raw));
        assert!(prepared.ends_with("`````"));
    }

    #[test]
    fn github_pr_file_identity_changes_only_for_a_different_path() {
        let key = PrWorkspaceKey {
            project: PathBuf::from("/projects/app"),
            number: 42,
        };
        let mut selections = PrWorkspaceSelections::default();

        assert!(selections.select_file(key.clone(), "src/lib.rs".into()));
        assert!(!selections.select_file(key.clone(), "src/lib.rs".into()));
        assert!(selections.select_file(key, "src/view.rs".into()));
    }

    #[gpui::test]
    fn github_pr_timeline_and_issue_comments_render_markdown(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            GitHubView::new(model, window, cx)
        });
        view.update(cx, |view, cx| {
            configure_pr_workspace(view, cx);
            view.pr_timeline_rows = vec![
                super::PrTimelineRow {
                    remote_id: "comment-1".into(),
                    kind: super::PrTimelineKind::IssueComment,
                    author: "alice".into(),
                    body: "## Performance report\n[workflow](https://github.com)".into(),
                    timestamp: "2026-09-02T15:00:00Z".into(),
                    url: "https://github.com".into(),
                    review_state: None,
                    path: None,
                    line: None,
                    in_reply_to_id: None,
                },
                super::PrTimelineRow {
                    remote_id: "comment-empty".into(),
                    kind: super::PrTimelineKind::Review,
                    author: "bob".into(),
                    body: "".into(),
                    timestamp: "2026-09-02T15:01:00Z".into(),
                    url: "https://github.com".into(),
                    review_state: Some("APPROVED".into()),
                    path: None,
                    line: None,
                    in_reply_to_id: None,
                },
            ];
            view.pr_timeline_list_state
                .reset(view.pr_timeline_rows.len());
            let key = view.current_pr_key().unwrap();
            view.pr_selections.select_tab(key, PrDetailTab::Summary);
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("github-pr-conversation-comments").is_none(),
            "Overview must not duplicate the conversation");
        view.update(cx, |view, cx| {
            let key = view.current_pr_key().unwrap();
            view.pr_selections.select_tab(key, PrDetailTab::Conversation);
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("github-pr-conversation-comments")
                .is_some_and(|bounds| bounds.size.height > gpui::px(0.)),
            "Conversation comments should be rendered in the visible scroll flow"
        );

        view.update(cx, |view, cx| {
            view.tab = GitHubTab::Issues;
            view.model.update(cx, |state, _| state.github_tab = GitHubTab::Issues);
            view.selected_issue = Some(GitHubItemKey {
                project: PathBuf::from("/projects/app"),
                number: 1,
            });
            view.issue_detail = Some(threadlane_git::GitHubIssueDetail {
                summary: issue(1),
                body: "Body".into(),
                ..Default::default()
            });
            view.comment_rows = vec![
                (
                    "alice".into(),
                    "10m ago".into(),
                    "### Issue note\nwith `code`".into(),
                ),
                ("bob".into(), "5m ago".into(), "".into()),
            ];
            view.comment_list_state.reset(view.comment_rows.len());
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn pr_viewed_key() -> PrWorkspaceKey {
        PrWorkspaceKey {
            project: PathBuf::from("/projects/app"),
            number: 42,
        }
    }

    fn viewed_fixture(files: &[(&str, PrFileViewedStatus)]) -> GitHubPrViewedState {
        GitHubPrViewedState {
            pull_request_id: "PR-node-1".into(),
            head_oid: "head-42".into(),
            viewer: "octocat".into(),
            files: files
                .iter()
                .map(|(path, status)| GitHubPrFileViewed {
                    path: (*path).into(),
                    status: *status,
                })
                .collect(),
            complete: true,
        }
    }

    fn viewed_snapshot(files: &[(&str, PrFileViewedStatus)]) -> PrViewedSnapshot {
        let state = viewed_fixture(files);
        PrViewedSnapshot {
            pull_request_id: state.pull_request_id,
            head_oid: state.head_oid,
            viewer: state.viewer,
            files: state
                .files
                .into_iter()
                .map(|file| (file.path, file.status))
                .collect(),
            complete: state.complete,
        }
    }

    /// Transport with test-driven reads/writes: every call announces itself on
    /// a request channel, then blocks until the test sends the response.
    /// Requests are observed with `recv()` (wait until issued) and
    /// `try_recv()` (assert none issued after `run_until_parked`).
    fn fake_viewed_transport() -> (
        PrViewedTransport,
        Receiver<String>,
        Receiver<(String, String, bool)>,
        Sender<Result<GitHubPrViewedState, String>>,
        Sender<Result<(), String>>,
    ) {
        let (read_resp_tx, read_resp_rx) = channel::<Result<GitHubPrViewedState, String>>();
        let (write_resp_tx, write_resp_rx) = channel::<Result<(), String>>();
        let (read_req_tx, read_req_rx) = channel::<String>();
        let (write_req_tx, write_req_rx) = channel::<(String, String, bool)>();
        let read_resp_rx = Mutex::new(read_resp_rx);
        let write_resp_rx = Mutex::new(write_resp_rx);
        let transport = PrViewedTransport {
            read: Arc::new(move |_dir, url| {
                read_req_tx.send(url).unwrap();
                read_resp_rx
                    .lock()
                    .unwrap()
                    .recv()
                    .unwrap_or_else(|_| Err("read dropped".to_owned()))
            }),
            write: Arc::new(move |_dir, _url, pull_request_id, path, viewed| {
                write_req_tx.send((pull_request_id, path, viewed)).unwrap();
                write_resp_rx
                    .lock()
                    .unwrap()
                    .recv()
                    .unwrap_or_else(|_| Err("write dropped".to_owned()))
            }),
        };
        (transport, read_req_rx, write_req_rx, read_resp_tx, write_resp_tx)
    }

    #[test]
    fn viewed_state_lifecycle_guards_writes_and_tokens() {
        let key = pr_viewed_key();
        let mut states = PrViewedStates::default();
        // Nothing may be written without a confirmed snapshot.
        assert!(states.begin_write(&key, "src/lib.rs".into(), true).is_none());
        let first = states.begin_read(&key);
        assert!(states.fail_read(&key, first, "offline".into()));
        assert_eq!(states.get(&key).unwrap().error.as_deref(), Some("offline"));

        let second = states.begin_read(&key);
        // A stale completion for the older attempt is dropped.
        assert!(!states.complete_read(&key, first, viewed_snapshot(&[])));
        assert!(states.complete_read(
            &key,
            second,
            viewed_snapshot(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Unviewed),
            ])
        ));
        let state = states.get(&key).unwrap();
        assert!(state.error.is_none());
        assert!(!state.loading);
        assert_eq!(state.marker_label("src/lib.rs"), Some("Viewed"));
        assert_eq!(state.marker_label("src/view.rs"), None);

        // Writes require the observed head to match the snapshot's head.
        assert!(states.begin_write(&key, "src/view.rs".into(), true).is_none());
        states.observe_head(&key, "head-other");
        assert!(states.begin_write(&key, "src/view.rs".into(), true).is_none());
        states.observe_head(&key, "head-42");
        let write = states.begin_write(&key, "src/view.rs".into(), true).unwrap();
        // Repeat activation is blocked while a write is pending, and a
        // refresh cannot race the unconfirmed mutation.
        assert!(states.begin_write(&key, "src/view.rs".into(), true).is_none());
        assert!(!states.refresh_allowed(&key));
        assert_eq!(
            states.get(&key).unwrap().marker_label("src/view.rs"),
            Some("Saving…")
        );
        assert!(states.complete_write(&key, write, Ok(())));
        assert!(states.refresh_allowed(&key));

        // A confirmed write is still pending: no rewrites until its readback
        // settles, and a failed readback is awaiting verification — not rolled
        // back to the pre-write marker.
        assert!(states.begin_write(&key, "src/lib.rs".into(), false).is_none());
        let readback = states.begin_read(&key);
        assert!(states.fail_read(&key, readback, "readback offline".into()));
        let state = states.get(&key).unwrap();
        assert_eq!(state.error.as_deref(), Some("readback offline"));
        assert_eq!(
            state.marker_label("src/view.rs"),
            Some("Couldn't confirm — refresh to settle")
        );
        let settle = states.begin_read(&key);
        assert!(states.complete_read(
            &key,
            settle,
            viewed_snapshot(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Viewed),
            ])
        ));
        assert!(states.get(&key).unwrap().pending_write.is_none());

        // An unconfirmed write stays blocked until a refresh settles it.
        let write = states.begin_write(&key, "src/lib.rs".into(), false).unwrap();
        assert!(states.complete_write(&key, write, Err("timed out".into())));
        let state = states.get(&key).unwrap();
        assert_eq!(state.error.as_deref(), Some("timed out"));
        assert_eq!(
            state.marker_label("src/lib.rs"),
            Some("Couldn't confirm — refresh to settle")
        );
        assert!(states.begin_write(&key, "src/lib.rs".into(), false).is_none());
        assert!(states.refresh_allowed(&key));
        let readback = states.begin_read(&key);
        assert!(states.complete_read(
            &key,
            readback,
            viewed_snapshot(&[
                ("src/lib.rs", PrFileViewedStatus::Unviewed),
                ("src/view.rs", PrFileViewedStatus::Viewed),
            ])
        ));
        let state = states.get(&key).unwrap();
        assert!(state.pending_write.is_none());
        assert!(state.error.is_none());
        assert_eq!(state.marker_label("src/view.rs"), Some("Viewed"));
        assert!(state.write_allowed("src/lib.rs"));
    }

    #[test]
    fn next_unviewed_file_wraps_once_and_skips_viewed() {
        let files = vec![
            GitHubPrFile {
                path: "a.rs".into(),
                ..Default::default()
            },
            GitHubPrFile {
                path: "b.rs".into(),
                ..Default::default()
            },
            GitHubPrFile {
                path: "dir/c.rs".into(),
                ..Default::default()
            },
        ];
        let snap = viewed_snapshot(&[
            ("a.rs", PrFileViewedStatus::Viewed),
            ("b.rs", PrFileViewedStatus::Unviewed),
            ("dir/c.rs", PrFileViewedStatus::ChangedSinceViewed),
        ]);
        assert_eq!(next_unviewed_file(Some(&snap), &files, Some("a.rs")), Some("b.rs"));
        // Wraps once past the end and skips viewed files.
        assert_eq!(next_unviewed_file(Some(&snap), &files, Some("dir/c.rs")), Some("b.rs"));
        // Never returns the current file.
        assert_eq!(next_unviewed_file(Some(&snap), &files, Some("b.rs")), Some("dir/c.rs"));
        // No snapshot → nothing is known-unviewed.
        assert_eq!(next_unviewed_file(None, &files, Some("a.rs")), None);
        let all_viewed = viewed_snapshot(&[
            ("a.rs", PrFileViewedStatus::Viewed),
            ("b.rs", PrFileViewedStatus::Viewed),
            ("dir/c.rs", PrFileViewedStatus::Viewed),
        ]);
        assert_eq!(next_unviewed_file(Some(&all_viewed), &files, Some("a.rs")), None);
        // A renamed basename does not inherit another path's marker.
        let renamed = viewed_snapshot(&[("dir/c.rs", PrFileViewedStatus::Viewed)]);
        let renamed_files = vec![
            GitHubPrFile {
                path: "other/c.rs".into(),
                ..Default::default()
            },
            GitHubPrFile {
                path: "d.rs".into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            next_unviewed_file(Some(&renamed), &renamed_files, Some("d.rs")),
            Some("other/c.rs")
        );

        let (viewed, total, all) = pr_viewed_progress(Some(&snap), &files);
        assert_eq!((viewed, total, all), (1, 3, false));
        let (viewed, total, all) = pr_viewed_progress(Some(&all_viewed), &files);
        assert_eq!((viewed, total, all), (3, 3, true));
        assert_eq!(pr_viewed_progress(None, &files), (0, 3, false));
        // A file missing from the snapshot is not confirmed viewed.
        let partial = viewed_snapshot(&[
            ("a.rs", PrFileViewedStatus::Viewed),
            ("b.rs", PrFileViewedStatus::Viewed),
        ]);
        assert_eq!(pr_viewed_progress(Some(&partial), &files), (2, 3, false));
    }

    /// Reads and writes run on the background executor during
    /// `run_until_parked`, so responses are queued first and requests are
    /// observed afterwards with `try_recv`; a wrongly-issued call blocks and
    /// fails via timeout instead of passing silently.
    #[gpui::test]
    fn github_pr_viewed_markers_settle_through_readback(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let key = pr_viewed_key();
        let (transport, read_req, write_req, read_resp, write_resp) = fake_viewed_transport();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            let mut view = GitHubView::new(model, window, cx);
            view.viewed_transport = transport;
            configure_pr_workspace(&mut view, cx);
            view.pr_viewed.observe_head(&key, "head-42");
            view
        });

        // Entering the Code tab queues one read: loading, no false zero.
        view.update(cx, |view, cx| view.select_pr_tab(PrDetailTab::Code, cx));
        view.read_with(cx, |view, _| {
            let state = view.pr_viewed.get(&key).unwrap();
            assert!(state.loading);
            assert!(state.snapshot.is_none());
        });
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Unviewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            read_req.try_recv().unwrap(),
            "https://github.com/threadlane/app/pull/42"
        );
        view.read_with(cx, |view, _| {
            let state = view.pr_viewed.get(&key).unwrap();
            assert!(!state.loading);
            let snap = state.snapshot.as_ref().unwrap();
            assert_eq!(snap.viewer, "octocat");
            assert_eq!(snap.status("src/lib.rs"), PrFileViewedStatus::Viewed);
            assert!(state.write_allowed("src/view.rs"));
        });

        // Check writes one mutation for the signed-in account's marker, with a
        // visible pending label until the readback settles.
        view.update(cx, |view, cx| {
            view.set_pr_file_viewed("src/view.rs".into(), true, cx)
        });
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.pr_viewed.get(&key).unwrap().marker_label("src/view.rs"),
                Some("Saving…")
            );
        });
        // Repeat activation is blocked while the write is pending.
        view.update(cx, |view, cx| {
            view.set_pr_file_viewed("src/lib.rs".into(), false, cx)
        });
        // A confirmed write triggers readback; the confirmed count only
        // changes once the fresh snapshot lands.
        write_resp.send(Ok(())).unwrap();
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Viewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            write_req.try_recv().unwrap(),
            ("PR-node-1".into(), "src/view.rs".into(), true)
        );
        assert!(write_req.try_recv().is_err());
        assert_eq!(
            read_req.try_recv().unwrap(),
            "https://github.com/threadlane/app/pull/42"
        );
        view.read_with(cx, |view, _| {
            let state = view.pr_viewed.get(&key).unwrap();
            assert!(!state.loading);
            assert!(state.error.is_none());
            assert!(state.pending_write.is_none());
            assert_eq!(
                state.snapshot.as_ref().unwrap().status("src/view.rs"),
                PrFileViewedStatus::Viewed
            );
            assert_eq!(state.marker_label("src/view.rs"), Some("Viewed"));
        });
    }

    #[gpui::test]
    fn github_pr_viewed_uncertain_write_blocks_until_refresh(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let key = pr_viewed_key();
        let (transport, read_req, write_req, read_resp, write_resp) = fake_viewed_transport();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            let mut view = GitHubView::new(model, window, cx);
            view.viewed_transport = transport;
            configure_pr_workspace(&mut view, cx);
            view.pr_viewed.observe_head(&key, "head-42");
            view
        });
        view.update(cx, |view, cx| view.select_pr_tab(PrDetailTab::Code, cx));
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Unviewed),
                ("src/view.rs", PrFileViewedStatus::Unviewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        read_req.try_recv().unwrap();

        view.update(cx, |view, cx| {
            view.set_pr_file_viewed("src/lib.rs".into(), true, cx)
        });
        write_resp.send(Err("request timed out".into())).unwrap();
        cx.run_until_parked();
        assert_eq!(
            write_req.try_recv().unwrap(),
            ("PR-node-1".into(), "src/lib.rs".into(), true)
        );
        view.read_with(cx, |view, _| {
            let state = view.pr_viewed.get(&key).unwrap();
            assert_eq!(state.error.as_deref(), Some("request timed out"));
            assert_eq!(
                state.marker_label("src/lib.rs"),
                Some("Couldn't confirm — refresh to settle")
            );
            // The snapshot still reports the last confirmed state, not a guess.
            assert_eq!(
                state.snapshot.as_ref().unwrap().status("src/lib.rs"),
                PrFileViewedStatus::Unviewed
            );
        });
        // The uncertain outcome blocks another write until a refresh settles.
        view.update(cx, |view, cx| {
            view.set_pr_file_viewed("src/lib.rs".into(), true, cx)
        });
        cx.run_until_parked();
        assert!(write_req.try_recv().is_err());
        // Refresh settles it.
        view.update(cx, |view, cx| view.refresh_pr_viewed(cx));
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Unviewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        read_req.try_recv().unwrap();
        view.read_with(cx, |view, _| {
            let state = view.pr_viewed.get(&key).unwrap();
            assert!(state.error.is_none());
            assert!(state.pending_write.is_none());
            assert_eq!(
                state.snapshot.as_ref().unwrap().status("src/lib.rs"),
                PrFileViewedStatus::Viewed
            );
            assert!(state.write_allowed("src/lib.rs"));
        });
    }

    #[gpui::test]
    fn github_pr_viewed_failed_readback_stays_uncertain(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let key = pr_viewed_key();
        let (transport, read_req, write_req, read_resp, write_resp) = fake_viewed_transport();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            let mut view = GitHubView::new(model, window, cx);
            view.viewed_transport = transport;
            configure_pr_workspace(&mut view, cx);
            view.pr_viewed.observe_head(&key, "head-42");
            view
        });
        view.update(cx, |view, cx| view.select_pr_tab(PrDetailTab::Code, cx));
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Unviewed),
                ("src/view.rs", PrFileViewedStatus::Unviewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        read_req.try_recv().unwrap();

        // The write is confirmed, but its readback fails: the marker is
        // awaiting verification — blocked, never rolled back to pre-write.
        view.update(cx, |view, cx| {
            view.set_pr_file_viewed("src/lib.rs".into(), true, cx)
        });
        write_resp.send(Ok(())).unwrap();
        read_resp.send(Err("connection reset".into())).unwrap();
        cx.run_until_parked();
        assert_eq!(
            write_req.try_recv().unwrap(),
            ("PR-node-1".into(), "src/lib.rs".into(), true)
        );
        read_req.try_recv().unwrap();
        view.read_with(cx, |view, _| {
            let state = view.pr_viewed.get(&key).unwrap();
            assert!(!state.loading);
            assert_eq!(state.error.as_deref(), Some("connection reset"));
            assert!(state.pending_write.as_ref().unwrap().uncertain);
            assert_eq!(
                state.marker_label("src/lib.rs"),
                Some("Couldn't confirm — refresh to settle")
            );
            assert!(!state.write_allowed("src/lib.rs"));
            assert_eq!(
                state.snapshot.as_ref().unwrap().status("src/lib.rs"),
                PrFileViewedStatus::Unviewed
            );
        });
        view.update(cx, |view, cx| {
            view.set_pr_file_viewed("src/lib.rs".into(), true, cx)
        });
        cx.run_until_parked();
        assert!(write_req.try_recv().is_err());
        // A refresh settles it.
        view.update(cx, |view, cx| view.refresh_pr_viewed(cx));
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Unviewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        read_req.try_recv().unwrap();
        view.read_with(cx, |view, _| {
            let state = view.pr_viewed.get(&key).unwrap();
            assert!(state.pending_write.is_none());
            assert!(state.error.is_none());
            assert_eq!(
                state.snapshot.as_ref().unwrap().status("src/lib.rs"),
                PrFileViewedStatus::Viewed
            );
        });
    }

    #[gpui::test]
    fn github_pr_viewed_superseded_reads_and_blocked_refresh(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let key = pr_viewed_key();
        let (transport, read_req, write_req, read_resp, write_resp) = fake_viewed_transport();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            let mut view = GitHubView::new(model, window, cx);
            view.viewed_transport = transport;
            configure_pr_workspace(&mut view, cx);
            view.pr_viewed.observe_head(&key, "head-42");
            view
        });
        // Two reads race: the refresh supersedes the entry read, so the
        // superseded response is dropped even though it arrives first.
        view.update(cx, |view, cx| view.select_pr_tab(PrDetailTab::Code, cx));
        view.update(cx, |view, cx| view.refresh_pr_viewed(cx));
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Viewed),
            ])))
            .unwrap();
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Unviewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        read_req.try_recv().unwrap();
        read_req.try_recv().unwrap();
        view.read_with(cx, |view, _| {
            let state = view.pr_viewed.get(&key).unwrap();
            assert!(!state.loading);
            assert_eq!(
                state.snapshot.as_ref().unwrap().status("src/view.rs"),
                PrFileViewedStatus::Unviewed
            );
        });

        // An in-flight write blocks refreshes until its readback.
        view.update(cx, |view, cx| {
            view.set_pr_file_viewed("src/view.rs".into(), true, cx)
        });
        view.update(cx, |view, cx| view.refresh_pr_viewed(cx));
        write_resp.send(Ok(())).unwrap();
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Viewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        write_req.try_recv().unwrap();
        // Exactly one more read — the write's own readback, never the
        // refresh refused while the write was pending.
        read_req.try_recv().unwrap();
        assert!(read_req.try_recv().is_err());
        view.read_with(cx, |view, _| {
            let state = view.pr_viewed.get(&key).unwrap();
            assert!(!state.loading);
            assert_eq!(
                state.snapshot.as_ref().unwrap().status("src/view.rs"),
                PrFileViewedStatus::Viewed
            );
        });
    }

    #[gpui::test]
    fn github_pr_viewed_next_unviewed_selects_and_skips(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let key = pr_viewed_key();
        let (transport, read_req, write_req, read_resp, _write_resp) =
            fake_viewed_transport();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            let mut view = GitHubView::new(model, window, cx);
            view.viewed_transport = transport;
            configure_pr_workspace(&mut view, cx);
            view
        });
        view.update(cx, |view, cx| view.select_pr_tab(PrDetailTab::Code, cx));
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Unviewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        read_req.try_recv().unwrap();
        // From viewed lib.rs, Next unviewed selects view.rs — without marking.
        view.update(cx, |view, cx| view.select_next_unviewed(cx));
        cx.run_until_parked();
        assert!(write_req.try_recv().is_err());
        view.read_with(cx, |view, _| {
            assert_eq!(view.current_pr_file(), Some("src/view.rs"));
            assert_eq!(
                view.pr_viewed
                    .get(&key)
                    .unwrap()
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .status("src/view.rs"),
                PrFileViewedStatus::Unviewed
            );
        });
        // Everything confirmed viewed: no other target exists.
        view.update(cx, |view, cx| view.refresh_pr_viewed(cx));
        read_resp
            .send(Ok(viewed_fixture(&[
                ("src/lib.rs", PrFileViewedStatus::Viewed),
                ("src/view.rs", PrFileViewedStatus::Viewed),
            ])))
            .unwrap();
        cx.run_until_parked();
        read_req.try_recv().unwrap();
        view.update(cx, |view, cx| view.select_next_unviewed(cx));
        view.read_with(cx, |view, _| {
            assert_eq!(view.current_pr_file(), Some("src/view.rs"));
        });
    }

    #[gpui::test]
    fn github_pr_viewed_results_stay_under_their_pr_key(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            super::init(cx);
        });
        let key = pr_viewed_key();
        let (transport, read_req, _write_req, read_resp, _write_resp) =
            fake_viewed_transport();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let model = cx.new(|_| AppState::default());
            let mut view = GitHubView::new(model, window, cx);
            view.viewed_transport = transport;
            configure_pr_workspace(&mut view, cx);
            view
        });
        view.update(cx, |view, cx| view.select_pr_tab(PrDetailTab::Code, cx));

        // Selection moves to a same-numbered PR in another repository before
        // the delayed read lands; the still-displayed detail belongs to the
        // first repo, so the URL guard refuses a read for the new selection.
        view.update(cx, |view, cx| {
            view.selected_pr = Some(GitHubItemKey {
                project: PathBuf::from("/projects/other"),
                number: 42,
            });
            view.pull_requests.push(ScopedPr {
                project: PathBuf::from("/projects/other"),
                project_name: "other".into(),
                summary: GitHubPullRequestSummary {
                    number: 42,
                    url: "https://github.com/threadlane/other/pull/42".into(),
                    ..Default::default()
                },
            });
            view.refresh_pr_viewed(cx);
        });
        read_resp
            .send(Ok(viewed_fixture(&[("src/lib.rs", PrFileViewedStatus::Viewed)])))
            .unwrap();
        cx.run_until_parked();
        read_req.try_recv().unwrap();
        assert!(read_req.try_recv().is_err());
        view.read_with(cx, |view, _| {
            assert!(view
                .pr_viewed
                .get(&key)
                .is_some_and(|state| state.snapshot.is_some()));
            let other = PrWorkspaceKey {
                project: PathBuf::from("/projects/other"),
                number: 42,
            };
            assert!(view.pr_viewed.get(&other).is_none());
        });
    }
}
