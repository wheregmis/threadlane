use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::link::Link;
use gpui_component::resizable::{h_resizable, resizable_panel, ResizableState};
use gpui_component::scroll::{ScrollableElement, Scrollbar};
use gpui_component::spinner::Spinner;
use gpui_component::status_bar::StatusBar;
use gpui_component::tag::Tag;
use gpui_component::text::{TextView, TextViewState};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt};
use threadlane_git::{
    GitHubIssueDetail, GitHubIssueRef, GitHubIssueSummary, GitHubPrInfo, GitHubPullRequestSummary,
    GitHubRepository,
};

use crate::app::actions::AppAction;
use crate::app::controller;
use crate::state::{AppState, SessionInfo};

actions!(
    threadlane_github,
    [SelectPrevious, SelectNext, OpenSelected]
);

const GITHUB_LIST_CONTEXT: &str = "GitHubList";
const PAGE_SIZE: usize = 50;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, Some(GITHUB_LIST_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(GITHUB_LIST_CONTEXT)),
        KeyBinding::new("enter", OpenSelected, Some(GITHUB_LIST_CONTEXT)),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitHubTab {
    Issues,
    PullRequests,
}

impl GitHubTab {
    fn label(self) -> &'static str {
        match self {
            Self::Issues => "Issues",
            Self::PullRequests => "Pull requests",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitHubStateFilter {
    Open,
    Closed,
}

impl GitHubStateFilter {
    fn value(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitHubRequest {
    pub(crate) work_dir: PathBuf,
    pub(crate) tab: GitHubTab,
    pub(crate) query_revision: u64,
    pub(crate) item_number: Option<u64>,
}

pub(crate) fn github_result_matches_request(
    result: &GitHubRequest,
    current: &GitHubRequest,
) -> bool {
    result == current
}

pub(crate) fn detail_result_matches_list(
    detail: &GitHubRequest,
    list: &GitHubRequest,
    selected: Option<u64>,
) -> bool {
    list.item_number.is_none()
        && detail.work_dir == list.work_dir
        && detail.tab == list.tab
        && detail.query_revision == list.query_revision
        && detail.item_number == selected
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitHubQueryMode {
    Local,
    Advanced,
}

pub(crate) fn github_query_mode(query: &str) -> GitHubQueryMode {
    const QUALIFIERS: &[&str] = &[
        "archived",
        "assignee",
        "author",
        "base",
        "closed",
        "comments",
        "created",
        "draft",
        "head",
        "interactions",
        "involves",
        "is",
        "label",
        "linked",
        "mentions",
        "milestone",
        "no",
        "org",
        "project",
        "reactions",
        "repo",
        "review",
        "review-requested",
        "reviewed-by",
        "sort",
        "state",
        "status",
        "team-review-requested",
        "type",
        "updated",
        "user",
    ];

    if query.split_whitespace().any(|token| {
        let token = token.trim_start_matches('-');
        let Some((key, value)) = token.split_once(':') else {
            return false;
        };
        !value.is_empty()
            && QUALIFIERS
                .iter()
                .any(|qualifier| key.eq_ignore_ascii_case(qualifier))
    }) {
        GitHubQueryMode::Advanced
    } else {
        GitHubQueryMode::Local
    }
}

fn github_server_query(query: &str) -> Option<&str> {
    (github_query_mode(query) == GitHubQueryMode::Advanced).then_some(query)
}

pub(crate) fn issue_filter_matches(issue: &GitHubIssueSummary, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || issue.title.to_lowercase().contains(&query)
        || issue.issue.number.to_string().contains(&query)
        || issue
            .labels
            .iter()
            .any(|label| label.name.to_lowercase().contains(&query))
        || issue
            .assignees
            .iter()
            .any(|assignee| assignee.to_lowercase().contains(&query))
}

pub(crate) fn selected_issue_after_refresh(
    selected: Option<u64>,
    issues: &[GitHubIssueSummary],
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
    } else if is_generating || session.health == crate::state::SessionHealth::Working {
        "Working"
    } else if session.health == crate::state::SessionHealth::Warning {
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

fn github_link_fingerprint(state: &AppState) -> u64 {
    let mut hasher = DefaultHasher::new();
    for project in &state.projects {
        project.name.hash(&mut hasher);
        project.work_dir.hash(&mut hasher);
        for session in &project.sessions {
            let Some(issue) = session.github_issue.as_ref() else {
                continue;
            };
            issue.host.hash(&mut hasher);
            issue.owner.hash(&mut hasher);
            issue.repo.hash(&mut hasher);
            issue.number.hash(&mut hasher);
            let pr = session.git_branch.as_ref().and_then(|branch| {
                state
                    .git_prs
                    .get(&(session.work_dir.clone(), branch.clone()))
                    .and_then(|pr| pr.as_ref())
            });
            linked_session_fingerprint(session, pr).hash(&mut hasher);
            state
                .pending_permissions
                .contains_key(&session.id)
                .hash(&mut hasher);
            state
                .session_is_generating(&session.session_file)
                .hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn linked_session_fingerprint(session: &SessionInfo, pr: Option<&GitHubPrInfo>) -> u64 {
    let mut hasher = DefaultHasher::new();
    session.id.hash(&mut hasher);
    session.title.hash(&mut hasher);
    session.health.hash(&mut hasher);
    session.worktree_available.hash(&mut hasher);
    session.is_worktree.hash(&mut hasher);
    session.git_branch.hash(&mut hasher);
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

fn selected_number_after_refresh<T>(
    selected: Option<u64>,
    rows: &[T],
    number: impl Fn(&T) -> u64,
) -> Option<u64> {
    selected
        .filter(|selected| rows.iter().any(|row| number(row) == *selected))
        .or_else(|| rows.first().map(number))
}

fn pr_filter_matches(pr: &GitHubPullRequestSummary, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || pr.title.to_lowercase().contains(&query)
        || pr.number.to_string().contains(&query)
        || pr.author.to_lowercase().contains(&query)
        || pr.head_ref.to_lowercase().contains(&query)
        || pr.base_ref.to_lowercase().contains(&query)
}

fn github_error_message(error: &str) -> String {
    let normalized = error.to_lowercase();
    if normalized.contains("auth") || normalized.contains("login") {
        "GitHub authentication is required. Sign in with gh and refresh.".into()
    } else if normalized.contains("remote") || normalized.contains("repository") {
        "This project does not have an accessible GitHub remote.".into()
    } else if normalized.contains("connect")
        || normalized.contains("network")
        || normalized.contains("resolve")
    {
        "GitHub is offline. Check your connection and refresh.".into()
    } else {
        format!("Couldn’t load GitHub: {error}")
    }
}

enum GitHubListResult {
    Issues(Result<Vec<GitHubIssueSummary>, String>),
    PullRequests(Result<Vec<GitHubPullRequestSummary>, String>),
}

enum GitHubDetailResult {
    Issue(Result<GitHubIssueDetail, String>),
    PullRequest(Result<GitHubPrInfo, String>),
}

struct LinkedSession {
    project_name: String,
    session: SessionInfo,
    status: &'static str,
    branch: Option<String>,
    pr_number: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IssueStartConfirmation {
    copy: String,
    model: String,
    reasoning_effort: String,
    branch_preview: String,
    branch_disclosure: String,
    start_enabled: bool,
    start_disabled_reason: Option<String>,
    show_open_task: bool,
    start_label: &'static str,
}

fn issue_start_confirmation(
    issue: &GitHubIssueRef,
    title: &str,
    model: &str,
    reasoning_effort: &str,
    is_git_repository: bool,
    has_linked_task: bool,
) -> IssueStartConfirmation {
    IssueStartConfirmation {
        copy: "Local Threadlane task".into(),
        model: model.into(),
        reasoning_effort: reasoning_effort.into(),
        branch_preview: AppState::issue_branch_name(issue.number, title, "xxxxxx"),
        branch_disclosure: "A unique six-character suffix is assigned when the task starts.".into(),
        start_enabled: is_git_repository,
        start_disabled_reason: (!is_git_repository)
            .then_some("This project is not a Git repository.".into()),
        show_open_task: has_linked_task,
        start_label: if has_linked_task {
            "Start another"
        } else {
            "Start task"
        },
    }
}

fn issue_start_activation(
    start_enabled: bool,
    start: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    if !start_enabled {
        return Ok(false);
    }
    start().map(|()| true)
}

struct IssueStartDialog {
    model: Entity<AppState>,
    work_dir: PathBuf,
    issue: GitHubIssueRef,
    title: String,
    confirmation: IssueStartConfirmation,
    error: Option<String>,
}

impl IssueStartDialog {
    fn start(&mut self, cx: &mut Context<Self>) -> bool {
        let result = issue_start_activation(self.confirmation.start_enabled, || {
            self.model.update(cx, |state, cx| {
                let result = state.start_issue_work(
                    self.work_dir.clone(),
                    self.issue.clone(),
                    self.title.clone(),
                );
                if let Err(error) = &result {
                    state.session_status = Some(error.clone());
                }
                cx.notify();
                result.map(|_| ())
            })
        });
        match result {
            Ok(_) => true,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                false
            }
        }
    }
}

fn activate_issue_start_dialog(dialog: &Entity<IssueStartDialog>, cx: &mut App) -> bool {
    dialog.update(cx, |dialog, cx| dialog.start(cx))
}

impl Render for IssueStartDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let confirmation = &self.confirmation;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_sm()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(confirmation.copy.clone()),
            )
            .child(format!("Issue: #{} {}", self.issue.number, self.title))
            .child(format!("Model: {}", confirmation.model))
            .child(format!(
                "Reasoning effort: {}",
                confirmation.reasoning_effort
            ))
            .child(format!("Branch preview: {}", confirmation.branch_preview))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(confirmation.branch_disclosure.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p_2()
                    .bg(theme.secondary)
                    .rounded_md()
                    .child("Isolated worktree")
                    .child(Tag::new().small().child("Locked")),
            )
            .children(confirmation.start_disabled_reason.as_ref().map(|reason| {
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(reason.clone())
            }))
            .children(self.error.as_ref().map(|error| {
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_color(theme.danger)
                    .child(error.clone())
                    .child(
                        Button::new("retry-issue-task")
                            .label("Retry")
                            .small()
                            .on_click(cx.listener(|this, _event, window, cx| {
                                if this.start(cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    )
            }))
    }
}

fn open_issue_start_dialog(
    model: Entity<AppState>,
    work_dir: PathBuf,
    issue: GitHubIssueRef,
    title: String,
    has_linked_task: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let (selected_model, reasoning_effort) = {
        let state = model.read(cx);
        (state.selected_model.clone(), state.reasoning_effort.label())
    };
    let confirmation = issue_start_confirmation(
        &issue,
        &title,
        &selected_model,
        reasoning_effort,
        threadlane_git::is_git_repo(&work_dir),
        has_linked_task,
    );
    let start_enabled = confirmation.start_enabled;
    let disabled_reason = confirmation.start_disabled_reason.clone();
    let dialog_state = cx.new(|_| IssueStartDialog {
        model,
        work_dir,
        issue,
        title,
        confirmation,
        error: None,
    });
    window.open_dialog(cx, move |dialog, _window, _cx| {
        let confirm_state = dialog_state.clone();
        let on_ok_state = dialog_state.clone();
        dialog
            .title("Start local task?")
            .child(dialog_state.clone())
            .footer(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-issue-task")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("confirm-issue-task")
                            .primary()
                            .label("Start task")
                            .disabled(!start_enabled)
                            .tooltip(disabled_reason.clone().unwrap_or_default())
                            .on_click(move |_, window, cx| {
                                if activate_issue_start_dialog(&confirm_state, cx) {
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
            )
            .on_ok(move |_, _, cx| activate_issue_start_dialog(&on_ok_state, cx))
    });
}

pub struct GitHubView {
    model: Entity<AppState>,
    project_work_dir: Option<PathBuf>,
    repository: Option<GitHubRepository>,
    tab: GitHubTab,
    state_filter: GitHubStateFilter,
    query_input: Entity<InputState>,
    query_revision: u64,
    issues: Vec<GitHubIssueSummary>,
    pull_requests: Vec<GitHubPullRequestSummary>,
    selected_issue: Option<u64>,
    selected_pr: Option<u64>,
    issue_detail: Option<GitHubIssueDetail>,
    pr_detail: Option<GitHubPrInfo>,
    detail_body: Entity<TextViewState>,
    comment_rows: Vec<(String, String, String)>,
    comment_list_state: ListState,
    list_loading: bool,
    detail_loading: bool,
    list_error: Option<String>,
    detail_error: Option<String>,
    issue_limit: usize,
    pr_limit: usize,
    issue_has_more: bool,
    pr_has_more: bool,
    active_list_request: Option<GitHubRequest>,
    active_detail_request: Option<GitHubRequest>,
    issue_list_state: ListState,
    pr_list_state: ListState,
    detail_split_state: Entity<ResizableState>,
    list_focus: FocusHandle,
    debounce_task: Option<Task<()>>,
    issue_comment_draft: String,
    pr_review_draft: String,
    linked_sessions_fingerprint: u64,
    _subscriptions: Vec<Subscription>,
}

impl GitHubView {
    pub(crate) fn new(
        model: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search issues…"));
        let detail_body = cx.new(|cx| TextViewState::markdown("", cx));
        let detail_split_state = cx.new(|_| ResizableState::default());
        let linked_sessions_fingerprint = github_link_fingerprint(model.read(cx));

        let model_subscription = cx.observe(&model, |this, model, cx| {
            let state = model.read(cx);
            let work_dir = state.active_work_dir.clone();
            let linked_sessions_fingerprint = github_link_fingerprint(state);
            if this.project_work_dir != work_dir {
                this.switch_project(work_dir, cx);
            }
            if this.linked_sessions_fingerprint != linked_sessions_fingerprint {
                this.linked_sessions_fingerprint = linked_sessions_fingerprint;
                cx.notify();
            }
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

        Self {
            model,
            project_work_dir: None,
            repository: None,
            tab: GitHubTab::Issues,
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
            comment_list_state: ListState::new(0, ListAlignment::Top, px(96.0)),
            list_loading: false,
            detail_loading: false,
            list_error: None,
            detail_error: None,
            issue_limit: PAGE_SIZE,
            pr_limit: PAGE_SIZE,
            issue_has_more: false,
            pr_has_more: false,
            active_list_request: None,
            active_detail_request: None,
            issue_list_state: ListState::new(0, ListAlignment::Top, px(88.0)),
            pr_list_state: ListState::new(0, ListAlignment::Top, px(78.0)),
            detail_split_state,
            list_focus: cx.focus_handle(),
            debounce_task: None,
            issue_comment_draft: String::new(),
            pr_review_draft: String::new(),
            linked_sessions_fingerprint,
            _subscriptions: vec![model_subscription, input_subscription],
        }
    }

    pub(crate) fn sync_active_project(&mut self, cx: &mut Context<Self>) {
        let work_dir = self.model.read(cx).active_work_dir.clone();
        if self.project_work_dir != work_dir {
            self.switch_project(work_dir, cx);
        }
    }

    fn switch_project(&mut self, work_dir: Option<PathBuf>, cx: &mut Context<Self>) {
        self.project_work_dir = work_dir;
        self.repository = None;
        self.issues.clear();
        self.pull_requests.clear();
        self.selected_issue = None;
        self.selected_pr = None;
        self.issue_detail = None;
        self.pr_detail = None;
        self.comment_rows.clear();
        self.comment_list_state.reset(0);
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
        self.issue_comment_draft.clear();
        self.pr_review_draft.clear();
        self.issue_list_state.reset(0);
        self.pr_list_state.reset(0);
        self.detail_body
            .update(cx, |body, cx| body.set_text("", cx));
        self.query_revision = self.query_revision.saturating_add(1);
        if self.project_work_dir.is_some() {
            self.fetch_list(cx);
        }
        cx.notify();
    }

    fn schedule_query(&mut self, _query: String, cx: &mut Context<Self>) {
        self.debounce_task.take();
        let revision = self.query_revision;
        self.debounce_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.query_revision == revision {
                    this.fetch_list(cx);
                }
            });
        }));
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
        self.detail_body
            .update(cx, |body, cx| body.set_text("", cx));
    }

    fn fetch_list(&mut self, cx: &mut Context<Self>) {
        let Some(work_dir) = self.project_work_dir.clone() else {
            return;
        };
        let tab = self.tab;
        let state = self.state_filter.value().to_owned();
        let query = self.query(cx);
        let limit = match tab {
            GitHubTab::Issues => self.issue_limit,
            GitHubTab::PullRequests => self.pr_limit,
        };
        let request = GitHubRequest {
            work_dir: work_dir.clone(),
            tab,
            query_revision: self.query_revision,
            item_number: None,
        };
        self.invalidate_detail(cx);
        self.active_list_request = Some(request.clone());
        self.list_loading = true;
        self.list_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let server_query = github_server_query(&query);
                    match tab {
                        GitHubTab::Issues => GitHubListResult::Issues(
                            threadlane_git::list_github_issues(
                                &work_dir,
                                &state,
                                server_query,
                                limit,
                            )
                            .map_err(|error| error.message),
                        ),
                        GitHubTab::PullRequests => GitHubListResult::PullRequests(
                            threadlane_git::list_github_pull_requests(
                                &work_dir,
                                &state,
                                server_query,
                                limit,
                            )
                            .map_err(|error| error.message),
                        ),
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
                    GitHubListResult::Issues(Ok(rows)) => {
                        let previous_selected = this.selected_issue;
                        let old_count = this.issues.len() + usize::from(this.issue_has_more);
                        this.issue_has_more = rows.len() == limit;
                        this.repository = rows.first().map(|row| GitHubRepository {
                            host: row.issue.host.clone(),
                            owner: row.issue.owner.clone(),
                            repo: row.issue.repo.clone(),
                        });
                        let query = this.query(cx);
                        let query_mode = github_query_mode(&query);
                        this.issues = rows
                            .into_iter()
                            .filter(|row| {
                                query_mode == GitHubQueryMode::Advanced
                                    || issue_filter_matches(row, &query)
                            })
                            .collect();
                        this.selected_issue =
                            selected_issue_after_refresh(this.selected_issue, &this.issues);
                        let new_count = this.issues.len() + usize::from(this.issue_has_more);
                        reconcile_list_count(&this.issue_list_state, old_count, new_count);
                        if previous_selected != this.selected_issue {
                            if let Some(ix) = this.selected_ix() {
                                this.issue_list_state.scroll_to_reveal_item(ix);
                            }
                        }
                        this.fetch_detail(cx);
                    }
                    GitHubListResult::PullRequests(Ok(rows)) => {
                        let previous_selected = this.selected_pr;
                        let old_count = this.pull_requests.len() + usize::from(this.pr_has_more);
                        this.pr_has_more = rows.len() == limit;
                        this.repository = rows.first().map(|row| row.repository.clone());
                        let query = this.query(cx);
                        let query_mode = github_query_mode(&query);
                        this.pull_requests = rows
                            .into_iter()
                            .filter(|row| {
                                query_mode == GitHubQueryMode::Advanced
                                    || pr_filter_matches(row, &query)
                            })
                            .collect();
                        this.selected_pr = selected_number_after_refresh(
                            this.selected_pr,
                            &this.pull_requests,
                            |row| row.number,
                        );
                        let new_count = this.pull_requests.len() + usize::from(this.pr_has_more);
                        reconcile_list_count(&this.pr_list_state, old_count, new_count);
                        if previous_selected != this.selected_pr {
                            if let Some(ix) = this.selected_ix() {
                                this.pr_list_state.scroll_to_reveal_item(ix);
                            }
                        }
                        this.fetch_detail(cx);
                    }
                    GitHubListResult::Issues(Err(error))
                    | GitHubListResult::PullRequests(Err(error)) => {
                        this.list_error = Some(github_error_message(&error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn fetch_detail(&mut self, cx: &mut Context<Self>) {
        let Some(work_dir) = self.project_work_dir.clone() else {
            return;
        };
        let number = match self.tab {
            GitHubTab::Issues => self.selected_issue,
            GitHubTab::PullRequests => self.selected_pr,
        };
        let Some(number) = number else {
            self.detail_loading = false;
            return;
        };
        let tab = self.tab;
        let request = GitHubRequest {
            work_dir: work_dir.clone(),
            tab,
            query_revision: self.query_revision,
            item_number: Some(number),
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
                            threadlane_git::inspect_github_issue(&work_dir, number)
                                .map_err(|error| error.message),
                        ),
                        GitHubTab::PullRequests => GitHubDetailResult::PullRequest(
                            threadlane_git::inspect_pr_number(&work_dir, number)
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
                    detail_result_matches_list(&request, list, this.selected_number())
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
                        this.comment_rows = detail
                            .issue_comments
                            .iter()
                            .map(|comment| {
                                (
                                    comment.author.clone(),
                                    comment.created_at.clone(),
                                    comment.body.clone(),
                                )
                            })
                            .chain(detail.reviews.iter().map(|review| {
                                (
                                    review.author.clone(),
                                    review.submitted_at.clone(),
                                    review.body.clone(),
                                )
                            }))
                            .collect();
                        this.comment_list_state.reset(this.comment_rows.len());
                        this.detail_body
                            .update(cx, |body, cx| body.set_text(&detail.body, cx));
                        this.pr_detail = Some(detail);
                    }
                    GitHubDetailResult::Issue(Err(error))
                    | GitHubDetailResult::PullRequest(Err(error)) => {
                        this.detail_error = Some(github_error_message(&error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_tab(&mut self, tab: GitHubTab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
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
        self.debounce_task.take();
        self.query_revision = self.query_revision.saturating_add(1);
        self.fetch_list(cx);
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        match self.tab {
            GitHubTab::Issues => self.issue_limit += PAGE_SIZE,
            GitHubTab::PullRequests => self.pr_limit += PAGE_SIZE,
        }
        self.refresh(cx);
    }

    fn selected_ix(&self) -> Option<usize> {
        match self.tab {
            GitHubTab::Issues => self.selected_issue.and_then(|number| {
                self.issues
                    .iter()
                    .position(|row| row.issue.number == number)
            }),
            GitHubTab::PullRequests => self.selected_pr.and_then(|number| {
                self.pull_requests
                    .iter()
                    .position(|row| row.number == number)
            }),
        }
    }

    fn selected_number(&self) -> Option<u64> {
        match self.tab {
            GitHubTab::Issues => self.selected_issue,
            GitHubTab::PullRequests => self.selected_pr,
        }
    }

    fn select_ix(&mut self, ix: usize, cx: &mut Context<Self>) {
        match self.tab {
            GitHubTab::Issues => {
                let Some(row) = self.issues.get(ix) else {
                    return;
                };
                self.selected_issue = Some(row.issue.number);
                self.issue_list_state.scroll_to_reveal_item(ix);
            }
            GitHubTab::PullRequests => {
                let Some(row) = self.pull_requests.get(ix) else {
                    return;
                };
                self.selected_pr = Some(row.number);
                self.pr_list_state.scroll_to_reveal_item(ix);
            }
        }
        self.fetch_detail(cx);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = match self.tab {
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

    fn open_selected(&mut self, _: &OpenSelected, _window: &mut Window, cx: &mut Context<Self>) {
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

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let close_model = self.model.clone();
        let repository = self
            .repository
            .as_ref()
            .map(|repo| format!("{}/{}", repo.owner, repo.repo))
            .or_else(|| {
                self.project_work_dir
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "No project".into());

        div()
            .flex_none()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .px_4()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .child(
                Button::new("github-close")
                    .icon(IconName::Close)
                    .tooltip("Back to chat")
                    .ghost()
                    .small()
                    .on_click(move |_, _, cx| {
                        close_model.update(cx, |state, cx| {
                            controller::dispatch(state, AppAction::CloseGitHub);
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("GitHub · {repository}")),
            )
            .child(
                Button::new("github-tab-issues")
                    .label("Issues")
                    .ghost()
                    .small()
                    .selected(self.tab == GitHubTab::Issues)
                    .on_click(cx.listener(|this, _, _, cx| this.select_tab(GitHubTab::Issues, cx))),
            )
            .child(
                Button::new("github-tab-prs")
                    .label("Pull requests")
                    .ghost()
                    .small()
                    .selected(self.tab == GitHubTab::PullRequests)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.select_tab(GitHubTab::PullRequests, cx)),
                    ),
            )
            .child(
                Button::new("github-refresh")
                    .icon(IconName::Redo)
                    .tooltip("Refresh GitHub")
                    .ghost()
                    .small()
                    .disabled(self.project_work_dir.is_none() || self.list_loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .into_any_element()
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .flex_none()
            .border_b_1()
            .border_color(theme.border)
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("github-state-open")
                    .label("Open")
                    .ghost()
                    .small()
                    .selected(self.state_filter == GitHubStateFilter::Open)
                    .on_click(
                        cx.listener(|this, _, _, cx| {
                            this.select_state(GitHubStateFilter::Open, cx)
                        }),
                    ),
            )
            .child(
                Button::new("github-state-closed")
                    .label("Closed")
                    .ghost()
                    .small()
                    .selected(self.state_filter == GitHubStateFilter::Closed)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_state(GitHubStateFilter::Closed, cx)
                    })),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(Input::new(&self.query_input).small()),
            )
            .children(self.list_loading.then(|| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().xsmall())
                    .child(if self.issues.is_empty() && self.pull_requests.is_empty() {
                        "Loading…"
                    } else {
                        "Refreshing…"
                    })
            }))
            .children(self.list_error.as_ref().map(|error| {
                div()
                    .max_w_64()
                    .text_xs()
                    .text_color(theme.danger)
                    .truncate()
                    .child(error.clone())
            }))
            .into_any_element()
    }

    fn render_issue_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        if ix == self.issues.len() && self.issue_has_more {
            return self.render_load_more(cx);
        }
        let Some(issue) = self.issues.get(ix).cloned() else {
            return div().into_any_element();
        };
        let selected = self.selected_issue == Some(issue.issue.number);
        let linked_count = self.linked_sessions(&issue.issue, cx).len();
        let number = issue.issue.number;
        let theme = cx.theme().colors;
        div()
            .id(SharedString::from(format!("github-issue-{number}")))
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .bg(if selected {
                theme.list_active
            } else {
                theme.background
            })
            .hover(|style| style.bg(theme.list_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.list_focus.focus(window, cx);
                if let Some(ix) = this
                    .issues
                    .iter()
                    .position(|row| row.issue.number == number)
                {
                    this.select_ix(ix, cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        Icon::new(if issue.state.eq_ignore_ascii_case("closed") {
                            IconName::CircleCheck
                        } else {
                            IconName::Asterisk
                        })
                        .small()
                        .text_color(
                            if issue.state.eq_ignore_ascii_case("closed") {
                                theme.muted_foreground
                            } else {
                                theme.success
                            },
                        ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(issue.title.clone()),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!(
                                        "#{number} · {} · {} · {} comments",
                                        issue.author, issue.updated_at, issue.comments_count
                                    ))
                                    .children(
                                        issue.labels.iter().take(3).map(|label| {
                                            Tag::new().small().child(label.name.clone())
                                        }),
                                    )
                                    .children((linked_count > 0).then(|| {
                                        Tag::info().small().child(format!(
                                            "{linked_count} linked task{}",
                                            if linked_count == 1 { "" } else { "s" }
                                        ))
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_pr_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        if ix == self.pull_requests.len() && self.pr_has_more {
            return self.render_load_more(cx);
        }
        let Some(pr) = self.pull_requests.get(ix).cloned() else {
            return div().into_any_element();
        };
        let selected = self.selected_pr == Some(pr.number);
        let number = pr.number;
        let theme = cx.theme().colors;
        div()
            .id(SharedString::from(format!("github-pr-{number}")))
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .bg(if selected {
                theme.list_active
            } else {
                theme.background
            })
            .hover(|style| style.bg(theme.list_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.list_focus.focus(window, cx);
                if let Some(ix) = this
                    .pull_requests
                    .iter()
                    .position(|row| row.number == number)
                {
                    this.select_ix(ix, cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        Icon::new(if pr.state.eq_ignore_ascii_case("closed") {
                            IconName::CircleCheck
                        } else {
                            IconName::Github
                        })
                        .small()
                        .text_color(
                            if pr.state.eq_ignore_ascii_case("closed") {
                                theme.muted_foreground
                            } else {
                                theme.success
                            },
                        ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(pr.title),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!(
                                        "#{number} · {} · {} · {} → {}{}",
                                        pr.author,
                                        pr.updated_at,
                                        pr.head_ref,
                                        pr.base_ref,
                                        if pr.is_draft { " · Draft" } else { "" }
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_load_more(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                Button::new("github-load-more")
                    .label("Load more")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
            )
            .into_any_element()
    }

    fn render_list(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        if self.project_work_dir.is_none() {
            return self.render_empty("Select a project to browse GitHub.", cx);
        }
        let row_count = match self.tab {
            GitHubTab::Issues => self.issues.len(),
            GitHubTab::PullRequests => self.pull_requests.len(),
        };
        if row_count == 0 && !self.list_loading {
            if let Some(error) = self.list_error.clone() {
                return self.render_empty(&error, cx);
            }
            return self.render_empty(
                &format!(
                    "No {} {}.",
                    self.state_filter.value(),
                    self.tab.label().to_lowercase()
                ),
                cx,
            );
        }
        let list_state = match self.tab {
            GitHubTab::Issues => self.issue_list_state.clone(),
            GitHubTab::PullRequests => self.pr_list_state.clone(),
        };
        let tab = self.tab;
        div()
            .relative()
            .size_full()
            .min_h_0()
            .border_1()
            .border_color(if self.list_focus.is_focused(window) {
                theme.primary
            } else {
                theme.border
            })
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
            .into_any_element()
    }

    fn render_empty(&self, message: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .text_sm()
            .text_color(theme.muted_foreground)
            .children(self.list_loading.then(|| Spinner::new().small()))
            .child(message.to_owned())
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
        let theme = cx.theme().colors;
        div()
            .p_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("{author} · {time}")),
            )
            .child(div().mt_2().text_sm().whitespace_normal().child(body))
            .into_any_element()
    }

    fn render_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        if self.selected_ix().is_none() {
            return self.render_empty("Select an item to see details.", cx);
        }
        if let Some(error) = &self.detail_error {
            return self.render_empty(error, cx);
        }

        let (title, metadata, url, issue) = match self.tab {
            GitHubTab::Issues => {
                let Some(detail) = self
                    .issue_detail
                    .as_ref()
                    .filter(|detail| self.selected_issue == Some(detail.summary.issue.number))
                else {
                    return self.render_empty("Loading details…", cx);
                };
                (
                    detail.summary.title.clone(),
                    format!(
                        "#{} · {} · {} · {}",
                        detail.summary.issue.number,
                        detail.summary.state,
                        detail.summary.author,
                        detail.summary.updated_at
                    ),
                    detail.summary.issue.url.clone(),
                    Some(detail.summary.issue.clone()),
                )
            }
            GitHubTab::PullRequests => {
                let Some(detail) = self
                    .pr_detail
                    .as_ref()
                    .filter(|detail| self.selected_pr == Some(detail.number))
                else {
                    return self.render_empty("Loading details…", cx);
                };
                (
                    detail.title.clone(),
                    format!(
                        "#{} · {} · {} · {} → {}",
                        detail.number,
                        detail.state,
                        detail.author,
                        detail.head_ref,
                        detail.base_ref
                    ),
                    detail.url.clone(),
                    None,
                )
            }
        };
        let linked_sessions = issue
            .as_ref()
            .map(|issue| self.linked_sessions(issue, cx))
            .unwrap_or_default();
        let start_model = self.model.clone();
        let start_work_dir = self.project_work_dir.clone();
        let start_issue = issue.clone();
        let start_title = title.clone();
        let start_has_linked_task = !linked_sessions.is_empty();

        div()
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .px_5()
            .py_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(metadata),
            )
            .child(
                div()
                    .mt_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Link::new("github-open-browser").href(url).child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(Icon::new(IconName::ExternalLink).small())
                                .child("Open on GitHub"),
                        ),
                    )
                    .children(start_issue.map(|issue| {
                        Button::new("github-start-agent-task")
                            .icon(IconName::Play)
                            .label(if start_has_linked_task {
                                "Start another"
                            } else {
                                "Start task"
                            })
                            .small()
                            .on_click(move |_, window, cx| {
                                let Some(work_dir) = start_work_dir.clone() else {
                                    return;
                                };
                                open_issue_start_dialog(
                                    start_model.clone(),
                                    work_dir,
                                    issue.clone(),
                                    start_title.clone(),
                                    start_has_linked_task,
                                    window,
                                    cx,
                                );
                            })
                    })),
            )
            .child(
                div()
                    .mt_5()
                    .text_sm()
                    .child(TextView::new(&self.detail_body).selectable(true)),
            )
            .children((!linked_sessions.is_empty()).then(|| {
                div()
                    .mt_5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Linked tasks"),
                    )
                    .children(linked_sessions.into_iter().map(|linked| {
                        let open_model = self.model.clone();
                        let open_work_dir = linked.session.work_dir.clone();
                        let open_session_id = linked.session.id.clone();
                        div()
                            .mt_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .child(linked.session.title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(linked.project_name),
                            )
                            .child(Tag::new().small().child(linked.status))
                            .children(
                                linked
                                    .session
                                    .is_worktree
                                    .then(|| Tag::new().small().child("Worktree")),
                            )
                            .children(linked.branch.map(|branch| Tag::new().small().child(branch)))
                            .children(
                                linked.pr_number.map(|number| {
                                    Tag::new().small().child(format!("PR #{number}"))
                                }),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "open-linked-task-{}",
                                    open_session_id
                                )))
                                .label("Open task")
                                .ghost()
                                .xsmall()
                                .on_click(move |_, _, cx| {
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
                                }),
                            )
                            .into_any_element()
                    }))
            }))
            .children((!self.comment_rows.is_empty()).then(|| {
                div()
                    .mt_5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Conversation ({})", self.comment_rows.len())),
                    )
                    .child(
                        div()
                            .relative()
                            .mt_2()
                            .h_64()
                            .border_1()
                            .border_color(theme.border)
                            .rounded(cx.theme().radius)
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
                            ),
                    )
            }))
            .children(self.detail_loading.then(|| {
                div()
                    .mt_3()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().xsmall())
                    .child("Refreshing details…")
            }))
            .into_any_element()
    }

    fn render_status_bar(&self) -> impl IntoElement {
        let count = match self.tab {
            GitHubTab::Issues => self.issues.len(),
            GitHubTab::PullRequests => self.pull_requests.len(),
        };
        let has_draft = match self.tab {
            GitHubTab::Issues => !self.issue_comment_draft.is_empty(),
            GitHubTab::PullRequests => !self.pr_review_draft.is_empty(),
        };
        StatusBar::new().left(format!(
            "{} {} · {}{}",
            count,
            self.tab.label().to_lowercase(),
            self.state_filter.value(),
            if has_draft { " · Unsaved draft" } else { "" }
        ))
    }
}

impl Render for GitHubView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = window.bounds().size.width < px(900.0);
        let master = div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_filters(cx))
            .child(div().flex_1().min_h_0().child(self.render_list(window, cx)));
        let detail = self.render_detail(cx);
        let content = if narrow {
            div()
                .size_full()
                .min_h_0()
                .flex()
                .flex_col()
                .child(div().flex_1().min_h_0().child(master))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(detail),
                )
                .into_any_element()
        } else {
            h_resizable("github-master-detail")
                .with_state(&self.detail_split_state)
                .child(
                    resizable_panel()
                        .size(window.bounds().size.width * 0.35)
                        .size_range(px(260.0)..px(640.0))
                        .child(master),
                )
                .child(resizable_panel().child(detail))
                .into_any_element()
        };

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(content))
            .child(self.render_status_bar())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detail_result_matches_list, github_query_mode, github_result_matches_request,
        github_server_query, issue_filter_matches, issue_start_activation,
        issue_start_confirmation, linked_session_fingerprint, linked_session_ids,
        linked_session_status, linked_sessions_across_projects, list_count_splice,
        selected_issue_after_refresh, GitHubQueryMode, GitHubRequest, GitHubTab,
    };
    use crate::state::{SessionHealth, SessionInfo};
    use std::path::PathBuf;
    use threadlane_git::{GitHubIssueRef, GitHubIssueSummary, GitHubPrInfo};

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
        }
    }

    #[test]
    fn github_result_matches_request_rejects_stale_project_tab_query_revision() {
        let current = GitHubRequest {
            work_dir: PathBuf::from("/projects/current"),
            tab: GitHubTab::Issues,
            query_revision: 4,
            item_number: Some(12),
        };

        assert!(github_result_matches_request(&current, &current));
        for stale in [
            GitHubRequest {
                work_dir: PathBuf::from("/projects/old"),
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
                item_number: Some(13),
                ..current.clone()
            },
        ] {
            assert!(!github_result_matches_request(&stale, &current));
        }
    }

    #[test]
    fn stale_detail_is_rejected_after_a_new_list_request() {
        let old_detail = GitHubRequest {
            work_dir: PathBuf::from("/projects/current"),
            tab: GitHubTab::Issues,
            query_revision: 4,
            item_number: Some(42),
        };
        let new_list = GitHubRequest {
            query_revision: 5,
            item_number: None,
            ..old_detail.clone()
        };

        assert!(!detail_result_matches_list(
            &old_detail,
            &new_list,
            Some(42)
        ));
        assert!(!detail_result_matches_list(&old_detail, &new_list, None));
    }

    #[test]
    fn github_query_mode_keeps_plain_text_local_and_sends_qualifiers_remote() {
        for query in ["linked task", "42", "desktop maintainer"] {
            assert_eq!(github_query_mode(query), GitHubQueryMode::Local);
        }
        for query in ["label:desktop", "is:open linked", "-author:octocat"] {
            assert_eq!(github_query_mode(query), GitHubQueryMode::Advanced);
            assert_eq!(github_server_query(query), Some(query));
        }
        for query in ["https://github.com", "note:", ":value", "unknown:value"] {
            assert_eq!(github_query_mode(query), GitHubQueryMode::Local);
            assert_eq!(github_server_query(query), None);
        }
    }

    #[test]
    fn issue_filter_matches_title_number_label_and_assignee() {
        let issue = issue(42);

        for query in ["linked task", "42", "desktop", "maintainer"] {
            assert!(issue_filter_matches(&issue, query), "query: {query}");
        }
        assert!(!issue_filter_matches(&issue, "unrelated"));
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
        assert_eq!(confirmation.copy, "Local Threadlane task");
        assert_eq!(
            confirmation.branch_disclosure,
            "A unique six-character suffix is assigned when the task starts."
        );
        assert!(!confirmation.copy.to_lowercase().contains("assigned"));
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
        assert_eq!(confirmation.start_label, "Start another");
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
}
